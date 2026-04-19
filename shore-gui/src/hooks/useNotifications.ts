import { useEffect, useRef } from "react";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { ServerMessageEvent } from "./useDaemon.ts";

const PREVIEW_MAX = 140;

function preview(content: unknown): string {
  if (typeof content !== "string") return "";
  const firstLine = content.split("\n").find((l) => l.trim().length > 0) ?? content;
  return firstLine.length > PREVIEW_MAX
    ? firstLine.slice(0, PREVIEW_MAX - 1) + "…"
    : firstLine;
}

export function useAssistantMessageNotifications(
  lastStreamEnd: ServerMessageEvent | null,
  title: string,
): void {
  const grantedRef = useRef<boolean | null>(null);

  useEffect(() => {
    (async () => {
      let granted = await isPermissionGranted();
      if (!granted) {
        const res = await requestPermission();
        granted = res === "granted";
      }
      grantedRef.current = granted;
    })();
  }, []);

  useEffect(() => {
    if (!lastStreamEnd) return;
    if (grantedRef.current === false) return;

    (async () => {
      const win = getCurrentWindow();
      const [focused, visible] = await Promise.all([win.isFocused(), win.isVisible()]);
      if (focused && visible) return;

      const body = preview(lastStreamEnd.content);
      if (grantedRef.current === null) {
        grantedRef.current = await isPermissionGranted();
        if (!grantedRef.current) return;
      }
      sendNotification({ title, body: body || "New message" });
    })();
  }, [lastStreamEnd, title]);
}
