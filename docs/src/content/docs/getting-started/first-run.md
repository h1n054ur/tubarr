---
title: First run
description: What Tubarr does on its first start, and what to check and set once it runs.
---

## What happens on first start

1. Tubarr reads `/config/tubarr.toml` (or the file named by `--config` or `TUBARR_CONFIG`). A missing `public_url` or a bad value stops it here, with a message naming the key.
2. It opens `tubarr.db` in `data_dir` (default `/config`) and runs the database migrations. They are built into the binary and run on every start.
3. It logs `tubarr started` with the address it listens on (default `0.0.0.0:8480`). While no user exists, the setup code for the first administrator follows (see below).
4. In the background it downloads yt-dlp and deno from their GitHub releases into `/config/bin`, unless the config points at copies installed some other way. This takes about a minute or two. Nothing is logged when it starts, and each tool logs `binary installed` when it is in place. Until yt-dlp is there, the Health page shows Tubarr as failing with "yt-dlp is not installed or does not run". After that it checks for updates once a day.
5. If Sonarr or Radarr is connected, in the config file or on the Settings page, it reads the tagged titles right away, and then every `sync_interval`.

## The first administrator

The first visit to the web UI shows the setup page, as long as no user exists. Enter the setup code printed in Tubarr's log, then a username and password. That user is an administrator and is signed in at once. On a headless install with no browser at hand, the same step is one call, `POST /auth/setup` (see [Admin API](../../reference/admin-api/)). For an automated install, set the `TUBARR_SETUP_CODE` environment variable to a code of your own, and Tubarr uses it instead of printing a random one. More users, and sign-in through OpenID Connect (Authentik, Keycloak and the like), are set up later on the Settings page and in `[auth]`.

## Look around

The web UI has six pages:

| Page | What it shows |
|---|---|
| Shows and films | every show, daily show and film, with its rules and its release and unmatched counts. The drawer edits the rules and runs a dry run that writes nothing. |
| Drafts | for a show missing from TheTVDB or a film missing from TMDB: checks the title there, drafts the entry from what Tubarr knows about the official channel, and shows the text to copy into the site's add form. It never posts anything. |
| Unmatched | what the matcher left out and why, with pins and ignore. |
| Activity | grabs, searches, pushes and play starts, page by page, and the "Push released" catch-up. |
| Health | yt-dlp and deno versions, YouTube lookups and the limiter, the canary, the watcher and its searches, the matcher. |
| Settings | the effective config without secret values, the Sonarr and Radarr connections (connect, test, check their setup), sign-in and users. |

## Check these first

- **Health page.** A minute or two after start, yt-dlp and deno should show a version. If they don't, Tubarr could not download them: check that `/config` is writable by uid 1000 and that the container can reach GitHub.
- **Connect Sonarr and Radarr.** The recommended way is the Settings page, under Connections: enter the arr's URL and API key and press **Test and save**. Tubarr tests them against the arr before it saves them in its database, never shows the key again, and reads the arr right away. **Remove** forgets a saved connection. You can set `[sonarr]` and `[radarr]` in `tubarr.toml` instead, with the key in a secret file: an arr set there wins, and the Settings page shows it as read only.
- **Check the arr's setup.** **Test Sonarr** and **Test Radarr** check the connection. **Check Sonarr setup** and **Check Radarr setup** read the arr and list what Tubarr needs there: the tag, the Tubarr indexer, the Tubarr download client, Import Extra Files with `srt`, and for Sonarr the webhook (optional unless you use audio only versions or episode images). Each item shows as In place, Missing or Optional with what to change. The check changes nothing in the arr. [Sonarr and Radarr](../../setup/sonarr-and-radarr/) has the full setup.
- **From the command line.** `curl http://<host>:8480/healthz` answers `ok`. `/api/health` returns the same details as the Health page as JSON, for scripts, with your API key sent as `X-Api-Key`.

## Settings worth making now

- Who may play: Jellyfin fetches `/play` as a plain URL, so it takes no sign-in. Keep Tubarr on a private network and set `[auth] play_allowed_networks` to the network Jellyfin is on, for example `["192.168.1.0/24"]`.
- The canary: `[canary] video_id` names a known good video that Tubarr looks up every hour. Three failures in a row make Tubarr update yt-dlp, and the Health page shows the result. It is off until you set it.
- Captions: `[subtitles] languages` lists the caption languages written next to each `.strm` (default English, Urdu and Hindi).
- Notices: `[notify]` can send a notice for the canary, cookies use, yt-dlp updates, grabs and a daily digest of unmatched uploads, either as a Discord card or as JSON to any HTTP endpoint. It is off by default.

Every key and its default is listed in `config/tubarr.example.toml` in the repository. After editing `tubarr.toml`, restart the container for the change to take effect.
