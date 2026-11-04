---
title: Admin API
description: Every call of /admin/api, the JSON API behind the web UI, and the sign-in calls under /auth.
---

The web UI does everything through `/admin/api`, so anything the UI does, a script can do too. This page lists every call, from `crates/tubarr/src/admin/mod.rs` and the routers it merges. The API can still change between 0.x versions. Breaking changes are listed in `CHANGELOG.md`.

## Authentication

Every `/admin/api` call, and `/api/health`, needs one of:

- the API key from `api_key_file`, sent as an `X-Api-Key` header. This is the way for scripts.
- a session cookie from signing in to the web UI.
- with `[auth] local_bypass = true`, a request from a loopback or private address. The address is the TCP peer's, or, when the peer is listed in `[auth] trusted_proxies`, the client that proxy names in `X-Forwarded-For`. A trusted proxy is never let in as itself. Behind a reverse proxy that is not listed, every request looks local, so leave this off there. See [Configuration](../../setup/configuration/#auth).

Every state-changing request (anything but `GET`) that does not use `X-Api-Key` must also send `X-Requested-With: tubarr`. A cross-site page cannot add that header, which keeps other sites from acting in your name.

Keep the key off the command line, where anyone on the host can read it in the process list. Write the header to a file once, readable only by you (mode 600), and let `curl` read it from there. Quote any URL with a `?` in it, or the shell may treat it as a pattern:

```sh
umask 077
printf 'X-Api-Key: %s\n' "$(sudo cat secrets/tubarr_api_key)" > ~/.tubarr-header
curl -H @"$HOME/.tubarr-header" 'http://192.168.1.20:8480/admin/api/mappings?kind=podcast'
```

Bodies are JSON. Errors are JSON too, as `{"error": "<message>"}`, with these status codes:

| Status | Meaning |
|---|---|
| 401 | not signed in and no valid key |
| 403 | the `X-Requested-With` header is missing |
| 404 | no such mapping, note, pin, draft or call |
| 409 | the row already exists, for example a second mapping for the same TVDB id |
| 422 | a value is not valid. The message names it |
| 502 | something outside Tubarr failed, for example a yt-dlp rollback, a channel handle YouTube did not answer for, or a Sonarr or Radarr connection that did not pass its test |

### Paging

The lists that can grow long page on the server: the unmatched notes, the grabs, the activity events and the drafts. They take `limit` (1 to 100, default 25) and `offset` (default 0), and answer with the page and a `total`, the number of rows the whole filter matches. A `limit` out of range answers 422. The other lists answer in full, as a JSON array.

## Mappings

| Call | What it does |
|---|---|
| `GET /admin/api/mappings` | every mapping, as an array. Filters: `kind` (`show`, `daily`, `film` or `podcast`) and `active` (`true` or `false`). `show` includes podcasts, `podcast` narrows to the shows with `title_number` on |
| `GET /admin/api/mappings/{id}` | one mapping |
| `PATCH /admin/api/mappings/{id}` | change the rules. See the fields below. Answers the mapping |
| `POST /admin/api/mappings` | add a mapping by hand. See below |
| `GET /admin/api/mappings/export` | every mapping with its rules, aliases, pins and root folder override, as the download `tubarr-mappings.json`: `{format, exported_at, mappings}`. No row ids, Sonarr or Radarr ids, notes, releases or secrets |
| `POST /admin/api/mappings/import` | the body is an export file. `?dry_run=true` reports and writes nothing. Answers `{dry_run, creates, updates, unchanged, conflicts}`: the first three are lists of `{title, kind, tvdb_id, tmdb_id}`, a conflict is `{title, tvdb_id, tmdb_id, reason}` and is left as it is. See [Backups and upgrades](../../operations/backups-and-upgrades/#mapping-export-and-import) |
| `POST /admin/api/mappings/{id}/test` | the dry run, "Test against channel": the title rules over the videos already known on the mapping's channels, with a verdict and a reason per video. The body may hold unsaved rule edits, with the same fields as `PATCH`. Writes nothing |
| `POST /admin/api/mappings/{id}/audio-versions` | bring the audio only versions of every episode Sonarr has in step with the mapping's option, and queue the Jellyfin merges again. Answers `{changed, queued}` |
| `POST /admin/api/mappings/{id}/thumbnails` | fetch the episode images still missing. Answers `{written, failed}` |
| `GET /admin/api/mappings/{id}/aliases` | the aliases, each `{alias, source}` with source `arr` (Sonarr's alternate titles, refreshed at every sync) or `user` |
| `PUT /admin/api/mappings/{id}/aliases` | `{"aliases": ["..."]}` replaces the whole list. Aliases from the arr that you leave in keep their source |

Each mapping holds its `id`, `kind`, `is_podcast`, `active`, `title`, `year`, `tvdb_id`, `tmdb_id`, `imdb_id`, `arr_id`, `language` and `language_source` (`arr`, `channel` or `user`), every rule below, `channel_titles` (the channel names YouTube gave, by channel id, for channels added by handle or link), the counts `releases` and `unmatched`, `created_at` and `updated_at` (Unix seconds), and the library folder state: `root_folder`, `wanted_root_folder`, `current_root_folder`, `root_folder_result` and `root_folder_result_at`.

### Rule fields

`PATCH` takes any of these. A field left out stays as it is, and `null` clears it where the type allows. The title, kind and ids belong to Sonarr and Radarr and are refused.

| Field | Type | What it is |
|---|---|---|
| `channel_ids` | list of text | the official channels. Each entry can be a `UC...` channel id, an `@handle`, a channel link (`/channel/UC...`, `/@handle`, `/c/name`, `/user/name`) or a video link, which stands for its uploader. Tubarr saves them as channel ids. Anything but an id or a `/channel/` link costs one read of the YouTube page from the lookup budget |
| `episode_regex` | text or null | a custom pattern with a named group `ep`, and optionally `season` and `part` |
| `excludes` | list of words | added to the built-in excludes |
| `min_duration_secs` | number or null | the minimum length. Null means the default |
| `language` | text | an English language name, like `Urdu`. Setting it marks the language as yours |
| `timezone` | IANA name or null | the channel's time zone, for daily shows and podcasts |
| `utc_offset_mins` | number or null | a fixed offset, used only without `timezone` |
| `playlist_id` | `PL...` or null | an official playlist |
| `playlist_mode` | `source` or `order` | use the playlist for finding videos, or for the episode order |
| `playlist_season`, `playlist_offset` | numbers | in `order` mode, the playlist's season and the number before its first episode |
| `backfill_limit` | number or null | back episodes to release. Null releases all of them, 0 only new ones |
| `title_number` | true or false | match by the `#N` in the title |
| `audio_version` | true or false | an Audio only version next to each episode |
| `episode_thumbnails` | true or false | the YouTube thumbnail as each episode's image |
| `root_folder` | path, `none` or null | the library folder override. `none` turns moves off, null follows the config |

### Adding a mapping by hand

`POST /admin/api/mappings` takes `kind` (`show`, `daily` or `film`), `title`, `language`, `tvdb_id` for a show or daily show or `tmdb_id` for a film, any rule field above except `root_folder`, and `preset` (`drama`, the default, or `podcast`). The podcast preset is for shows only, and fills in what the body leaves out: `title_number` on, `backfill_limit` 10, `min_duration_secs` 1200, the excludes `Clip` and `Best of`, `episode_thumbnails` on, and the time zone of the first channel in `[channels]`. Without a time zone in the body or in `[channels]`, a podcast is refused with a message asking for one. It answers 201 with the new mapping.

## Unmatched and pins

| Call | What it does |
|---|---|
| `GET /admin/api/unmatched` | the notes, newest first, paged. Filters: `state` (`pending`, `duplicate`, `unsure`, `partial` or `capped`), `mapping_id`, `ignored=true` to include ignored notes, and `q`, text in the video title, the show title, the video id or the channel id, in any case. Answers `{items, total}` |
| `POST /admin/api/unmatched/{id}/ignore` | `{"ignored": true}` hides a note, `false` shows it again. Answers 204 |
| `GET /admin/api/pins?mapping_id=<id>` | the pins of one mapping |
| `POST /admin/api/pins` | pin a video. See below. Answers 201 |
| `DELETE /admin/api/pins/{id}` | remove a pin. Answers 204 |

Each note holds its `id`, `mapping_id`, `mapping_title`, `mapping_kind`, `video_id`, `video_title`, `channel_id`, `duration_secs`, `published_at`, `upload_date`, `state`, `season`, `episode`, `part`, `reason`, `ignored` and `updated_at`.

A pin body is `mapping_id` and one 11 character `video_id`, plus the target, which depends on the mapping's kind:

| Kind | Target |
|---|---|
| show | `season` (0 or more) and `episode` (1 or more) |
| daily show | `air_date` as `YYYY-MM-DD` |
| film | nothing |

```json
{"mapping_id": 12, "video_id": "dQw4w9WgXcQ", "season": 1, "episode": 5}
```

A new pin for the same episode replaces the old one. A pin or an unpin wakes the matcher, which acts on it within about half a minute.

## Playability

| Call | What it does |
|---|---|
| `GET /admin/api/unplayable` | videos Tubarr cannot serve: gone, private, or offered by YouTube only in VP9 or AV1 for now. Each is `{video_id, reason, checked_at, retry}`, where `retry` means Tubarr looks it up again later |
| `DELETE /admin/api/unplayable/{video_id}` | let the matcher try a video again. Answers 204 |
| `GET /admin/api/releases/check` | releases, imported ones included, whose video is unplayable or was never looked up, to find dead `.strm` files. `status` is `unplayable` or `unresolved` |

## Activity, settings and tools

| Call | What it does |
|---|---|
| `GET /admin/api/activity` | the grabs, newest first, paged: `{grabs, total}`. Filter: `category` (`tv` or `movies`). Each grab is `{nzo_id, release_name, video_id, category, status, storage_path, created_at}`, with `status` `completed` or `failed` |
| `GET /admin/api/activity/events` | the searches, arr pushes and play starts, newest first, paged: `{items, total, retention_secs}`. Filter: `kind` (`search`, `arr_push` or `play`). Each item has `at` (Unix seconds) and `kind`, plus `function`, `query` and `results` for a search, `app`, `release`, `outcome` and `reasons` for a push, and `video_ids`, `client` (the User-Agent) and `status` for a play. `retention_secs` is `[activity] retention`, 0 when events are kept for good |
| `GET /admin/api/settings` | the effective config and the Sonarr and Radarr connections in use, without secret values: each secret says only `set` or `not set` |
| `GET /admin/api/arr/connections` | `{sonarr, radarr}`, each `{source, url, api_key}`. `source` is `config`, `saved` or null, and `api_key` only says `set` or `not set` |
| `PUT /admin/api/arr/{app}/connection` | save a connection for `sonarr` or `radarr`: `{url, api_key}`. A missing or empty `api_key` keeps the saved one. Tubarr tests it against the arr first and saves only when the arr answers (502 otherwise), then syncs at once. Answers the connection with the arr's `version`. 422 while the config file sets that arr |
| `DELETE /admin/api/arr/{app}/connection` | forget the saved connection and stop using it. 404 when none is saved, 422 while the config file sets that arr |
| `POST /admin/api/arr/{app}/test` | look up the `[arr] tag` in the arr, which tests the URL, the key and the tag at once. Answers `{ok, tag, tag_id}`, or `{ok: false, tag, error}`. 422 when the arr is not connected |
| `POST /admin/api/arr/{app}/wiring` | read only: what Tubarr needs in the arr (the tag, the indexer, the download client, extra file import, and for Sonarr the webhook) and whether each is there. Answers `{ok, items}`, each item `{key, label, state, detail}` with `state` `ok`, `missing` or `optional` (missing, but only needed for a feature that is off). `{ok: false, error}` when the arr cannot be read |
| `POST /admin/api/arr/push-released` | the catch-up: queue every release for a push, skipping what the arr has or does not monitor. Answers `{queued, waiting}`. 422 when auto push is off (it needs `[arr] auto_push` and `api_key_file`) |
| `POST /admin/api/binaries/check` | check yt-dlp and deno for updates now and install them. Answers one `{tool, ok, result}` per tool |
| `POST /admin/api/binaries/{tool}/rollback` | put the previous `yt-dlp` or `deno` back. Answers `{tool, rolled_back}` |

## Drafts

The helper for titles TheTVDB or TMDB do not have yet. It drafts the entry from what the watcher knows about the channel, laid out in the order of the site's add form, and never posts anything anywhere. It costs no YouTube lookups: the checks are read-only searches on TheTVDB or TMDB.

| Call | What it does |
|---|---|
| `POST /admin/api/drafts/check` | `{kind, title, original_title, year}`: is the title on TheTVDB or TMDB? Asks Sonarr and Radarr first, then TheTVDB and TMDB with the `[drafts]` keys |
| `POST /admin/api/drafts/show` | draft a show from a channel: `channel_id`, `title` as the channel writes it, and optionally `playlist_id`, `regex`, `season`, `preset` and the channel's offset |
| `POST /admin/api/drafts/film` | draft a film from one upload: `{"video_id": "..."}` |
| `GET /admin/api/drafts` | the saved drafts, newest first, paged: `{items, total, keys}`. Filters: `kind` (`show` or `film`) and `q`, text in the title in any case. Each item is `{id, kind, title, verdict, episodes, created_at}`. `keys` says for `tvdb` and `tmdb` whether the check can ask that site, as `{ready}` or `{ready: false, reason}` |
| `GET /admin/api/drafts/{id}` | a saved draft |
| `DELETE /admin/api/drafts/{id}` | remove a saved draft. Answers 204 |

## Users

| Call | What it does |
|---|---|
| `GET /admin/api/users` | every user: `id`, `username`, `display_name`, `role`, `oidc_issuer`, `oidc_subject`, `created_at`, `last_login_at`, `disabled` and `has_password`, never the hash |
| `POST /admin/api/users` | add a user: `username` plus `password` and/or `oidc_subject`, and optionally `display_name`. Answers 201 with the user |
| `DELETE /admin/api/users/{id}` | remove a user. The last administrator cannot be removed |

## Sign-in

These are under `/auth` and need no key. Every `POST` must send `X-Requested-With: tubarr`.

| Call | What it does |
|---|---|
| `GET /auth/state` | `setup_required`, the sign-in `methods` (`password`, `oidc`), `oidc_button_label`, the signed-in `user` or null, and `via` (`session`, `api_key` or `local_bypass`) |
| `POST /auth/setup` | `{username, password, setup_code}`: the first administrator, only while no user exists. The setup code is printed in the log at start. Signs in |
| `POST /auth/login` | `{username, password}`: sets a new session cookie. 401 with one message for any failure, 429 with `Retry-After` when throttled |
| `POST /auth/logout` | ends the session |
| `POST /auth/password` | `{current_password, new_password}`: ends the user's other sessions |
| `GET /auth/oidc/start?next=/path` | sends the browser to the OpenID Connect provider. `next` must be a path on this site |
| `GET /auth/oidc/callback` | where the provider sends the browser back |

## Sign-in and response headers

Sign-in is a username and password (argon2id), OpenID Connect (authorization code with PKCE, for Authentik, Keycloak and the like), or both (`[auth] method` is `forms`, `oidc` or `forms+oidc`). With OIDC, `allowed_groups` limits who gets in and new users can be created on their first sign-in. Each user can have a password, an SSO identity or both.

Sessions are `HttpOnly`, `SameSite=Lax` cookies, `Secure` when the UI URL (`[auth] ui_url`, else `public_url`) is https or `[auth] cookie_secure = true`, and only a SHA-256 of each is stored.

Failed logins are counted three ways:

- After five failures from one address, that address waits before it may try any username again, and the wait doubles with each further failure up to 15 minutes.
- Failures for one username from one address block that pair the same way, but never the same username from other addresses, so nobody can lock an account out by failing on purpose.
- Failures for one username from all addresses together only slow its sign-ins: past 20, each sign-in for that name is held for a moment before the answer, at most 4 seconds, and never refused.

Counters live in memory and fade after an hour without failures. The client address here is the same one `/play` and `local_bypass` use, so behind a trusted proxy each real client is counted on its own.

Every response except `/play` carries `Content-Security-Policy` (same-origin scripts, styles, fonts, images and API calls only, since the UI bundles its Lexend font and loads nothing from another host, and `frame-ancestors 'none'`), `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` and a `Permissions-Policy` that turns off camera, microphone, location, payment and USB. `/play` responses carry only `nosniff`, since players fetch them as plain media. `/api` for Sonarr and Radarr keeps its `apikey`. `/play` takes no credentials since Jellyfin fetches it as a plain URL: see [Who may fetch /play](../../setup/jellyfin/#who-may-fetch-play).

## Other endpoints

| Endpoint | Auth | What it is |
|---|---|---|
| `GET /healthz` | none | answers `ok` while Tubarr runs, for health checks |
| `GET /api/health` | as `/admin/api` | the Health page as JSON, `version` included |
| `GET /metrics` | see [Configuration](../../setup/configuration/#metrics) | Prometheus metrics, only with `[metrics] enabled = true` |
| `/api` | `apikey` parameter | the Newznab and SABnzbd APIs Sonarr and Radarr use |
| `GET`, `HEAD /play/{video_id}` | allowed networks | the video, see [Jellyfin](../../setup/jellyfin/#who-may-fetch-play) |
| `GET`, `HEAD /play/{video_id}/audio` | allowed networks | the audio alone, as M4A |
| `POST /hooks/sonarr` | `Authorization: Bearer <token>` | the Sonarr webhook, see [Sonarr and Radarr](../../setup/sonarr-and-radarr/#the-sonarr-webhook) |
