#!/usr/bin/env bash
set -euo pipefail

# Keep Crow's current stable Rust toolchain while entering the repository's
# declared native-library environment for GTK, Wayland and protobuf builds.
export CTERM_CI_WORKER_PATH="$PATH"
exec nix develop --command bash -euo pipefail -c '
  export PATH="$CTERM_CI_WORKER_PATH:$PATH"
  cargo fmt --all -- --check
  cargo test --locked --workspace --exclude cterm-cocoa --exclude cterm-win32 --all-targets
  cargo clippy --locked --workspace --exclude cterm-cocoa --exclude cterm-win32 --all-targets -- -D warnings
'
