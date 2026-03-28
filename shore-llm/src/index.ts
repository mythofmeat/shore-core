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
  // shore-llm is one-request-per-connection. After the response is
  // flushed, close the socket so the client sees EOF. Bun's Unix socket
  // handling can discard buffered data on immediate close, so we set
  // allowHalfOpen and wait for the client to close its end first.
  if (req.socket) req.socket.allowHalfOpen = true;
  res.on("finish", () => {
    const sock = req.socket;
    if (!sock) return;
    // Signal no more writes; the client reads until EOF on its side,
    // then closes, which triggers 'end' here.
    sock.end();
    sock.on("end", () => sock.destroy());
    // Safety: if the client doesn't close promptly, destroy after 5s.
    sock.setTimeout(5000, () => sock.destroy());
  });

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
