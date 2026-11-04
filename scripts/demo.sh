#!/usr/bin/env bash
# See the UI on demo data: a debug Tubarr on localhost:8480 with a seeded database, the e2e fake
# YouTube and fake yt-dlp, a fake Radarr that takes pushes (nothing reaches the internet, no
# Sonarr or Jellyfin), and the web dev server. Every run starts from an empty .demo/ (git-ignored). Ctrl-C stops everything.
# See "See your change on demo data" in CONTRIBUTING.md.
#
#   scripts/demo.sh    # ports from TUBARR_PORT, YT_PORT, ARR_PORT, WEB_PORT (8480, 8482, 8483, 5173)
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
demo=$repo/.demo
port=${TUBARR_PORT:-8480}
yt_port=${YT_PORT:-8482}
arr_port=${ARR_PORT:-8483}
web_port=${WEB_PORT:-5173}

for p in "$port" "$yt_port" "$arr_port" "$web_port"; do
  if ss -Hltn "sport = :$p" | grep -q .; then
    echo "demo: port $p is already in use" >&2
    exit 1
  fi
done

cargo build -p tubarr --manifest-path "$repo/Cargo.toml"
binary=${CARGO_TARGET_DIR:-$repo/target}/debug/tubarr
[ -d "$repo/web/node_modules" ] || (cd "$repo/web" && bun install --frozen-lockfile)

rm -rf "$demo"
mkdir -p "$demo"/{data/bin,secrets,downloads/tubarr}
chmod 700 "$demo/secrets"
(umask 077; head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$demo/secrets/tubarr_api_key")
(umask 077; echo demo-radarr-key > "$demo/secrets/radarr_api_key")
# deno only has to answer --version.
printf '#!/bin/sh\necho "deno 2.9.7 (stable, release, x86_64-unknown-linux-gnu)"\n' > "$demo/data/bin/deno"
chmod +x "$demo/data/bin/deno"
python3 "$repo/scripts/demo_seed.py" catalog "$demo/catalog.json"

cat > "$demo/tubarr.toml" <<TOML
listen = "127.0.0.1:$port"
public_url = "http://127.0.0.1:$port"
data_dir = "$demo/data"
complete_dir = "$demo/downloads/tubarr"
api_key_file = "$demo/secrets/tubarr_api_key"

[rate_limit]
lookups = 20
interval = "1s"
jitter = "0s"
burst = 40

[binaries]
bin_dir = "$demo/data/bin"
yt_dlp = "$repo/e2e/fake-yt-dlp"
deno = "$demo/data/bin/deno"

[watcher]
feed_url = "http://127.0.0.1:$yt_port/feeds/videos.xml"
# One channel search a day at most, so the seeded backfill state stays as it is.
backfill_spacing = "24h"
# Every channel's feed is read soon after start, so the Health page lists them.
rss_spacing = "1s"
rss_jitter = "0s"

# The matcher never looks anything up, so the seeded notes and releases stay as they are.
[matcher]
interval = "8760h"
lookups_per_run = 0

[subtitles]
timeout = "5s"

# The fake Radarr: its films are tagged but unmonitored, so the matcher wants none of them.
[radarr]
url = "http://127.0.0.1:$arr_port"
api_key_file = "$demo/secrets/radarr_api_key"

[arr]
sync_interval = "8760h"
push_interval = "1s"

# /metrics, read with the API key as the bearer token (scripts/soak reads it).
[metrics]
enabled = true

TOML
python3 "$repo/scripts/demo_seed.py" channels >> "$demo/tubarr.toml"

pids=()
# Exits with the status the script was leaving with: 0 after Ctrl-C, the failure's otherwise. Under
# set -e a failing command in here would replace it, and a terminal or pipe that is already gone
# makes the last echo fail (or SIGPIPE the shell), so nothing in here may fail.
stop() {
  local status=$?
  trap - EXIT
  trap '' PIPE
  for pid in "${pids[@]}"; do kill -TERM -- "-$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
  echo "demo: stopped" 2>/dev/null || true
  exit "$status"
}
trap stop EXIT
trap 'exit 0' INT TERM
# Each background job gets its own process group, so stop() takes its children down with it.
set -m

export TUBARR_E2E_CATALOG=$demo/catalog.json TUBARR_E2E_YT=http://127.0.0.1:$yt_port
export TUBARR_E2E_MEDIA=$repo/crates/tubarr/tests/fixtures/play TUBARR_PORT=$port
# A fixed first-run setup code, so the seed can create the demo admin. Local test value only.
export TUBARR_SETUP_CODE=demo-setup-code-0000
python3 "$repo/e2e/fake_youtube.py" serve "$yt_port" "$TUBARR_E2E_MEDIA" 2> "$demo/fake-youtube.log" &
pids+=($!)
python3 "$repo/scripts/demo_seed.py" radarr "$arr_port" "$demo/radarr-films.json" 2> "$demo/fake-radarr.log" &
pids+=($!)
start_tubarr() {
  RUST_LOG=info "$binary" --config "$demo/tubarr.toml" >> "$demo/tubarr.log" 2>&1 &
  tubarr_pid=$!
  pids+=("$tubarr_pid")
  for _ in $(seq 60); do
    curl -sf "http://127.0.0.1:$port/healthz" >/dev/null && return 0
    sleep 0.5
  done
  echo "demo: Tubarr did not start, see $demo/tubarr.log" >&2
  exit 1
}

key_file=$demo/secrets/tubarr_api_key
start_tubarr
python3 "$repo/scripts/demo_seed.py" seed "http://127.0.0.1:$port" "$demo/data/tubarr.db" "$key_file" \
  "$demo/radarr-films.json"
# A restart has the watcher pick up the new mappings' channels at once, and the Radarr sync give the
# films their Radarr ids. The Activity log lives in memory, so its searches, plays and pushes come
# after it.
kill -TERM -- "-$tubarr_pid"
wait "$tubarr_pid" 2>/dev/null || true
start_tubarr
python3 "$repo/scripts/demo_seed.py" activity "http://127.0.0.1:$port" "$key_file"

(cd "$repo/web" && exec bunx vite --port "$web_port" --strictPort --host 127.0.0.1) > "$demo/vite.log" 2>&1 &
pids+=($!)
for _ in $(seq 60); do
  curl -sf "http://127.0.0.1:$web_port/" >/dev/null && break
  sleep 0.5
done

cat <<EOF

demo: UI at http://127.0.0.1:$web_port/  (sign in as demo / demo-password)
demo: Tubarr at http://127.0.0.1:$port, logs in $demo/*.log
demo: Ctrl-C stops everything
EOF
wait
