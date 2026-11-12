# Changelog

All notable changes to Tubarr are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow [semantic versioning](https://semver.org), on 0.x until 1.0 (see [the upgrade docs](https://h1n054ur.github.io/tubarr/operations/backups-and-upgrades/#versions)).

## [0.1.1] - 2026-11-12

### Fixed

- Playback validates the full Content-Range and bounds reads to the requested and returned byte span before appending data.
- Requests through configured trusted proxies are rejected when the forwarded chain does not establish an untrusted client address.
- Managed binary installation has separate limits for downloads, extracted binaries and checksum files, while keeping staging and rollback.
- E2E runs use per-user state and owned Docker resources, reject overlapping runs by the same user and support recovery after KEEP or interrupted cleanup.
- Public pull requests get unprivileged checks. Image publication requires tests and E2E checks for the exact commit being released, and public E2E failure artifacts remove generated credentials, including encoded URL forms.
- Eligible podcast uploads awaiting metadata leave unsure notes instead of being silently skipped or marked not wanted.
- Unmatched-note replacement takes SQLite write intent before reading, avoiding a stale-snapshot failure when another connection writes.

### Known limitations

- Bundled SQLite 3.51.3 (libsqlite3-sys 0.37.0 via SQLx 0.9.0) remains vulnerable to [RUSTSEC-2026-0459](https://rustsec.org/advisories/RUSTSEC-2026-0459). A crafted database and sibling WAL file with differing page sizes can expose process memory. This release accepts that risk with a specific audit exception pending a compatible official SQLx fix. Keep database files, WAL files, storage and backups trusted, and never restore untrusted database files. Normal mapping JSON imports are metadata, not raw database uploads. See [SECURITY.md](SECURITY.md).

## [0.1.0] - 2026-11-04

The first public release. It covers everything built so far.

### Indexer and download client

- A Newznab API on `/api` that Sonarr and Radarr add as an indexer, with release names they parse into the right episode, film and quality tier.
- A SABnzbd style download client that writes a `.strm` link per grab, with the captions as `.srt` next to it.
- Tubarr pushes its own releases to Sonarr and Radarr, so they import new episodes without searching.
- Grabs of videos that cannot play fail at once, and dead releases are listed.

### Finding and matching uploads

- Channel feeds, channel lists, official playlists and a channel search backfill to find uploads, with Shorts, members-only videos and unfinished livestreams left out.
- Show matching by episode number, by air date for daily shows, by playlist position, by aliases and by the running number in podcast titles.
- Film matching on official channels by title, aliases, year and duration.
- Episodes and films split over several videos are joined into one release.
- Per-show rules, pins, a backfill limit for long back catalogues and IANA time zones per channel and mapping.
- Channels can be entered as `@handles`, channel links or video links, and Tubarr stores the channel id and name.
- An unclear match releases nothing and leaves a note with the reason.
- Mappings whose inputs have not changed since the last run are skipped, and unmatched notes are kept only for recent uploads.

### Playback

- `/play` serves a flat MP4 joined from YouTube's video and audio tracks by byte range, with no re-encode and no ffmpeg. Seeking works in Jellyfin and other players.
- Sample tables are cached on disk and read in the background after a grab, so the first play starts quickly.
- First plays of different videos read their sample tables side by side (`[play] max_builds`), and `[play] max_streams` caps the plays streaming at once.
- An audio only M4A per episode, kept as an Audio only version next to each imported episode and merged into it in Jellyfin.
- The YouTube thumbnail saved as each episode's image.
- `/play` serves only videos Tubarr released, pinned or grabbed.
- `/play` answers loopback and private networks unless `[auth] play_allowed_networks` says otherwise, and believes `X-Forwarded-For` only from the proxies in `[auth] trusted_proxies`.
- The play index, watched videos, grabs and SABnzbd history are kept within bounds, and Health shows how big each store is.

### Keeping YouTube lookups working

- One shared lookup budget with a cache, and a fallback to a PO token, then cookies, when YouTube pushes back.
- An hourly canary and a Health page that says why the status is not ok.
- yt-dlp and deno fetched from their GitHub releases, checked against the published sums, updated daily and rolled back when a new build fails. Managed builds exist for x86_64 and arm64, and other architectures get a clear Health message asking for your own binaries.

### Web UI and admin

- A web UI with six pages (Shows and films, Unmatched, Drafts, Activity, Health and Settings, which also manages users), with search, paging and phone layouts.
- A Drafts page lists the TheTVDB and TMDB drafts made so far, to check a title, draft a show or film, copy the text, draft again or delete a draft.
- The Activity page's searches, arr pushes and play starts are stored and survive a restart, kept for `[activity] retention` (30 days by default), and page on the server like the grabs.
- Sonarr and Radarr can be connected from Settings without a restart, with a check of each arr's setup.
- Built-in sign-in with passwords and OpenID Connect.
- Sign-in throttles and `local_bypass` judge the client named by a trusted proxy, guesses at one username from many addresses slow the answers instead of locking its owner out, and every answer carries security headers.
- The Lexend font ships inside the image, so the web UI loads nothing from Google.
- A fresh instance is claimed only with a one-time setup code from the log, or the one set in `TUBARR_SETUP_CODE`.
- Notices for canary trouble, cookies use, yt-dlp updates, grabs and a daily unmatched digest, as Discord cards or as JSON posted to a webhook of your choice.
- The running version and build commit at the top of the Health page and in `/api/health`.
- An optional Prometheus endpoint, `/metrics`, for lookups, plays, first byte times, the matcher, the arrs and memory, behind a token.
- A Docker image running as uid 1000, built for amd64 and arm64 and published to GHCR, tagged `X.Y.Z`, `X.Y` and `latest`.
- A demo mode with seeded data.
- Measured memory figures ([sizing page](https://h1n054ur.github.io/tubarr/operations/sizing/)) and a 1g memory limit in the compose example.
- A copy of the database before an upgrade migrates it, the newest 3 kept (`[backup]`), and a docs page on what to back up and how to go back to an older version ([backups and upgrades page](https://h1n054ur.github.io/tubarr/operations/backups-and-upgrades/)).
- Mapping export and import, with their rules, aliases and pins, after a dry run (Settings, or the admin API).
- An optional `[network]` proxy, set with `proxy` or `proxy_file` (http, https, socks5 or socks5h), for hosts whose own address YouTube answers with a bot check. It carries YouTube traffic only: yt-dlp, channel feeds, channel pages, thumbnails, the `/play` relay and captions. Sonarr, Radarr, Jellyfin, notices, sign-in, drafts lookups and the yt-dlp and deno downloads do not use it, and the proxy's user name and password are never shown.
- The clients that go straight out ignore the `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` environment variables, so a proxy that Docker sets for every container does not catch them.
- Errors in the config file name the line and the key and never quote the file, since the line may hold a password or an API key.

### Project

- CI checks the licences of Rust crates and npm packages and the RustSec advisories, and the image carries the third-party notices.
- Security and contributing guides.
- A Starlight docs site with install and quick start pages.
- Docs pages for configuration, Sonarr, Radarr, Prowlarr and Jellyfin, using Tubarr, operations, privacy, the admin API, the FAQ and the terms of use, plus a "Where to run it" page on home connections versus VPS hosts and a monitoring page.
