import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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

export interface DaemonHandle {
  status: ConnectionStatus | null;
  events: ServerMessageEvent[];
  connect: (addr?: string, character?: string) => Promise<void>;
  disconnect: () => Promise<void>;
  send: (text: string) => Promise<void>;
}

export function useDaemon(): DaemonHandle {
  const [status, setStatus] = useState<ConnectionStatus | null>(null);
  const [events, setEvents] = useState<ServerMessageEvent[]>([]);

  useEffect(() => {
    let unlistenStatus: UnlistenFn | undefined;
    let unlistenMsg: UnlistenFn | undefined;

    (async () => {
      unlistenStatus = await listen<ConnectionStatus>("connection-status", (e) => {
        setStatus(e.payload);
      });
      unlistenMsg = await listen<ServerMessageEvent>("server-message", (e) => {
        setEvents((prev) => [...prev, e.payload]);
      });
    })();

    return () => {
      unlistenStatus?.();
      unlistenMsg?.();
    };
  }, []);

  const connect = useCallback(async (addr?: string, character?: string) => {
    await invoke("connect", { addr: addr ?? null, character: character ?? null });
  }, []);

  const disconnect = useCallback(async () => {
    await invoke("disconnect");
  }, []);

  const send = useCallback(async (text: string) => {
    await invoke("send_message", { text });
  }, []);

  return { status, events, connect, disconnect, send };
}
