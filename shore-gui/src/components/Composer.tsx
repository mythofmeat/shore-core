import { useLayoutEffect, useRef, useState } from "react";

interface ComposerProps {
  connected: boolean;
  characterName: string;
  onSend: (text: string) => Promise<void> | void;
}

export function Composer({ connected, characterName, onSend }: ComposerProps) {
  const [text, setText] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Auto-grow the textarea to fit its content, capped by CSS max-height.
  useLayoutEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [text]);

  const canSend = connected && text.trim().length > 0;

  const handleSend = async () => {
    if (!canSend) return;
    await onSend(text.trim());
    setText("");
  };

  const placeholder = connected
    ? `Speak to ${characterName}...`
    : "Not connected";

  return (
    <footer className="composer">
      <div className="input-wrap">
        <div className="input-row">
          <span className="input-prompt">⟩</span>
          <textarea
            ref={textareaRef}
            rows={1}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void handleSend();
              }
            }}
            placeholder={placeholder}
            disabled={!connected}
          />
          <button
            className="send-btn"
            onClick={() => void handleSend()}
            disabled={!canSend}
            title="Send"
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <line x1="7" y1="17" x2="17" y2="7" />
              <polyline points="7 7 17 7 17 17" />
            </svg>
          </button>
        </div>
      </div>
    </footer>
  );
}
