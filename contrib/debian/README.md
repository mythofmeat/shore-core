# Shore Daemon Debian Package

Build a local `.deb` containing:

- `/usr/bin/shore-daemon`
- `/usr/bin/shore-llm-sidecar`
- `shore-daemon.service`
- example config files under `/usr/share/doc/shore/`

Run this on the target architecture for Pi builds:

```sh
bash contrib/debian/build-shore-daemon-deb.sh
```

The script requires `cargo`, `bun`, and either `dpkg-deb` or `ar`+`tar`. It
writes the package to `target/debian/shore-daemon_<version>_<arch>.deb`.

Useful overrides:

```sh
DEB_ARCH=arm64 bash contrib/debian/build-shore-daemon-deb.sh
DEB_DEPENDS='ca-certificates, libssl3' bash contrib/debian/build-shore-daemon-deb.sh
OUT_DIR=/tmp/shore-debs bash contrib/debian/build-shore-daemon-deb.sh
```
