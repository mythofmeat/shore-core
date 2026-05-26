import { describe, expect, it } from "bun:test";

import { SwpServer } from "../src/swp/server.ts";

function fakeHandshake() {
  return {
    helloSnapshot: () => ({ characters: [] }),
    historySnapshot: () => ({
      messages: [],
      config: { active_model: null, private: false },
      revision: 0,
    }),
  };
}

async function connectExpectingRead(host: string, port: number): Promise<string> {
  let received = "";
  let closed = false;
  await Bun.connect({
    hostname: host,
    port,
    socket: {
      open() {},
      data(_sock, chunk) {
        received += chunk.toString("utf8");
      },
      close() {
        closed = true;
      },
    },
  });
  // Drain — the server either sends a hello frame or half-closes immediately.
  const deadline = Date.now() + 500;
  while (!closed && received.length === 0 && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 10));
  }
  // Give one more brief window for late-arriving FIN after a hello.
  await new Promise((r) => setTimeout(r, 20));
  return received;
}

describe("SwpServer allowed_hosts filter", () => {
  it("accepts the connection when allowed_hosts is empty", async () => {
    const server = new SwpServer({
      host: "127.0.0.1",
      port: 0,
      serverName: "test",
      handshake: fakeHandshake(),
    });
    const listen = server.start();
    try {
      const received = await connectExpectingRead(listen.host, listen.port);
      expect(received).toContain('"type":"hello"');
    } finally {
      server.stop();
    }
  });

  it("accepts the connection when peer IP is in allowed_hosts", async () => {
    const server = new SwpServer({
      host: "127.0.0.1",
      port: 0,
      serverName: "test",
      handshake: fakeHandshake(),
      allowedHosts: ["127.0.0.1"],
    });
    const listen = server.start();
    try {
      const received = await connectExpectingRead(listen.host, listen.port);
      expect(received).toContain('"type":"hello"');
    } finally {
      server.stop();
    }
  });

  it("drops the connection when peer IP is not in allowed_hosts", async () => {
    const server = new SwpServer({
      host: "127.0.0.1",
      port: 0,
      serverName: "test",
      handshake: fakeHandshake(),
      allowedHosts: ["10.0.0.5"],
    });
    const listen = server.start();
    try {
      const received = await connectExpectingRead(listen.host, listen.port);
      // No hello frame should arrive — rejected connections get FIN with no payload.
      expect(received).toBe("");
    } finally {
      server.stop();
    }
  });
});
