#!/usr/bin/env bash
# A short soak on the demo stack: starts scripts/demo.sh on its own ports, drives it with
# scripts/soak/soak.py and stops it again. The CSV lands in .demo-soak/ (git-ignored).
#
#   scripts/soak/run.sh [workers] [minutes]    # default 2 workers for 5 minutes
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
workers=${1:-2}
minutes=${2:-5}
export TUBARR_PORT=${TUBARR_PORT:-18480} YT_PORT=${YT_PORT:-18482}
export ARR_PORT=${ARR_PORT:-18483} WEB_PORT=${WEB_PORT:-15173}
out=$repo/.demo-soak
mkdir -p "$out"

"$repo/scripts/demo.sh" > "$out/demo.log" 2>&1 &
demo=$!
trap 'kill -TERM "$demo" 2>/dev/null || true; wait "$demo" 2>/dev/null || true' EXIT
for _ in $(seq 600); do
  grep -q "demo: Ctrl-C stops everything" "$out/demo.log" && break
  kill -0 "$demo" 2>/dev/null || { echo "soak: the demo stopped, see $out/demo.log" >&2; exit 1; }
  sleep 1
done
grep -q "demo: Ctrl-C stops everything" "$out/demo.log" || { echo "soak: the demo did not come up" >&2; exit 1; }

csv=$out/soak-$(date +%Y%m%d-%H%M%S).csv
python3 "$repo/scripts/soak/soak.py" --url "http://127.0.0.1:$TUBARR_PORT" \
  --key-file "$repo/.demo/secrets/tubarr_api_key" --workers "$workers" --minutes "$minutes" --out "$csv"
