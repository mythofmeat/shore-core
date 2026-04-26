import { Fragment, useEffect, useMemo, useRef } from "react";
import { useDaemon } from "./hooks/useDaemon.ts";
import { useAssistantMessageNotifications } from "./hooks/useNotifications.ts";
import { Composer } from "./components/Composer.tsx";
import { Message } from "./components/Message.tsx";
import { TimeGap } from "./components/TimeGap.tsx";
import { deriveMessages, literaryDuration } from "./lib/messages.ts";

const DEFAULT_CHARACTER_NAME = "Shore";
// How close to the bottom (in px) counts as "following" — auto-scroll only
// chases new content when the user is within this band.
const STICK_TO_BOTTOM_PX = 120;

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

  // Auto-scroll to bottom. Uses a sentinel + scrollIntoView (more robust to
  // font/layout timing than setting scrollTop). Only chases new content when
  // the user is already near the bottom — if they've scrolled up to read
  // history, don't yank them back.
  const streamRef = useRef<HTMLElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const stickyRef = useRef(true);

  useEffect(() => {
    const el = streamRef.current;
    if (!el) return;
    const onScroll = () => {
      const distFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
      stickyRef.current = distFromBottom < STICK_TO_BOTTOM_PX;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  useEffect(() => {
    if (!stickyRef.current) return;
    const scroll = () => {
      bottomRef.current?.scrollIntoView({ block: "end" });
    };
    scroll();
    requestAnimationFrame(scroll);
    void document.fonts.ready.then(scroll);
  }, [messages, streaming]);

  return (
    <>
      <main className="stream" ref={streamRef}>
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
          {messages.map((m, i) => {
            const prev = messages[i - 1];
            const gap = gapBetween(prev, m);
            return (
              <Fragment key={m.msg_id}>
                {gap && <TimeGap label={gap} />}
                <Message message={m} characterName={characterName} />
              </Fragment>
            );
          })}
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

function gapBetween(
  prev: { timestamp: string; streaming?: boolean } | undefined,
  curr: { timestamp: string; streaming?: boolean },
): string | null {
  if (!prev || !prev.timestamp || !curr.timestamp) return null;
  // Don't inject gaps against a live streaming message — its timestamp is
  // synthetic (set when streaming starts) and would create a bogus gap.
  if (prev.streaming || curr.streaming) return null;
  const a = new Date(prev.timestamp).getTime();
  const b = new Date(curr.timestamp).getTime();
  if (!Number.isFinite(a) || !Number.isFinite(b) || b <= a) return null;
  return literaryDuration(b - a);
}
