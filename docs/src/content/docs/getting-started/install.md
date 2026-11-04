---
title: Install
description: Run Tubarr with Docker or Docker Compose next to Sonarr, Radarr and Jellyfin.
---

Tubarr ships as a Docker image. It runs as uid 1000 on Debian slim and holds the Tubarr binary with its web UI built in. yt-dlp and deno are not in the image: Tubarr downloads them from their GitHub releases into `/config/bin` on first start and keeps them up to date there.

:::note
The image is `ghcr.io/h1n054ur/tubarr`. Each release is published for amd64 and arm64 (arm64 covers a Raspberry Pi 4 or 5 or an ARM server), tagged `X.Y.Z`, `X.Y` and `latest`.
:::

## What you need

Before you start, please read [Terms and personal use](../../about/terms/). Tubarr is for one household, and running it goes against YouTube's terms.


- Docker, with Compose if you want the Compose setup below.
- Sonarr (for shows) and Radarr (for films), or one of them.
- Jellyfin to play what Sonarr and Radarr import. Plex does not work, since it does not play `.strm` files. Emby and Kodi are untested: see [Which media servers work](../../setup/jellyfin/#which-media-servers-work).
- A downloads folder that Sonarr, Radarr and Tubarr all see at the same path. Grabs write their `.strm` and `.srt` files there, under `complete_dir` (default `/downloads/tubarr`), and the arrs import them from that path.

## Folders

Tubarr uses two mounts:

| Mount | What it holds |
|---|---|
| `/config` | `tubarr.toml`, the database `tubarr.db`, and the managed yt-dlp and deno in `/config/bin`. Must be writable by uid 1000. |
| `/downloads` | the downloads folder Sonarr and Radarr see, mounted at the same path they use. Tubarr writes its grabs under `complete_dir` in it, so it must be writable by uid 1000 too. |

Create the folders and give them to uid 1000. `config` is the `/config` mount. `secrets` holds the files with the API keys, which Compose hands to the container as secrets (see below). The downloads folder is the one Sonarr and Radarr already use, `/srv/media/downloads` in the examples here. `/srv` belongs to root, so the folder under it needs `sudo`:

```sh
mkdir -p config secrets
sudo mkdir -p /srv/media/downloads/tubarr
sudo chown -R 1000:1000 config /srv/media/downloads/tubarr
```

Run the `chown` even if your own uid is 1000: `sudo mkdir` made the downloads folder owned by root.

If a mounted folder does not exist when the container starts, Docker creates it owned by root. Tubarr runs as uid 1000 and then cannot write there: in the downloads folder every grab fails, because `complete_dir` is not writable.

## The config file

Copy `config/tubarr.example.toml` from the repository to `./config/tubarr.toml`. In the copy, set `public_url` and uncomment the `api_key_file` line near the top (not the ones under `[sonarr]`, `[radarr]` and `[jellyfin]`). Every key in it has a default except `public_url`, which is required. `api_key_file` is not required to start, but Sonarr and Radarr need it to talk to Tubarr's `/api`, so a useful file has at least these two lines:

```toml
public_url = "http://192.168.1.20:8480"
api_key_file = "/run/secrets/tubarr_api_key"
```

`public_url` is how Jellyfin, Sonarr and Radarr reach Tubarr. It is written into every `.strm` file and every NZB link, so it must work from inside their containers. Use the address of the Docker host (or a name your network resolves) and the published port, not `localhost`.

`api_key_file` names a file holding the key Sonarr and Radarr send to Tubarr's `/api`. Without it `/api` refuses every request. Secrets never go in `tubarr.toml` itself: every key ending in `_file` names a file holding the value.

Make a key and keep it in `./secrets`:

```sh
openssl rand -hex 32 > secrets/tubarr_api_key
sudo chown 1000:1000 secrets/tubarr_api_key
sudo chmod 600 secrets/tubarr_api_key
```

Compose mounts secret files as they are, with the same owner and mode, and Tubarr runs as uid 1000. Owned by uid 1000 with mode 600, only Tubarr (and root) can read the key. Do the same for each secret file you add later. As with the folders, the `chown` can be skipped when your uid is 1000.

A missing `public_url` or a bad value stops Tubarr at start with a message naming the key. The full list of keys and their defaults is in `config/tubarr.example.toml`, and the [Configuration](../../setup/configuration/) page describes each one.

## Docker Compose

This is the Compose file from the repository (`compose.example.yaml`):

```yaml
# Tubarr next to Sonarr and Radarr. Copy config/tubarr.example.toml to ./config/tubarr.toml and
# set public_url first. The config folder must be writable by uid 1000.
services:
  tubarr:
    # 0.1 follows the newest 0.1.x release, and 0.1.0 would pin one build.
    image: ghcr.io/h1n054ur/tubarr:0.1
    container_name: tubarr
    restart: unless-stopped
    ports:
      - "8480:8480"
    volumes:
      - ./config:/config                # tubarr.toml, tubarr.db, the managed yt-dlp and deno
      - /srv/media/downloads:/downloads # the same folder, at the same path, as in Sonarr and Radarr
    # Each secret appears as /run/secrets/<name> (point the *_file keys in tubarr.toml there). Compose
    # mounts the files as they are, so uid 1000 must be able to read them.
    secrets:
      - tubarr_api_key
      # Only if you connect Sonarr and Radarr in tubarr.toml instead of on the Settings page:
      # create the key file first, then uncomment its line here and below.
      # - sonarr_api_key
      # - radarr_api_key
    environment:
      RUST_LOG: info
    # Measured peak with the default config: about 600 MiB for a yt-dlp lookup while two plays
    # stream, and a lookup alone peaks near 475 MiB, so 512m is too small. 1g leaves room for a
    # full fragment cache on top. Raise it by about 400 MiB for each [rate_limit] processes over 1,
    # by as much as you raise [play] fragment_cache_bytes over its 96 MiB default, and by about
    # 65 MiB for each [play] max_streams over 8. See "Sizing" in the README.
    mem_limit: 1g
    # The image has the same check built in. It is repeated here so it is easy to tune.
    healthcheck:
      test: ["CMD", "/usr/local/bin/tubarr", "--healthcheck"]
      interval: 30s
      timeout: 10s
      start_period: 30s
      retries: 3

secrets:
  tubarr_api_key:
    file: ./secrets/tubarr_api_key
  # sonarr_api_key:
  #   file: ./secrets/sonarr_api_key
  # radarr_api_key:
  #   file: ./secrets/radarr_api_key
```

Compose refuses to start a container whose secret file is missing ("bind source path does not exist"), so the Sonarr and Radarr secrets are commented out here. The simplest way to connect Sonarr and Radarr is the Settings page in Tubarr's web UI, which needs no secret files (see the [Quick start](../quick-start/)). If you would rather set them in `tubarr.toml`, put each arr's API key (Settings > General > Security in Sonarr or Radarr) in `secrets/sonarr_api_key` or `secrets/radarr_api_key`, owned and moded like `tubarr_api_key`, and uncomment its lines in both places.

Start it:

```sh
docker compose up -d
docker compose logs -f tubarr
```

## Docker without Compose

```sh
docker run -d --name tubarr \
  -p 8480:8480 \
  -v ./config:/config \
  -v /srv/media/downloads:/downloads \
  --memory 1g \
  ghcr.io/h1n054ur/tubarr:0.1
```

Without Compose secrets, keep the key files under `./config/secrets/` and point the `_file` keys at `/config/secrets/<name>` instead.

## Check that it runs

```sh
curl http://192.168.1.20:8480/healthz
```

answers `ok`. The image checks itself the same way with `tubarr --healthcheck`, so `docker ps` shows the container as healthy after a short while.

## Build the image yourself

The repository's `Dockerfile` builds the web UI and the binary:

```sh
docker build -t tubarr .
```

Then use `tubarr` as the image name above.

## Next

Follow the [Quick start](../quick-start/) to get from here to a playing episode.
