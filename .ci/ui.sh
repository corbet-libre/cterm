#!/usr/bin/env bash
set -euo pipefail

# Wayland pane UI/action gate (mirrors build.yml test-linux Run Wayland pane
# UI/action test step, no weakening). Builds run in the repo flake environment
# (GTK/Wayland/protobuf); weston comes from the Crow worker PATH.
export CTERM_UI_WORKER_PATH="$PATH"
exec nix develop --no-update-lock-file --command bash -euo pipefail -c '
  export PATH="$CTERM_UI_WORKER_PATH:$PATH"
  # Runtime loader path for the flake-declared native libraries (cairo, gtk4,
  # libadwaita, pango, wayland): same derivation as linux.sh — the weston-run
  # gtk binary needs the store lib dirs at exec time.
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
  cargo build --locked -p cterm -p cterm-plugin-host
  OUTPUT_DIR="${RUNNER_TEMP:-/tmp}/cterm_ui_test"
  mkdir -p "$OUTPUT_DIR"
  chmod +x scripts/linux_ui_test.sh
  export XDG_RUNTIME_DIR="${RUNNER_TEMP:-/tmp}/cterm-xdg-runtime"
  mkdir -p "$XDG_RUNTIME_DIR"
  chmod 700 "$XDG_RUNTIME_DIR"
  unset DISPLAY
  # ci-job exports CARGO_TARGET_DIR at a persistent absolute path; default to
  # the repo target dir when unset. Relative values resolve under repo root.
  target_dir="${CARGO_TARGET_DIR:-target}"
  case "$target_dir" in /*) ;; *) target_dir="$PWD/$target_dir";; esac
  weston \
    --backend=headless-backend.so \
    --socket=cterm-ci \
    --idle-time=0 \
    --log="$OUTPUT_DIR/weston.log" &
  WESTON_PID=$!
  cleanup_weston() { kill "$WESTON_PID" 2>/dev/null || true; }
  trap cleanup_weston EXIT
  export WAYLAND_DISPLAY=cterm-ci
  export GDK_BACKEND=wayland
  export GSK_RENDERER=cairo
  ./scripts/linux_ui_test.sh "$target_dir/debug/cterm" "$OUTPUT_DIR"
'
