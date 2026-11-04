---
title: Backups and upgrades
description: What to back up, how to restore it, and how upgrades and database migrations work.
---

## What to back up

| What | Where | Why it matters |
|---|---|---|
| The database | `tubarr.db` in `data_dir` (`/config`), with `tubarr.db-wal` and `tubarr.db-shm` next to it | your mappings and their rules, aliases, pins, ignored notes, users and their sessions, the releases Tubarr wrote and the list of videos `/play` may serve |
| The config | `/config/tubarr.toml` | every setting. Mappings are not in it |
| The secret files | wherever your `*_file` keys point, `./secrets` in the Compose example | Tubarr's API key, the Sonarr, Radarr and Jellyfin keys, the webhook token, and any OpenID Connect secret, Discord webhook, TheTVDB or TMDB key and cookies file |
| The link files | your Sonarr and Radarr library folders | the `.strm` and `.srt` files the arrs imported. Back them up with the rest of your media library |

Leave out what Tubarr fetches or rebuilds on its own. These are safe to throw away:

| What | What happens without it |
|---|---|
| `/config/bin` | yt-dlp and deno are downloaded again on the next start |
| the sample tables in `[play] index_dir` | each is read again on the next play of its video, one slower first start |
| `tubarr.db-wal` and `tubarr.db-shm` of a stopped instance | nothing: a clean stop has already written them into `tubarr.db` |
| old `tubarr.db.pre-*.bak` copies | nothing, once you no longer want to go back to the version before that upgrade |

If you want the mappings without the whole database, a [mapping export](#mapping-export-and-import) holds them as one small JSON file.

Keep the API key with the database. Every `.strm` file holds `public_url`, and Sonarr and Radarr hold the API key in their indexer and download client settings. A restore with a new key or a new `public_url` means changing both arrs, and rewriting the `.strm` files if the address changed.

### How to back it up

The database runs in SQLite's WAL mode, so the three files belong together and a copy taken while Tubarr writes can be torn. The simple way is a short stop:

```sh
docker compose stop tubarr
tar czf tubarr-backup-$(date +%F).tar.gz config secrets
docker compose start tubarr
```

Anything that reaches Tubarr during the stop fails, a play included, so pick a quiet time. If your backup tool takes filesystem snapshots (ZFS, Btrfs, LVM, a Proxmox backup of the whole container), a snapshot of the running container is fine too: SQLite recovers a WAL database from a snapshot on its next start.

### Restoring

Stop Tubarr, put `config` and `secrets` back in place (owned by uid 1000), and start it. Tubarr runs any migrations the restored database lacks, downloads yt-dlp and deno if `/config/bin` is empty, and carries on. A play of a video whose sample table is gone reads the table again.

Restoring to an older version of Tubarr than the one that wrote the database does not work, see [Downgrades](#downgrades).

## Upgrading

1. Read the release's notes in `CHANGELOG.md`. A release that needs a config change says so in an upgrade note.
2. Back up as above. It takes a minute and is the only way back.
3. Pull the new image and recreate the container:

```sh
docker compose pull tubarr
docker compose up -d tubarr
```

4. Check the log for `tubarr started`, and the version at the top of the Health page.

### Migrations

The database schema is built into the binary as numbered SQL migrations. On every start, Tubarr applies any it has not applied yet, in order, before it serves anything. A new version that changes the schema does so on its first start, with no step for you. Migrations are written to keep existing data: the releases, mappings, pins and notes you had are carried over, and every `.strm` file already imported keeps playing.

A migration that fails stops Tubarr at start with the error in the log, and the database is left as it was before that migration. Restore the backup, or report the error.

### The copy taken before migrations

When a new version brings database changes, Tubarr first copies the database next to it, before it applies any of them. The copy is named

```text
tubarr.db.pre-<from>-to-<to>.<UTC time>.bak
```

where `<from>` is the last migration the database had and `<to>` the newest one the new version brings, for example `tubarr.db.pre-0036-to-0039.20261026T120000Z.bak`. It is a consistent snapshot taken through SQLite, so it holds everything up to that moment, the WAL included, and the log says where it went.

- Only the newest copies stay: `[backup] keep` of them, 3 by default. Older ones are deleted after a new copy is written.
- A start with nothing to migrate takes no copy, and neither does a brand new database.
- If the copy fails, for example on a full disk or a folder Tubarr cannot write, Tubarr stops before it migrates anything and says why. Fix the cause and start again, or set `[backup] before_migrations = false` to migrate without a copy.

```toml
[backup]
before_migrations = true
keep = 3
```

This copy is a safety net for upgrades, not a backup: it sits on the same disk as the database. Keep taking your own backups as above.

### Downgrades

Downgrades are not supported. An older version refuses to start on a database that a newer one has migrated, because it finds migrations it does not know. To go back to the version you ran before an upgrade:

1. Stop Tubarr: `docker compose stop tubarr`.
2. In `/config`, move `tubarr.db` aside, for example to `tubarr.db.newer`.
3. Delete `tubarr.db-wal` and `tubarr.db-shm` if they are there.
4. Copy the `.bak` that upgrade wrote to `tubarr.db`. Its name says which migrations it sits between. A backup you took yourself before the upgrade works the same way.
5. Make sure the file is owned by uid 1000.
6. Set the image tag in your Compose file back to the older version and run `docker compose up -d tubarr`.

Anything done since the upgrade is lost, apart from what the arr sync and the watcher find again on their own.

## Mapping export and import

Settings > Export and import mappings downloads every mapping with its rules, aliases, pins and root folder override as one JSON file, `tubarr-mappings.json`. The same file can be imported into another instance or back into this one. The admin API has the same calls: `GET /admin/api/mappings/export` and `POST /admin/api/mappings/import?dry_run=true|false`.

The file holds no secrets and nothing the instance learns by itself: no Sonarr or Radarr ids, notes, releases or caches.

An import first runs as a dry run and shows what it would do, as lists of creates, updates, unchanged mappings and conflicts. Nothing is written until you apply it. Then:

- **Matching.** A mapping in the file is matched to one here by its TVDB id (shows and daily shows) or its TMDB id (films).
- **Merging.** A mapping with no match is added. A matched one takes the file's rules and user aliases. The file's pins are added, or replace the pin for the same episode, and pins the file does not list stay.
- **Skipping.** A mapping is skipped as a conflict, with the reason, when its rules fail the same checks the admin UI makes, when its id appears in the file twice or belongs to another mapping here, or when the mapping here is of another kind. The rest of the file is still imported.

An imported mapping gets its Sonarr or Radarr id from the next arr sync. The import does not write audio only versions or episode images for episodes Sonarr already has: use the buttons on the mapping for those.

## Versions

Tubarr follows [semantic versioning](https://semver.org) and stays on 0.x until 1.0 is declared. While on 0.x:

- a minor release (0.2.0) brings new features, or a breaking change to the config, the APIs or the database. Breaking changes come with an upgrade note in `CHANGELOG.md` saying what to change.
- a patch release (0.1.1) only fixes things and never needs a config change.

Each release is a signed git tag `vX.Y.Z` on `main`, with its notes in `CHANGELOG.md` (Keep a Changelog format). The release workflow builds each release's image for `linux/amd64` and `linux/arm64` and tags it:

| Tag | What it follows |
|---|---|
| `X.Y.Z`, for example `0.1.0` | that one build, never moved |
| `X.Y`, for example `0.1` | the newest patch of that minor release |
| `latest` | the newest release |

Pin `X.Y` to get fixes without surprises, or `X.Y.Z` to stay on one build.

On start Tubarr downloads the yt-dlp and deno builds for the machine it runs on (`yt-dlp_linux` or `yt-dlp_linux_aarch64`, `deno-x86_64-unknown-linux-gnu` or `deno-aarch64-unknown-linux-gnu`), each checked against the SHA-256 sums its release publishes. On any other architecture there is no managed copy: install both yourself and point `[binaries] yt_dlp` and `deno` at them.

The running version and the commit it was built from show at the top of the Health page, and in `/api/health` under `version`. The commit is filled in by the image build (`TUBARR_GIT_SHA`), and a plain `cargo build` reports none.

## yt-dlp and deno

These are not part of the image and need no image upgrade. Tubarr checks their GitHub releases once a day, verifies each download against the SHA-256 sums the release publishes, and swaps it in. It keeps the previous file, so **Roll back** on the Health page (or `POST /admin/api/binaries/{tool}/rollback`) puts it back when a new release breaks lookups. With `[canary] video_id` set, three failed canary lookups in a row make Tubarr check for a yt-dlp update at once, at most once a day.
