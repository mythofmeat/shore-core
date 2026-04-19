import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const ADDR_KEY = "shore-gui:last-addr";

export type ConnectionStatus =
  | {
      kind: "connected";
      server_name: string;
      characters: CharacterInfo[];
      selected_character: string | null;
      history: unknown[];
      config: unknown;
    }
  | { kind: "disconnected"; reason: string };

export interface CharacterInfo {
  name: string;
}

export interface ServerMessageEvent {
  type: string;
  [key: string]: unknown;
}

export interface HistoryMessage {
  msg_id: string;
  role: string;
  content: string;
  timestamp: string;
  [key: string]: unknown;
}

export type EventItem =
  | { source: "history"; message: HistoryMessage }
  | { source: "stream"; message: ServerMessageEvent };

export interface DaemonHandle {
  status: ConnectionStatus | null;
  events: EventItem[];
  lastAddr: string;
  streaming: boolean;
  lastStreamEnd: ServerMessageEvent | null;
  connect: (addr?: string, character?: string) => Promise<void>;
  disconnect: () => Promise<void>;
  cancel: () => Promise<void>;
  send: (text: string) => Promise<void>;
}

function readStoredAddr(): string {
  try {
    return localStorage.getItem(ADDR_KEY) ?? "";
  } catch {
    return "";
  }
}

export function useDaemon(): DaemonHandle {
  const [status, setStatus] = useState<ConnectionStatus | null>(null);
  const [events, setEvents] = useState<EventItem[]>([]);
  const [streaming, setStreaming] = useState(false);
  const [lastStreamEnd, setLastStreamEnd] = useState<ServerMessageEvent | null>(null);
  const lastAddrRef = useRef<string | null>(null);

  useEffect(() => {
    let unlistenStatus: UnlistenFn | undefined;
    let unlistenMsg: UnlistenFn | undefined;

    (async () => {
      unlistenStatus = await listen<ConnectionStatus>("connection-status", (e) => {
        setStatus(e.payload);
        if (e.payload.kind === "connected") {
          const history = e.payload.history as HistoryMessage[];
          setEvents(history.map((message) => ({ source: "history", message })));
          try {
            localStorage.setItem(ADDR_KEY, lastAddrRef.current ?? "");
          } catch {
            // storage unavailable — silently skip persistence
          }
        }
      });
      unlistenMsg = await listen<ServerMessageEvent>("server-message", (e) => {
        const msg = e.payload;
        if (msg.type === "stream_start") setStreaming(true);
        else if (msg.type === "stream_end" || msg.type === "error") setStreaming(false);
        if (msg.type === "stream_end") setLastStreamEnd(msg);
        setEvents((prev) => [...prev, { source: "stream", message: msg }]);
      });

      const stored = readStoredAddr();
      const addr = stored.length > 0 ? stored : null;
      lastAddrRef.current = addr;
      try {
        await invoke("connect", { addr, character: null });
      } catch (err) {
        console.error("auto-connect failed", err);
      }
    })();

    return () => {
      unlistenStatus?.();
      unlistenMsg?.();
    };
  }, []);

  const connect = useCallback(async (addr?: string, character?: string) => {
    const normalized = addr && addr.length > 0 ? addr : null;
    lastAddrRef.current = normalized;
    await invoke("connect", { addr: normalized, character: character ?? null });
  }, []);

  const disconnect = useCallback(async () => {
    setStreaming(false);
    await invoke("disconnect");
  }, []);

  const cancel = useCallback(async () => {
    await invoke("cancel");
  }, []);

  const send = useCallback(async (text: string) => {
    await invoke("send_message", { text });
  }, []);

  return {
    status,
    events,
    lastAddr: readStoredAddr(),
    streaming,
    lastStreamEnd,
    connect,
    disconnect,
    cancel,
    send,
  };
}
