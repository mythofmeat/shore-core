/**
 * SWP server — newline-delimited JSON over TCP.
 *
 * Phase 0 scope: accept connection, perform the 3-step handshake
 * (server-hello → client-hello → history-snapshot), then idle until the
 * client disconnects. No engine, no LLM, no real messages.
 */

import type { Socket, TCPSocketListener } from "bun";

import { MAX_WIRE_MESSAGE_SIZE, SWP_V1 } from "./types.ts";
import type { ClientMessage, ServerMessage } from "./types.ts";

interface SessionState {
  /** Accumulated bytes for the in-progress frame. */
  buf: Buffer;
  /** Has the client sent ClientHello yet? */
  hello: boolean;
}

export interface SwpServerOptions {
  /** Address to bind. `"0.0.0.0"` for any, `"127.0.0.1"` for loopback. */
  host: string;
  /** Port to bind. `0` to let the OS pick. */
  port: number;
  /** Server name advertised in ServerHello. */
  serverName: string;
  /** Called when a client successfully completes the handshake. */
  onClient?: (clientType: string, clientName: string) => void;
}

export class SwpServer {
  private server: TCPSocketListener<SessionState> | undefined;
  private clients = new Set<Socket<SessionState>>();

  constructor(private readonly opts: SwpServerOptions) {}

  /** Start listening. Returns the resolved listen address. */
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

  /** Stop accepting new connections and close all open sessions. */
  stop(): void {
    for (const sock of this.clients) {
      sock.end();
    }
    this.clients.clear();
    this.server?.stop(true);
    this.server = undefined;
  }

  // ── connection callbacks ────────────────────────────────────────

  private onOpen(sock: Socket<SessionState>): void {
    sock.data = { buf: Buffer.alloc(0), hello: false };
    this.clients.add(sock);

    // Step 1 of the handshake: server sends Hello first.
    const hello: ServerMessage = {
      type: "hello",
      v: SWP_V1,
      server_name: this.opts.serverName,
      // characters list is empty until Phase 2 wires up config + workspace
      // discovery. Empty is wire-valid.
      characters: [],
    };
    this.sendFrame(sock, hello);
  }

  private onData(sock: Socket<SessionState>, chunk: Buffer): void {
    sock.data.buf = sock.data.buf.length === 0 ? Buffer.from(chunk) : Buffer.concat([sock.data.buf, chunk]);

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
      const nl = sock.data.buf.indexOf(0x0a); // '\n'
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
      this.opts.onClient?.(msg.client_type, msg.client_name);

      // Step 3 of the handshake: send empty history snapshot.
      // Phase 0/1 has no engine. The `config` object mirrors what the Rust
      // daemon's `handshake.rs::history_config_snapshot` emits when no
      // character is selected — null active_model, private=false. Keeping
      // the field shape matched lets parity-traces diff cleanly.
      const history: ServerMessage = {
        type: "history",
        messages: [],
        config: { active_model: null, private: false },
        revision: 0,
      };
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

    // Phase 0: we accept frames but don't act on them. Echo a stub error
    // using one of the protocol's defined error codes so the CLI doesn't
    // reject our response as a deserialization failure.
    const errMsg: ServerMessage = {
      type: "error",
      code: "internal_error",
      message: `shore-daemon-ts is in Phase 0 of the rewrite (REWRITE.md) and does not implement ${msg.type} yet`,
    };
    const r = rid(msg);
    if (r !== undefined) errMsg.rid = r;
    this.sendFrame(sock, errMsg);
  }

  private sendFrame(sock: Socket<SessionState>, msg: ServerMessage): void {
    const line = JSON.stringify(msg) + "\n";
    sock.write(line);
  }
}

function rid(msg: ClientMessage): string | undefined {
  if ("rid" in msg && typeof msg.rid === "string") return msg.rid;
  return undefined;
}
