import type { EventItem } from "../hooks/useDaemon.ts";

export interface DisplayMessage {
  msg_id: string;
  role: "user" | "assistant" | "system";
  content: string;
  timestamp: string;
}

interface ProtoMessage {
  msg_id?: string;
  role?: string;
  content?: string;
  timestamp?: string;
}

// PR 1: minimal derivation — use the convenience `content` string from each
// Message. content_blocks (thinking / tool_use / images) are unpacked in PR 3.
// History events from the initial connect define the baseline; a later History
// stream event replaces it.
export function deriveMessages(events: EventItem[]): DisplayMessage[] {
  let baseline: ProtoMessage[] = [];
  let seenHistoryRefresh = false;

  for (const e of events) {
    if (e.source === "history" && !seenHistoryRefresh) {
      baseline.push(e.message as unknown as ProtoMessage);
      continue;
    }
    const msg = e.message as Record<string, unknown>;
    if (msg.type === "history" && Array.isArray(msg.messages)) {
      baseline = msg.messages as ProtoMessage[];
      seenHistoryRefresh = true;
    }
  }

  return baseline
    .filter((m): m is ProtoMessage & { msg_id: string; role: string } =>
      typeof m.msg_id === "string" && typeof m.role === "string",
    )
    .map((m) => ({
      msg_id: m.msg_id,
      role: (m.role === "user" || m.role === "assistant" ? m.role : "system"),
      content: typeof m.content === "string" ? m.content : "",
      timestamp: typeof m.timestamp === "string" ? m.timestamp : "",
    }));
}

export function formatTimestamp(iso: string): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}
