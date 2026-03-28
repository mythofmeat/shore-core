import { createServer } from "node:http";
import { unlinkSync } from "node:fs";
import { logger } from "./logger.js";
import { dispatch } from "./router.js";

const socketPath = process.argv[2] ?? process.env.SHORE_LLM_SOCKET;

if (!socketPath) {
  logger.fatal("Usage: shore-llm <socket-path>  (or set SHORE_LLM_SOCKET)");
  process.exit(1);
}

const server = createServer((req, res) => {
  // Bun's HTTP server does not close the socket after res.end() for async
  // streaming handlers. Since shore-llm is one-request-per-connection,
  // destroy the socket once the response is fully flushed.
  res.on("finish", () => req.socket?.destroy());

  dispatch(req, res).catch((err) => {
    logger.error({ err }, "unhandled error");
    const errMsg = err instanceof Error ? err.message : String(err);
    if (!res.headersSent) {
      const body = JSON.stringify({ error: "internal", message: errMsg });
      res.writeHead(500, { "Content-Type": "application/json", "Connection": "close" });
      res.end(body);
    } else {
      // Headers already sent (mid-stream error) — just close the response.
      res.end();
    }
  });
});

// Remove stale socket file from a previous (unclean) run so that bind()
// does not fail with EADDRINUSE on restart.
try {
  unlinkSync(socketPath);
} catch {
  // File does not exist — nothing to clean up.
}

server.listen(socketPath, () => {
  logger.info({ socketPath }, "shore-llm listening");
});

function shutdown(): void {
  logger.info("shutting down");
  server.close(() => process.exit(0));
}

process.on("SIGTERM", shutdown);
process.on("SIGINT", shutdown);

export { server };
