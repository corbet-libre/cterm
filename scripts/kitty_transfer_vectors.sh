#!/usr/bin/env bash
# Headless Kitty OSC 5113 file-transfer vectors (no compositor, no live Kitty).
#
# Runs the scripted consent-driver tests, the default-deny bypass regression
# tests, and the documented-wire differential vectors (codec edge cases,
# zlib/plain chunking, rsync signature/delta validation, decompressed-byte
# accounting). The vectors encode Kitty's documented wire behavior
# (sw.kovidgoyal.net/kitty/file-transfer-protocol); rerun them against a live
# Kitty before claiming differential parity.
#
# Patterned after scripts/foot_compat_test.sh, minus the compositor: live
# comparison stays a manual step (see below) until a Kitty binary is available
# to the harness.

set -euo pipefail

cd "$(dirname "$0")/.."

# The staging-security tests create tempdirs via tempfile, which honors the
# process umask. A 002 umask yields group-writable tempdirs that cterm's
# fail-closed sticky-bit check (validate_shared_parent) correctly refuses, so
# run with the 022 default CI uses.
umask 022

cargo test -p cterm-core --lib kitty_documented_wire
cargo test -p cterm-core --lib kitty_live_corpus
cargo test -p cterm-app --lib kitty_documented_wire
cargo test -p cterm-app --lib kitty_live
cargo test -p cterm-app --lib kitty_transfer_consent
cargo test -p cterm-app --lib bypass

if command -v kitty >/dev/null 2>&1; then
    cat <<'EOF'
LIVE-KITTY CHECKLIST (manual until automated):
  1. Replay each kitty_documented_wire vector against `kitty +kitten transfer`
     and diff the wire bytes (signature layout, delta op framing, status codes).
  2. Drive approve/deny/expire/cancel through a live Kitty peer and compare
     the PTY status sequences with apply_dialog_outcome's outputs.
  3. Record the Kitty version tested in docs/parity.md before closing the row.
EOF
else
    echo "SKIP live-Kitty comparison: no kitty binary installed; documented-wire vectors above stand in."
fi
