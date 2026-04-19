import { useEffect, useState } from "react";
import { useDaemon, type ConnectionStatus } from "./hooks/useDaemon.ts";

function statusLabel(status: ConnectionStatus | null): string {
  if (!status) return "idle";
  if (status.kind === "connected") return `connected — ${status.server_name}`;
  return `disconnected — ${status.reason}`;
}

export default function App() {
  const { status, events, lastAddr, streaming, connect, disconnect, cancel, send } =
    useDaemon();
  const [input, setInput] = useState("");
  const [addr, setAddr] = useState(lastAddr);

  const connected = status?.kind === "connected";

  const handleSend = async () => {
    const text = input.trim();
    if (!text || !connected) return;
    await send(text);
    setInput("");
  };

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && streaming) {
        e.preventDefault();
        void cancel();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [streaming, cancel]);

  return (
    <main className="min-h-screen flex flex-col">
      <header className="px-4 py-3 border-b border-neutral-800 flex items-center gap-3">
        <h1 className="font-semibold tracking-tight">Shore</h1>
        <span className="text-xs text-neutral-500">{statusLabel(status)}</span>
        <div className="ml-auto flex items-center gap-2">
          <input
            className="bg-neutral-900 border border-neutral-800 rounded px-2 py-1 text-xs w-48"
            placeholder="host:port (optional)"
            value={addr}
            onChange={(e) => setAddr(e.target.value)}
            disabled={connected}
          />
          {streaming && (
            <button
              onClick={cancel}
              className="text-xs px-3 py-1 bg-red-600 rounded hover:bg-red-500"
              title="Esc"
            >
              Cancel
            </button>
          )}
          {connected ? (
            <button
              onClick={disconnect}
              className="text-xs px-3 py-1 bg-neutral-800 rounded hover:bg-neutral-700"
            >
              Disconnect
            </button>
          ) : (
            <button
              onClick={() => connect(addr || undefined)}
              className="text-xs px-3 py-1 bg-blue-600 rounded hover:bg-blue-500"
            >
              Connect
            </button>
          )}
        </div>
      </header>

      <div className="flex-1 overflow-auto px-4 py-3 space-y-1 font-mono text-xs">
        {events.length === 0 && (
          <p className="text-neutral-600 italic">no events yet — connect and send a message.</p>
        )}
        {events.map((e, i) => (
          <pre
            key={i}
            className={
              "border rounded p-2 whitespace-pre-wrap break-all " +
              (e.source === "history"
                ? "border-amber-900/40 text-amber-200/70"
                : "border-neutral-900 text-neutral-400")
            }
          >
            <span className="text-[10px] uppercase tracking-wider opacity-60">
              {e.source}
            </span>
            {"\n"}
            {JSON.stringify(e.message, null, 2)}
          </pre>
        ))}
      </div>

      <footer className="p-3 border-t border-neutral-800 flex gap-2">
        <input
          className="flex-1 bg-neutral-900 border border-neutral-800 rounded px-3 py-2 text-sm disabled:opacity-40"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void handleSend();
            }
          }}
          placeholder={connected ? "Type a message…" : "Connect first"}
          disabled={!connected}
        />
        <button
          onClick={handleSend}
          disabled={!connected || !input.trim()}
          className="px-4 py-2 text-sm bg-blue-600 rounded hover:bg-blue-500 disabled:bg-neutral-800 disabled:cursor-not-allowed"
        >
          Send
        </button>
      </footer>
    </main>
  );
}
