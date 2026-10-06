#!/usr/bin/env bash
set -euo pipefail

# Keep Crow's current stable Rust toolchain while entering the repository's
# declared native-library environment for GTK, Wayland and protobuf builds.
export CTERM_CI_WORKER_PATH="$PATH"
exec nix develop --no-update-lock-file --command bash -euo pipefail -c '
  exec 2>&1
  export PATH="$CTERM_CI_WORKER_PATH:$PATH"
  # Runtime loader path for the flake-declared native libraries (cairo, gtk4,
  # libadwaita, pango, wayland): `nix develop` provides them at build time via
  # pkg-config, but cargo-built test binaries need the store lib dirs at exec
  # time (else libgtk-4.so.1 fails to load). Derive libdirs from the in-shell
  # pkg-config — actual closure paths, never hardcoded store/host paths.
  # Runtime loader path from the FULL pkg-config link closure of the
  # flake-declared libraries: --libs-only-L follows Requires chains, so
  # transitive dirs (glib/gobject/gio and friends) are included — a fixed
  # per-package list silently misses them (libgio-2.0.so.0, exit 127).
  # Actual closure paths, never hardcoded store/host paths; deduplicated.
  # The query runs separately under set -e (not inside the for-list): a
  # failed pkg-config must abort with its diagnostics, never iterate partial
  # output. Only usable absolute -L dirs enter the loader path.
  cterm_flags=$(pkg-config --libs-only-L cairo gtk4 libadwaita-1 pangocairo wayland-client 2>&1) || { echo "pkg-config link closure query failed in nix develop shell" >&2; printf "%s\n" "$cterm_flags" >&2; exit 2; }
  cterm_ld=""
  # Intentional word splitting of the captured flag list.
  for flag in $cterm_flags; do
    dir=${flag#-L}
    case "$dir" in /*) ;; *) continue;; esac
    [ -d "$dir" ] || continue
    case ":$cterm_ld:" in *":$dir:"*) ;; *) cterm_ld="${cterm_ld:+$cterm_ld:}$dir";; esac
  done
  test -n "$cterm_ld" || { echo "pkg-config link closure yielded no usable lib dirs" >&2; exit 2; }
  export LD_LIBRARY_PATH="$cterm_ld${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  cargo fmt --all -- --check
  # Integration tests spawn the daemon binary (mirrors GHA
  # "Build ctermd for integration tests" step).
  cargo build --locked -p cterm-headless
  # Integration tests locate the daemon at source-relative target/debug
  # (owner GHA layout: no CARGO_TARGET_DIR there), but this job builds under
  # persistent $CARGO_TARGET_DIR; expose ONLY the freshly built debug binary
  # into the expected layout (a release artifact here would be stale — this
  # job never builds release). Same repo-root-cwd precondition as the adjacent
  # cargo invocations; the canonical-path compare also skips the self-copy
  # when CARGO_TARGET_DIR already is the repo target dir. Product code, cache
  # behavior and the binary itself are untouched.
  if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    src_bin="$CARGO_TARGET_DIR/debug/ctermd"
    dst_bin="target/debug/ctermd"
    if [ -x "$src_bin" ] && [ "$(readlink -m "$src_bin")" != "$(readlink -m "$dst_bin")" ]; then
      mkdir -p "target/debug"
      cp -f "$src_bin" "$dst_bin"
    fi
  fi
  cargo test --locked --workspace --exclude cterm-cocoa --exclude cterm-win32 --all-targets
  printf "%s\n" "cterm ci: stage=cargo-clippy"
  command -v cargo || true
  cargo --version || true
  cargo clippy --version || true
  cargo clippy --locked --workspace --exclude cterm-cocoa --exclude cterm-win32 --all-targets -- -D warnings
'
