import pino from "pino";

export const logger = pino({
  name: "shore-llm",
  level: process.env.LOG_LEVEL ?? "info",
  base: { service: "shore-llm" },
});

export function childWithRid(rid: string | undefined): pino.Logger {
  return rid ? logger.child({ rid }) : logger;
}
