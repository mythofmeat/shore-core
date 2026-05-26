/**
 * Listen-address resolution and remote-access policy for the TS daemon.
 *
 * Mirrors `resolve_listen_addr`, `bind_addr_is_loopback`, and
 * `validate_remote_access_policy` in `backend/daemon/src/main.rs`. Lives
 * in its own module so unit tests can import the helpers without running
 * `main.ts`'s top-level startup.
 */

export type ListenAddrSource = "cli" | "env" | "config";

/**
 * Precedence: `--addr` → non-empty `SHORE_ADDR` → `[daemon].addr` from
 * loaded config (default `127.0.0.1:7320`).
 */
export function resolveListenAddr(
  cliAddr: string | undefined,
  envAddr: string | undefined,
  configAddr: string,
): { addr: string; source: ListenAddrSource } {
  if (cliAddr !== undefined) return { addr: cliAddr, source: "cli" };
  const env = envAddr?.trim();
  if (env !== undefined && env.length > 0) return { addr: env, source: "env" };
  return { addr: configAddr, source: "config" };
}

function extractBindHost(addr: string): string | undefined {
  if (addr.startsWith("[")) {
    const rb = addr.indexOf("]");
    if (rb < 0) return undefined;
    const host = addr.slice(1, rb);
    const suffix = addr.slice(rb + 1);
    if (host.length === 0 || !suffix.startsWith(":")) return undefined;
    return host;
  }
  const lastColon = addr.lastIndexOf(":");
  if (lastColon <= 0) return undefined;
  const host = addr.slice(0, lastColon);
  const port = addr.slice(lastColon + 1);
  if (host.length === 0 || port.length === 0) return undefined;
  return host;
}

/**
 * Matches `localhost`, `::1`, and any IPv4 in 127.0.0.0/8 — same set the
 * Rust daemon treats as loopback. Throws on malformed input so the caller
 * surfaces a precise startup error rather than silently allowing a bind.
 */
export function bindAddrIsLoopback(addr: string): boolean {
  const host = extractBindHost(addr);
  if (host === undefined) {
    throw new Error(
      `Invalid daemon listen address ${JSON.stringify(addr)}. Expected HOST:PORT or [IPv6]:PORT.`,
    );
  }
  if (host === "localhost" || host === "::1") return true;
  const ipv4 = /^(\d{1,3})\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.exec(host);
  if (ipv4) return Number(ipv4[1]) === 127;
  return false;
}

/**
 * Returns the warnings produced by an opted-in remote bind. Throws on a
 * non-loopback bind without `unsafe_allow_remote_access = true`.
 */
export function validateRemoteAccessPolicy(
  addr: string,
  unsafeAllowRemoteAccess: boolean,
  allowedHosts: string[],
): string[] {
  if (bindAddrIsLoopback(addr)) return [];
  if (!unsafeAllowRemoteAccess) {
    throw new Error(
      `Refusing to bind shore-daemon-ts to non-loopback address ${addr}. ` +
        `Set [daemon].unsafe_allow_remote_access = true to acknowledge unauthenticated remote TCP exposure. ` +
        `[daemon].allowed_hosts is only an IP allowlist and does not provide authentication or TLS.`,
    );
  }
  const warnings: string[] = [
    "Remote TCP access is enabled. Shore does not provide authentication or TLS. Restrict Shore to trusted private or overlay networks; [daemon].allowed_hosts only narrows peer IPs and is not a complete security boundary.",
  ];
  if (allowedHosts.length === 0) {
    warnings.push(
      "Remote TCP access is enabled with an empty [daemon].allowed_hosts list; any host that can reach the port may connect.",
    );
  }
  return warnings;
}
