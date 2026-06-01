# shore-daemon — Debian / Raspberry Pi package

A personal arm64 `.deb` of `shore-daemon` for a headless always-on host
(built and tested on a Raspberry Pi 5 running Debian 12 "bookworm").

> This package is published for the maintainer's own convenience. The program
> makes no safety or fitness guarantees for anyone else; you are encouraged
> **not** to run it.

## What it installs

- `/usr/bin/shore-daemon` — the daemon binary (arm64, glibc 2.36).
- A dedicated, unprivileged **`shore`** system user/group (no sudo, no docker,
  no login shell). The daemon — and the bash/exec tool it hands the model —
  runs as this user, so the OS account is the sandbox boundary.
- `/lib/systemd/system/shore-daemon.service` — a hardened **system** service
  (read-only host, writes confined to `/var/lib/shore`, all capabilities
  dropped). Distinct from the interactive `user` unit in
  `contrib/shore-daemon.service`.
- `/etc/shore/` — config dir, `root:shore 0750` (holds provider keys; readable
  by the daemon, not world-readable).
- `/var/lib/shore/` — state dir (character workspaces, ledger), owned by
  `shore`.
- Example configs under `/usr/share/doc/shore-daemon/`.

## First-time setup

The service is **enabled but not started** on first install — it needs config
before it can run.

```sh
# 1. Drop config (with your provider keys) into /etc/shore as root.
sudo cp /usr/share/doc/shore-daemon/config.toml.example /etc/shore/config.toml
sudo cp /usr/share/doc/shore-daemon/models.toml.example /etc/shore/models.toml
sudo chgrp shore /etc/shore/*.toml && sudo chmod 0640 /etc/shore/*.toml
sudoedit /etc/shore/config.toml   # add keys ...

# 2. Start it.
sudo systemctl start shore-daemon
systemctl status shore-daemon
journalctl -u shore-daemon -f
```

To inspect or back up character workspaces under `/var/lib/shore` as your own
user, add yourself to the group: `sudo usermod -aG shore eshen` (re-login).

## Updates

Updates arrive through the signed apt repo (`repo-deb`) and
`unattended-upgrades`; the package restarts the running daemon automatically on
upgrade. To update by hand: `sudo apt update && sudo apt upgrade`.

## Sandbox scope (important)

The `shore` user protects the **host** from the model's bash tool: no
privilege escalation (`NoNewPrivileges`), `/home` and `/` hidden/read-only,
writes confined to `/var/lib/shore`. It does **not** hide the daemon's own
secrets from the model — the bash tool runs in-process as the same `shore`
user, so it can read whatever the daemon can (e.g. `/etc/shore/config.toml`).
Isolating keys from the tool would require app-level privilege separation
(running the tool as a separate user), which this package does not do.

Syscall/exec filtering is intentionally **not** applied: it would also break
the arbitrary commands the model legitimately runs through the bash tool.
