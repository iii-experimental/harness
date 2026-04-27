#!/usr/bin/env bash
# Replay every agent-session fixture against the harness loop.
# Exits non-zero if any fixture's emitted AgentEvent stream diverges from its golden.

set -euo pipefail

cd "$(dirname "$0")/.."

cargo build --bin replay --quiet

failed=0
for fixture in fixtures/agent-sessions/*.jsonl; do
    name=$(basename "$fixture")
    golden="fixtures/golden-events/${name%.jsonl}.json"
    if [ ! -f "$golden" ]; then
        echo "skip $name (no golden; run scripts/replay.sh --write-golden to seed)" >&2
        continue
    fi
    if ./target/debug/replay "$fixture"; then
        :
    else
        failed=1
    fi
done

exit "$failed"
