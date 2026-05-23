/**
 * SWP server — newline-delimited JSON over TCP.
 *
 * Owns the handshake (server-hello → client-hello → history-snapshot) and
 * the per-client frame loop. Application state (characters, engine,
 * config) lives behind the `HandshakeProvider` callback so the transport
 * stays decoupled from Phase-N business logic.
 */

import type { Socket, TCPSocketListener } from "bun";

import type { CharacterInfo } from "../characters/registry.ts";
import { MAX_WIRE_MESSAGE_SIZE, SWP_V1 } from "./types.ts";
import type { ClientMessage, ServerHistory, ServerMessage } from "./types.ts";

interface SessionState {
  buf: Buffer;
  hello: boolean;
  /** Character this client is attached to (after handshake). */
  character: string | undefined;
}

/**
 * Per-connection callbacks. The hello snapshot is taken on every new
 * connection (so a re-registered character appears the next time a CLI
 * connects); the history snapshot is taken after the client tells us
 * which character it wants.
 *
 * Mirror of `backend/daemon/src/handshake.rs::HandshakeProvider`.
 */
export interface HandshakeProvider {
  helloSnapshot(): { characters: CharacterInfo[] };
  historySnapshot(selectedCharacter: string | undefined): Omit<ServerHistory, "type" | "rid">;
}

/**
 * Application-level handler called when a client sends a ClientMessage.
 * Returns a promise that resolves when the message has been persisted.
 * The SWP server is transport-only; the actual append / broadcast / LLM
 * call live behind this callback.
 */
export type MessageHandler = (
  session: { character: string | undefined },
  msg: { text: string; rid: string | undefined },
) => Promise<void>;

export interface SwpServerOptions {
  host: string;
  port: number;
  serverName: string;
  handshake: HandshakeProvider;
  /** Called when a client sends a ClientMessage. Optional — without it the server replies with an error. */
  onMessage?: MessageHandler;
  onClient?: (clientType: string, clientName: string, character: string | undefined) => void;
}

export class SwpServer {
  private server: TCPSocketListener<SessionState> | undefined;
  private clients = new Set<Socket<SessionState>>();

  constructor(private readonly opts: SwpServerOptions) {}

  start(): { host: string; port: number } {
    const server = Bun.listen<SessionState>({
      hostname: this.opts.host,
      port: this.opts.port,
      socket: {
        open: (sock) => this.onOpen(sock),
        data: (sock, chunk) => this.onData(sock, chunk),
        close: (sock) => this.onClose(sock),
        error: (sock, err) => this.onError(sock, err),
      },
    });
    this.server = server;
    return { host: server.hostname, port: server.port };
  }

  stop(): void {
    for (const sock of this.clients) sock.end();
    this.clients.clear();
    this.server?.stop(true);
    this.server = undefined;
  }

  // ── connection callbacks ────────────────────────────────────────

  private onOpen(sock: Socket<SessionState>): void {
    sock.data = { buf: Buffer.alloc(0), hello: false, character: undefined };
    this.clients.add(sock);

    const snapshot = this.opts.handshake.helloSnapshot();
    const hello: ServerMessage = {
      type: "hello",
      v: SWP_V1,
      server_name: this.opts.serverName,
      characters: snapshot.characters,
    };
    this.sendFrame(sock, hello);
  }

  private onData(sock: Socket<SessionState>, chunk: Buffer): void {
    sock.data.buf =
      sock.data.buf.length === 0 ? Buffer.from(chunk) : Buffer.concat([sock.data.buf, chunk]);

    if (sock.data.buf.length > MAX_WIRE_MESSAGE_SIZE) {
      this.sendFrame(sock, {
        type: "error",
        code: "protocol_error",
        message: `frame exceeded ${MAX_WIRE_MESSAGE_SIZE} bytes`,
      });
      sock.end();
      return;
    }

    while (true) {
      const nl = sock.data.buf.indexOf(0x0a);
      if (nl < 0) return;
      const line = sock.data.buf.subarray(0, nl).toString("utf8");
      sock.data.buf = sock.data.buf.subarray(nl + 1);
      if (line.trim() === "") continue;
      this.handleFrame(sock, line);
    }
  }

  private onClose(sock: Socket<SessionState>): void {
    this.clients.delete(sock);
  }

  private onError(sock: Socket<SessionState>, err: Error): void {
    console.error(`[swp] socket error: ${err.message}`);
    this.clients.delete(sock);
  }

  // ── frame dispatch ──────────────────────────────────────────────

  private handleFrame(sock: Socket<SessionState>, line: string): void {
    let msg: ClientMessage;
    try {
      msg = JSON.parse(line) as ClientMessage;
    } catch (e) {
      this.sendFrame(sock, {
        type: "error",
        code: "protocol_error",
        message: `frame is not valid JSON: ${(e as Error).message}`,
      });
      sock.end();
      return;
    }

    if (msg.type === "hello") {
      if (sock.data.hello) {
        this.sendFrame(sock, {
          type: "error",
          code: "protocol_error",
          message: "client sent Hello more than once",
        });
        sock.end();
        return;
      }
      sock.data.hello = true;

      const helloSnapshot = this.opts.handshake.helloSnapshot();
      const selected = resolveHandshakeCharacter(msg.character, helloSnapshot.characters);
      sock.data.character = selected;
      this.opts.onClient?.(msg.client_type, msg.client_name, selected);

      const historyBody = this.opts.handshake.historySnapshot(selected);
      const history: ServerMessage = { type: "history", ...historyBody };
      this.sendFrame(sock, history);
      return;
    }

    if (!sock.data.hello) {
      this.sendFrame(sock, {
        type: "error",
        code: "protocol_error",
        message: "client must send Hello before any other frame",
      });
      sock.end();
      return;
    }

    if (msg.type === "message") {
      if (this.opts.onMessage === undefined) {
        this.sendFrame(sock, {
          type: "error",
          code: "internal_error",
          message: "no message handler configured",
        });
        return;
      }
      // Fire and forget — the handler is responsible for broadcasting any
      // resulting state changes. We don't await here so a slow LLM call
      // (in later phases) doesn't block the read loop.
      this.opts.onMessage(
        { character: sock.data.character },
        { text: msg.text, rid: msg.rid },
      ).catch((e) => {
        const errMsg: ServerMessage = {
          type: "error",
          code: "internal_error",
          message: (e as Error).message,
        };
        const r = rid(msg);
        if (r !== undefined) errMsg.rid = r;
        this.sendFrame(sock, errMsg);
      });
      return;
    }

    const errMsg: ServerMessage = {
      type: "error",
      code: "internal_error",
      message: `shore-daemon-ts does not implement ${msg.type} yet (see REWRITE.md)`,
    };
    const r = rid(msg);
    if (r !== undefined) errMsg.rid = r;
    this.sendFrame(sock, errMsg);
  }

  /**
   * Send a frame to every connected client. Used for `History`
   * broadcasts after engine state changes.
   */
  broadcast(msg: ServerMessage): void {
    const line = JSON.stringify(msg) + "\n";
    for (const sock of this.clients) {
      sock.write(line);
    }
  }

  private sendFrame(sock: Socket<SessionState>, msg: ServerMessage): void {
    const line = JSON.stringify(msg) + "\n";
    sock.write(line);
  }
}

/**
 * Mirror of `swp-server::resolve_handshake_character`:
 *   - requested name that exists → that name
 *   - requested name that doesn't exist → none (warn)
 *   - no request + exactly one character → auto-select that one
 *   - no request + zero or >1 characters → none
 */
function resolveHandshakeCharacter(
  requested: string | undefined,
  characters: CharacterInfo[],
): string | undefined {
  if (requested !== undefined) {
    if (characters.some((c) => c.name === requested)) return requested;
    console.warn(`[swp] ignoring unknown connect-time character: ${requested}`);
    return undefined;
  }
  if (characters.length === 1) return characters[0]!.name;
  return undefined;
}

function rid(msg: ClientMessage): string | undefined {
  if ("rid" in msg && typeof msg.rid === "string") return msg.rid;
  return undefined;
}
