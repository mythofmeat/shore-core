import { useEffect, useMemo, useRef } from "react";
import { useDaemon } from "./hooks/useDaemon.ts";
import { useAssistantMessageNotifications } from "./hooks/useNotifications.ts";
import { Composer } from "./components/Composer.tsx";
import { Message, StreamingIndicator } from "./components/Message.tsx";
import { deriveMessages } from "./lib/messages.ts";

const DEFAULT_CHARACTER_NAME = "Shore";

export default function App() {
  const daemon = useDaemon();
  const { status, events, streaming, lastStreamEnd, connect, cancel, send } =
    daemon;

  const characterName =
    status?.kind === "connected" && status.selected_character
      ? status.selected_character
      : DEFAULT_CHARACTER_NAME;

  useAssistantMessageNotifications(lastStreamEnd, characterName);

  const messages = useMemo(() => deriveMessages(events), [events]);
  const connected = status?.kind === "connected";

  // Esc cancels an in-flight stream
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

  // Auto-scroll to bottom via a sentinel at the end of the message list.
  // scrollIntoView handles layout/font-loading timing more reliably than
  // setting scrollTop manually.
  const bottomRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const scroll = () => {
      bottomRef.current?.scrollIntoView({ block: "end" });
    };
    scroll();
    requestAnimationFrame(scroll);
    void document.fonts.ready.then(scroll);
  }, [messages.length, streaming]);

  return (
    <>
      <main className="stream">
        <div className="stream-inner">
          {!connected && (
            <div className="msg user" style={{ textAlign: "center", padding: 0 }}>
              not connected —{" "}
              <button
                onClick={() => void connect()}
                style={{
                  background: "none",
                  border: "none",
                  color: "var(--ember)",
                  cursor: "pointer",
                  font: "inherit",
                  padding: 0,
                  textDecoration: "underline",
                }}
              >
                retry
              </button>
            </div>
          )}
          {messages.map((m) => (
            <Message key={m.msg_id} message={m} characterName={characterName} />
          ))}
          {streaming && <StreamingIndicator characterName={characterName} />}
          <div ref={bottomRef} aria-hidden />
        </div>
        <div className="fog-bottom" />
      </main>

      <Composer
        connected={connected}
        characterName={characterName}
        onSend={send}
      />
    </>
  );
}
