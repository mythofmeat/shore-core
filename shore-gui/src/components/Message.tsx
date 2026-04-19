import { Sigil } from "./Sigil.tsx";
import { formatTimestamp, type DisplayMessage } from "../lib/messages.ts";

interface MessageProps {
  message: DisplayMessage;
  characterName: string;
}

export function Message({ message, characterName }: MessageProps) {
  const time = formatTimestamp(message.timestamp);

  if (message.role === "user") {
    return (
      <div className="msg user">
        {message.content}
        {time && <div className="msg-meta">{time}</div>}
      </div>
    );
  }

  if (message.role === "assistant") {
    return (
      <div className="msg char">
        <div className="name-line">
          <Sigil />
          <span className="name">{characterName}</span>
        </div>
        <div className="body">{message.content}</div>
        {time && <div className="msg-meta">{time}</div>}
      </div>
    );
  }

  // system / other — render as a quiet dim line for now
  return (
    <div className="msg user" style={{ opacity: 0.6, fontStyle: "italic" }}>
      {message.content}
    </div>
  );
}

export function StreamingIndicator({ characterName }: { characterName: string }) {
  return (
    <div className="msg char">
      <div className="name-line">
        <Sigil streaming />
        <span className="name">{characterName}</span>
      </div>
      <div className="body">
        <span className="ember-cursor" />
      </div>
    </div>
  );
}
