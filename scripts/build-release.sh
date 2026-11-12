#!/usr/bin/env bash
# Compila Ponte in release e mette in dist/ il pacchetto .deb e l'eseguibile .exe.
# Requisiti (Ubuntu/Debian): rustup, build-essential, mingw-w64, cargo-deb
#   rustup target add x86_64-pc-windows-gnu && cargo install cargo-deb
set -euo pipefail
cd "$(dirname "$0")/.."
source "$HOME/.cargo/env" 2>/dev/null || true

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
rm -rf dist && mkdir -p dist

echo "==> test"
cargo test --quiet

echo "==> Linux (.deb)"
cargo deb --quiet --output "dist/ponte_${VERSION}_amd64.deb"

echo "==> Windows (.exe)"
cargo build --release --target x86_64-pc-windows-gnu
cp target/x86_64-pc-windows-gnu/release/ponte.exe "dist/ponte-${VERSION}-windows-x64.exe"

echo "==> pronto:"
ls -lh dist/
