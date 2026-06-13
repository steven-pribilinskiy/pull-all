#!/bin/sh
# polygit installer — downloads the latest release binary for your platform.
#   curl -fsSL https://steven-pribilinskiy.github.io/polygit/install.sh | bash
#
# Env:
#   POLYGIT_INSTALL   install dir (default: ~/.local/bin)
set -eu

repo="steven-pribilinskiy/polygit"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)  os="unknown-linux-gnu" ;;
  Darwin) os="apple-darwin" ;;
  *) echo "polygit: unsupported OS '$os' — Linux and macOS only (on Windows, use WSL)." >&2; exit 1 ;;
esac

case "$arch" in
  x86_64 | amd64)  arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *) echo "polygit: unsupported architecture '$arch'." >&2; exit 1 ;;
esac

target="${arch}-${os}"
url="https://github.com/${repo}/releases/latest/download/polygit-${target}"
dest="${POLYGIT_INSTALL:-$HOME/.local/bin}"

echo "polygit: downloading ${target}…"
mkdir -p "$dest"
if ! curl -fsSL "$url" -o "$dest/polygit"; then
  echo "polygit: download failed from $url" >&2
  echo "polygit: no prebuilt binary for ${target}? Install with cargo instead:" >&2
  echo "  cargo install --git https://github.com/${repo}" >&2
  exit 1
fi
chmod +x "$dest/polygit"

echo "polygit: installed to $dest/polygit"
"$dest/polygit" --version 2>/dev/null || true

case ":$PATH:" in
  *":$dest:"*) ;;
  *)
    echo ""
    echo "Add it to your PATH (then restart your shell):"
    echo "  export PATH=\"$dest:\$PATH\""
    ;;
esac
