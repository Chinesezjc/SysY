#!/usr/bin/env bash
set -euo pipefail

echo "[post-create] checking Rust toolchain..."
rustc -V
cargo -V

echo "[post-create] installing rust-src for rust-analyzer..."
RUSTUP_DIST_SERVER=https://static.rust-lang.org rustup component add rust-src

echo "[post-create] installing Claude Code CLI..."
if ! command -v npm &>/dev/null; then
    apt-get update && apt-get install -y npm
fi
npm install -g @anthropic-ai/claude-code

echo "[post-create] done."