#!/bin/bash
# Build shore packages and upload to Gitea's Arch package registry.
#
# Usage: ./contrib/build-and-repo.sh
#
# Requires GITEA_TOKEN env var (personal access token with package write scope).
#
# One-time pacman.conf setup:
#
#   [shore]
#   SigLevel = Optional TrustAll
#   Server = http://localhost:3000/api/packages/eshen/arch/$repo/$arch
#
# Then: sudo pacman -Sy shore-daemon shore-cli shore-tui shore-matrix

set -euo pipefail

GITEA_URL="${GITEA_URL:-http://localhost:3000}"
GITEA_OWNER="${GITEA_OWNER:-eshen}"
GITEA_REPO_NAME="eshen"  # pacman repo name (the [section] in pacman.conf)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if [[ -z "${GITEA_TOKEN:-}" ]]; then
    echo "Error: GITEA_TOKEN env var is required (Gitea personal access token)"
    exit 1
fi

# Build the packages
cd "$SCRIPT_DIR"
makepkg -sf --noconfirm

# Upload to Gitea
for pkg in *.pkg.tar.zst; do
    [ -f "$pkg" ] || continue
    echo "Uploading $pkg to Gitea..."
    curl --fail --user "${GITEA_OWNER}:${GITEA_TOKEN}" \
         --upload-file "$pkg" \
         "${GITEA_URL}/api/packages/${GITEA_OWNER}/arch/${GITEA_REPO_NAME}"
    echo ""
    echo "Uploaded $pkg"
    rm -f "$pkg"
done

echo ""
echo "Done! Packages available in Gitea package registry."
echo ""
echo "If this is your first time, add to /etc/pacman.conf:"
echo ""
echo "  [${GITEA_REPO_NAME}]"
echo "  SigLevel = Optional TrustAll"
echo "  Server = ${GITEA_URL}/api/packages/${GITEA_OWNER}/arch/\$repo/\$arch"
echo ""
echo "Then run: sudo pacman -Sy shore-daemon shore-cli shore-tui shore-matrix"
