#!/usr/bin/env bash
# End-to-end check: real Sonarr and Radarr in throwaway containers, the release Tubarr binary and
# a fake YouTube in a third container on the same network (no network to YouTube). See "End-to-end check" in CONTRIBUTING.md.
#
#   e2e/run.sh [evidence_dir]     # KEEP=1 leaves everything running for a look, then e2e/run.sh down
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
if [ "${1:-}" != --lifecycle-worker ]; then
  if [ "$#" -gt 1 ]; then
    echo 'usage: e2e/run.sh [evidence_dir] | down' >&2
    exit 2
  fi
  if [ "${1:-}" = down ]; then
    exec python3 "$repo/e2e/state.py" down
  fi
  exec python3 "$repo/e2e/state.py" start "$@"
fi
shift
[ "$#" -le 1 ] || { echo 'e2e: invalid worker arguments' >&2; exit 2; }
python3 "$repo/e2e/state.py" check-worker
umask 077
trap 'exit 130' INT
trap 'exit 143' TERM

worker() {
work=$E2E_WORK
net=$E2E_NET
labels=(--label io.tubarr.e2e=1 --label "io.tubarr.e2e.uid=$E2E_OWNER_UID" --label "io.tubarr.e2e.run=$E2E_RUN_ID")
sonarr_image=lscr.io/linuxserver/sonarr:4.0.20.3014-ls326
radarr_image=lscr.io/linuxserver/radarr:6.4.4.10685-ls319
# Debian trixie's glibc runs a binary built on a current distro, and python runs the fakes.
tubarr_image=python:3.13.8-slim-trixie
sonarr_port=${SONARR_PORT:-18989}
radarr_port=${RADARR_PORT:-17878}
tubarr_port=${TUBARR_PORT:-18480}
yt_port=8481

evidence=${1:-$repo/target/e2e-evidence}
mkdir -p "$evidence"
binary=$repo/target/release/tubarr
[ -x "$binary" ] || { echo "build first: cargo build --release" >&2; exit 1; }

mkdir -p "$work"/{sonarr,radarr,tubarr/secrets,bin} "$work"/data/{downloads/tubarr,media/tv,media/movies}
chmod 700 "$work/tubarr/secrets"

docker network create "${labels[@]}" "$net" >/dev/null
for app in sonarr radarr; do
  image=$sonarr_image port=8989 host_port=$sonarr_port
  [ "$app" = radarr ] && image=$radarr_image port=7878 host_port=$radarr_port
  name=$E2E_SONARR_NAME
  [ "$app" = radarr ] && name=$E2E_RADARR_NAME
  docker run -d --name "$name" --network "$net" --network-alias "tubarr-e2e-$app" "${labels[@]}" --memory 1g \
    -e PUID="$(id -u)" -e PGID="$(id -g)" -e TZ=Etc/UTC \
    -p "127.0.0.1:$host_port:$port" \
    -v "$work/$app:/config" -v "$work/data:$work/data" "$image" >/dev/null
done

# API keys go from config.xml straight into 600 files, never onto a command line or the screen.
for app in sonarr radarr; do
  for _ in $(seq 60); do
    grep -q '<ApiKey>' "$work/$app/config.xml" 2>/dev/null && break
    sleep 2
  done
  (umask 077; sed -n 's:.*<ApiKey>\(.*\)</ApiKey>.*:\1:p' "$work/$app/config.xml" > "$work/tubarr/secrets/${app}_api_key")
done
(umask 077; head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$work/tubarr/secrets/tubarr_api_key")

# deno is never run against the fake, it only has to answer --version.
printf '#!/bin/sh\necho "deno 2.9.7 (stable, release, x86_64-unknown-linux-gnu)"\n' > "$work/bin/deno"
chmod +x "$work/bin/deno"
echo '{"videos": []}' > "$work/catalog.json"

yt=http://127.0.0.1:$yt_port
media=$repo/crates/tubarr/tests/fixtures/play

cat > "$work/tubarr/tubarr.toml" <<TOML
listen = "0.0.0.0:8480"
public_url = "http://tubarr-e2e-tubarr:8480"
data_dir = "$work/tubarr"
complete_dir = "$work/data/downloads/tubarr"
api_key_file = "$work/tubarr/secrets/tubarr_api_key"

[rate_limit]
lookups = 10
interval = "1s"
jitter = "0s"
burst = 20

[binaries]
bin_dir = "$work/bin"
yt_dlp = "$repo/e2e/fake-yt-dlp"
deno = "$work/bin/deno"

[watcher]
feed_url = "$yt/feeds/videos.xml"

[matcher]
lookups_per_run = 20

[arr]
sync_interval = "1m"

# Sonarr reports the drama as English (it has no Urdu): its release names must still say URDU.
[channels.UCe2eFakeDramaChannel001]
network = "ARY Digital"
language = "Urdu"

[sonarr]
url = "http://tubarr-e2e-sonarr:8989"
api_key_file = "$work/tubarr/secrets/sonarr_api_key"

[radarr]
url = "http://tubarr-e2e-radarr:7878"
api_key_file = "$work/tubarr/secrets/radarr_api_key"
TOML

# Tubarr starts before the arr setup: their indexer and client tests call it.
docker run -d --name "$E2E_TUBARR_NAME" --network "$net" --network-alias tubarr-e2e-tubarr "${labels[@]}" --memory 512m --user "$(id -u):$(id -g)" \
  -p "127.0.0.1:$tubarr_port:8480" \
  -v "$repo:$repo:ro" -v "$work:$work" \
  -e TUBARR_E2E_CATALOG="$work/catalog.json" -e TUBARR_E2E_YT="$yt" -e TUBARR_E2E_MEDIA="$media" \
  -e E2E_WORK="$work" -e RUST_LOG=info,tubarr=debug \
  "$tubarr_image" sh -c "python3 $repo/e2e/fake_youtube.py serve $yt_port $media & exec $binary --config $work/tubarr/tubarr.toml" >/dev/null

export E2E_WORK=$work E2E_EVIDENCE=$evidence E2E_TUBARR=http://127.0.0.1:$tubarr_port
export E2E_TUBARR_INNER=http://tubarr-e2e-tubarr:8480 TUBARR_E2E_CATALOG=$work/catalog.json
export E2E_SONARR=http://127.0.0.1:$sonarr_port E2E_RADARR=http://127.0.0.1:$radarr_port
rc=0
printf 'driver-started\n' >&"$E2E_DRIVER_FD"
python3 "$repo/e2e/drive.py" || rc=$?
exit "$rc"
}

worker "$@"
