#!/usr/bin/env bash
# Regenerate README screenshots (docs/assets/*.png) from synthetic data.
# Real ~/.claude data never appears: the tape redirects HOME to DEMO_HOME,
# and demo_data.py itself refuses to write into the real home.
set -euo pipefail
cd "$(dirname "$0")/.."

DEMO_HOME=/tmp/ccsight-demo-home

if [ "$DEMO_HOME" = "$HOME" ]; then
    echo "refusing to run: DEMO_HOME equals the real HOME" >&2
    exit 1
fi
if ! command -v vhs >/dev/null 2>&1; then
    echo "vhs is not installed (brew install vhs)" >&2
    exit 1
fi

cargo build --release

# Several alive PIDs make the Live tab show a full set of busy sessions in
# the captures (each maps to a distinct today session).
LIVE_PIDS=()
LIVE_ARGS=()
for _ in 1 2 3 4 5; do
    sleep 600 &
    LIVE_PIDS+=("$!")
    LIVE_ARGS+=(--live-pid "$!")
done
trap 'kill "${LIVE_PIDS[@]}" 2>/dev/null || true' EXIT

python3 scripts/demo_data.py --home "$DEMO_HOME" --force "${LIVE_ARGS[@]}" --days 365

mkdir -p docs/assets
vhs scripts/demo-screens.tape
rm -f docs/assets/screens-run.gif

echo "--- generated assets ---"
ls -la docs/assets/
