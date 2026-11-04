# Changelog

All notable changes to Tubarr are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow [semantic versioning](https://semver.org), on 0.x until 1.0 (see "Versions and releases" in the README).

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
- Measured memory figures in the README and a 1g memory limit in the compose example.
- A copy of the database before an upgrade migrates it, the newest 3 kept (`[backup]`), and a README section on what to back up and how to go back to an older version.
- Mapping export and import, with their rules, aliases and pins, after a dry run (Settings, or the admin API).

### Project

- CI checks the licences of Rust crates and npm packages and the RustSec advisories, and the image carries the third-party notices.
- Security and contributing guides.
- A Starlight docs site with install and quick start pages.
- Docs pages for configuration, Sonarr, Radarr, Prowlarr and Jellyfin, using Tubarr, operations, privacy, the admin API, the FAQ and the terms of use.
