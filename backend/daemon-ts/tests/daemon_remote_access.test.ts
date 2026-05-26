import { describe, expect, it } from "bun:test";

import {
  bindAddrIsLoopback,
  resolveListenAddr,
  validateRemoteAccessPolicy,
} from "../src/runtime/listen_addr.ts";

describe("resolveListenAddr precedence", () => {
  it("prefers --addr over SHORE_ADDR and config", () => {
    const { addr, source } = resolveListenAddr(
      "127.0.0.1:9000",
      "127.0.0.1:8000",
      "127.0.0.1:7320",
    );
    expect(addr).toBe("127.0.0.1:9000");
    expect(source).toBe("cli");
  });

  it("falls back to SHORE_ADDR when --addr is unset", () => {
    const { addr, source } = resolveListenAddr(undefined, "127.0.0.1:8000", "127.0.0.1:7320");
    expect(addr).toBe("127.0.0.1:8000");
    expect(source).toBe("env");
  });

  it("ignores empty/whitespace SHORE_ADDR and falls through to config", () => {
    const { addr, source } = resolveListenAddr(undefined, "   ", "127.0.0.1:7320");
    expect(addr).toBe("127.0.0.1:7320");
    expect(source).toBe("config");
  });

  it("uses config when CLI and env are both unset", () => {
    const { addr, source } = resolveListenAddr(undefined, undefined, "0.0.0.0:1112");
    expect(addr).toBe("0.0.0.0:1112");
    expect(source).toBe("config");
  });
});

describe("bindAddrIsLoopback", () => {
  it("treats 127.0.0.1, ::1, and localhost as loopback", () => {
    expect(bindAddrIsLoopback("127.0.0.1:7320")).toBe(true);
    expect(bindAddrIsLoopback("[::1]:7320")).toBe(true);
    expect(bindAddrIsLoopback("localhost:7320")).toBe(true);
  });

  it("treats all of 127.0.0.0/8 as loopback (matches Rust is_loopback)", () => {
    expect(bindAddrIsLoopback("127.0.0.2:7320")).toBe(true);
  });

  it("treats 0.0.0.0 and routable IPs as non-loopback", () => {
    expect(bindAddrIsLoopback("0.0.0.0:7320")).toBe(false);
    expect(bindAddrIsLoopback("100.84.100.99:7320")).toBe(false);
  });

  it("throws on malformed addresses", () => {
    expect(() => bindAddrIsLoopback("not-an-address")).toThrow("Invalid daemon listen address");
  });
});

describe("validateRemoteAccessPolicy", () => {
  it("loopback binds need no opt-in and produce no warnings", () => {
    expect(validateRemoteAccessPolicy("127.0.0.1:7320", false, [])).toEqual([]);
  });

  it("refuses non-loopback bind without unsafe_allow_remote_access", () => {
    expect(() => validateRemoteAccessPolicy("0.0.0.0:1112", false, [])).toThrow(
      /unsafe_allow_remote_access/,
    );
  });

  it("opted-in remote bind with empty allowed_hosts emits two warnings", () => {
    const warnings = validateRemoteAccessPolicy("0.0.0.0:1112", true, []);
    expect(warnings).toHaveLength(2);
    expect(warnings[0]).toContain("does not provide authentication or TLS");
    expect(warnings[1]).toContain("any host that can reach the port may connect");
  });

  it("opted-in remote bind with allowed_hosts emits only the thin-security warning", () => {
    const warnings = validateRemoteAccessPolicy("0.0.0.0:1112", true, ["10.0.0.5"]);
    expect(warnings).toHaveLength(1);
    expect(warnings[0]).toContain("trusted private or overlay networks");
  });
});
