import { createServer } from "node:http";
import { logger } from "./logger.js";
import { dispatch } from "./router.js";

const socketPath = process.argv[2] ?? process.env.SHORE_LLM_SOCKET;

if (!socketPath) {
  logger.fatal("Usage: shore-llm <socket-path>  (or set SHORE_LLM_SOCKET)");
  process.exit(1);
}

const server = createServer((req, res) => {
  dispatch(req, res).catch((err) => {
    logger.error({ err }, "unhandled error");
    if (!res.headersSent) {
      res.writeHead(500, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "internal", message: "Internal server error" }));
    }
  });
});

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
