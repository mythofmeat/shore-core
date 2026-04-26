import type { EventItem } from "../hooks/useDaemon.ts";

export interface DisplayMessage {
  msg_id: string;
  role: "user" | "assistant" | "system";
  content: string;
  timestamp: string;
  streaming?: boolean;
}

interface ProtoMessage {
  msg_id?: string;
  role?: string;
  content?: string;
  timestamp?: string;
}

const STREAMING_ID = "__streaming__";

// Walk the event log and derive a displayable message list. Baseline comes
// from History events (initial connect or subsequent full-history refresh);
// live stream_chunk text is accumulated on top as a pending assistant entry.
//
// PR 2: text content_blocks only. Thinking / tool_use / images land in PR 3.
export function deriveMessages(events: EventItem[]): DisplayMessage[] {
  // 1. Baseline — history-source events (from initial connect) OR the
  //    most recent `type: "history"` stream event (which replaces).
  let baseline: ProtoMessage[] = [];
  let lastHistoryRefreshIndex = -1;

  for (let i = 0; i < events.length; i++) {
    const e = events[i];
    if (e.source === "history" && lastHistoryRefreshIndex < 0) {
      baseline.push(e.message as unknown as ProtoMessage);
      continue;
    }
    if (e.source === "stream") {
      const msg = e.message as Record<string, unknown>;
      if (msg.type === "history" && Array.isArray(msg.messages)) {
        baseline = msg.messages as ProtoMessage[];
        lastHistoryRefreshIndex = i;
      }
    }
  }

  // 2. Walk stream events AFTER the last history-refresh (or all events if
  //    none), accumulating text from stream_chunk events. A turn may split
  //    across multiple (StreamStart → chunks → StreamEnd) phases with
  //    finish_reason == "tool_use" between them — keep accumulating until we
  //    see a terminal StreamEnd.
  let streamText = "";
  let inStream = false;

  for (let i = lastHistoryRefreshIndex + 1; i < events.length; i++) {
    const e = events[i];
    if (e.source !== "stream") continue;
    const msg = e.message as Record<string, unknown>;
    switch (msg.type) {
      case "stream_start":
        // Only clear accumulated text when we weren't already mid-turn.
        if (!inStream) streamText = "";
        inStream = true;
        break;
      case "stream_chunk":
        if (inStream && msg.content_type === "text" && typeof msg.text === "string") {
          streamText += msg.text;
        }
        break;
      case "stream_end":
        if (msg.finish_reason !== "tool_use") {
          inStream = false;
          streamText = "";
        }
        break;
      case "error":
        inStream = false;
        streamText = "";
        break;
    }
  }

  // 3. Materialize baseline messages.
  const messages: DisplayMessage[] = baseline
    .filter((m): m is ProtoMessage & { msg_id: string; role: string } =>
      typeof m.msg_id === "string" && typeof m.role === "string",
    )
    .map((m) => ({
      msg_id: m.msg_id,
      role:
        m.role === "user" || m.role === "assistant"
          ? (m.role as "user" | "assistant")
          : "system",
      content: typeof m.content === "string" ? m.content : "",
      timestamp: typeof m.timestamp === "string" ? m.timestamp : "",
    }));

  // 4. Append pending streaming message if we're mid-turn.
  if (inStream) {
    messages.push({
      msg_id: STREAMING_ID,
      role: "assistant",
      content: streamText,
      timestamp: new Date().toISOString(),
      streaming: true,
    });
  }

  console.log("[shore-gui] derive", {
    eventsLen: events.length,
    baselineLen: baseline.length,
    inStream,
    streamTextLen: streamText.length,
    messagesLen: messages.length,
    lastMessageRole: messages[messages.length - 1]?.role,
    lastMessageStreaming: messages[messages.length - 1]?.streaming,
  });

  return messages;
}

export function formatTimestamp(iso: string): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

const WORDS = [
  "zero", "one", "two", "three", "four", "five", "six", "seven",
  "eight", "nine", "ten", "eleven", "twelve",
];

function englishNumber(n: number): string {
  return WORDS[n] ?? String(n);
}

// Literary phrasing for the gap between two messages. Returns null when the
// gap isn't long enough to call out (under an hour).
export function literaryDuration(ms: number): string | null {
  const HOUR = 60 * 60 * 1000;
  if (ms < HOUR) return null;

  const hours = ms / HOUR;
  const days = hours / 24;

  if (hours < 1.75) return "an hour passes";
  if (hours < 12) return `${englishNumber(Math.round(hours))} hours pass`;
  if (hours < 20) return "most of a day passes";
  if (days < 1.75) return "a day passes";
  if (days < 7) return `${englishNumber(Math.round(days))} days pass`;
  if (days < 11) return "a week passes";
  if (days < 28) return `${englishNumber(Math.round(days / 7))} weeks pass`;
  if (days < 55) return "a month passes";
  if (days < 330) return `${englishNumber(Math.round(days / 30))} months pass`;
  return "a year passes";
}
