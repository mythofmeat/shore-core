# Maintainer: eshen
pkgname=shore
pkgver=0.1.0
pkgrel=1
pkgdesc='Persistent AI character engine — daemon, CLI, and LLM provider proxy'
arch=('x86_64')
url='http://localhost:3000/eshen/silvershore'
license=('custom')
depends=('gcc-libs' 'nodejs')
makedepends=('cargo' 'npm')
source=("git+http://localhost:3000/eshen/silvershore.git")
sha256sums=('SKIP')

prepare() {
    cd silvershore
    export RUSTUP_TOOLCHAIN=stable
    cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"

    cd shore-llm
    npm install
}

build() {
    cd silvershore
    export RUSTUP_TOOLCHAIN=stable
    export CARGO_TARGET_DIR=target
    cargo build --workspace --release --frozen

    cd shore-llm
    npm run build
}

package() {
    cd silvershore

    # Rust binaries
    install -Dm755 target/release/shore-daemon "$pkgdir/usr/bin/shore-daemon"
    install -Dm755 target/release/shore         "$pkgdir/usr/bin/shore"
    install -Dm755 target/release/shore-matrix  "$pkgdir/usr/bin/shore-matrix"

    # shore-llm (Node.js)
    install -dm755 "$pkgdir/usr/lib/shore-llm"
    cp -a shore-llm/dist "$pkgdir/usr/lib/shore-llm/"
    cp -a shore-llm/node_modules "$pkgdir/usr/lib/shore-llm/"
    install -Dm644 shore-llm/package.json "$pkgdir/usr/lib/shore-llm/package.json"

    # Wrapper script for shore-llm (so the daemon can find it)
    install -Dm755 /dev/stdin "$pkgdir/usr/bin/shore-llm" <<'EOF'
#!/bin/sh
exec node /usr/lib/shore-llm/dist/index.js "$@"
EOF

    # Systemd user service
    install -Dm644 contrib/shore-daemon.service \
        "$pkgdir/usr/lib/systemd/user/shore-daemon.service"

    # Example config
    install -Dm644 examples/config.toml \
        "$pkgdir/usr/share/doc/shore/config.toml.example"
    install -Dm644 examples/models.toml \
        "$pkgdir/usr/share/doc/shore/models.toml.example"

    # Shell completions
    install -dm755 "$pkgdir/usr/share/fish/vendor_completions.d"
    install -dm755 "$pkgdir/usr/share/bash-completion/completions"
    install -dm755 "$pkgdir/usr/share/zsh/site-functions"
    target/release/shore completions fish > "$pkgdir/usr/share/fish/vendor_completions.d/shore.fish"
    target/release/shore completions bash > "$pkgdir/usr/share/bash-completion/completions/shore"
    target/release/shore completions zsh  > "$pkgdir/usr/share/zsh/site-functions/_shore"
}
