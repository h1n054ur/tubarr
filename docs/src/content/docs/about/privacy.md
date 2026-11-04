---
title: Privacy
description: What Tubarr stores and what it sends where.
---

Tubarr runs on your server and answers to you alone. It has no telemetry, no analytics and no account with its authors, and it sends nothing to them. This page lists what it keeps and every service it talks to, as the code does it.

## What Tubarr keeps

Everything lives on your server, in the folders you mount.

**The database** (`tubarr.db` in `/config`) holds:

- your mappings: the titles Sonarr and Radarr tag, with their TVDB, TMDB and IMDb ids, the rules you set, aliases, pins and ignored notes
- what the watcher saw on the channels you mapped: video ids, titles, lengths, upload dates and picture sizes, and the playlists you named
- the releases Tubarr wrote, the unmatched notes with their reasons, and the list of videos `/play` may serve
- the download history: each grab's release name, video id, category and folder, kept for 30 days by default (`[arr] grab_retention`)
- the Activity page's events, kept for 30 days by default (`[activity] retention`): the searches Sonarr and Radarr made (their search terms, like `tvdbid=452001 season=1 ep=3`, and the number of results), the pushes to the arrs with their outcome and the arr's reasons, and each play start with its video ids, the player's User-Agent string and the status Tubarr answered. Play starts hold no IP address
- the connections to Sonarr and Radarr you save on the Settings page, API key included
- the stream part of recent YouTube lookups, for at most `cache_ttl` (5 hours by default)
- drafts you made with the TheTVDB and TMDB helper
- users: username, display name, an argon2id hash of the password (never the password), the OpenID Connect issuer and subject for single sign-on users, and when each last signed in
- sessions: only a SHA-256 hash of each session cookie, never the cookie itself

**Files**: the `.strm` link files and `.srt` captions in `complete_dir` (which the arrs then move into your library), episode images and audio only link files next to imported episodes when you turn those on, the sample tables in `[play] index_dir`, and yt-dlp and deno in `/config/bin`.

**The log** goes to the container's standard output. At the default `info` level it records syncs, releases, grabs, pushes and warnings. At `debug` it also records video ids and the addresses of refused plays. Tubarr keeps no log file of its own: how long the log is kept is up to Docker's logging setup.

Tubarr stores no record of who watched what beyond the play starts above, which name the player's User-Agent but not the person or their address, and are removed after `[activity] retention`. Set it lower to keep them for less time. Jellyfin keeps its own watch history, as for any library.

## What Tubarr sends, and where

### YouTube

Tubarr has to ask YouTube for the videos it serves. YouTube sees your server's public address, and:

- the handles and links you type for a channel, when Tubarr reads the YouTube page they name to find the channel id
- the channels you mapped, when Tubarr reads their RSS feeds (sent with the User-Agent `tubarr/<version>`) and their upload lists and searches through yt-dlp
- the playlists you named
- every video Tubarr looks up, and the byte ranges it streams when someone plays an episode
- caption downloads, and the episode thumbnails from `i.ytimg.com` when episode images are on

Your viewers' addresses are not sent to YouTube: Tubarr fetches the video and passes it on, so YouTube only sees the server.

With `[fallback] cookies_file` set, lookups that fall through to the cookies step are made as the signed-in account in that file, and YouTube can tie them to it. Use a spare account (see [Troubleshooting](../../operations/troubleshooting/#youtube-lookups-fail)). With `po_token_url` set, yt-dlp asks your own bgutil PO token server for a token, which then goes to YouTube with the lookup.

### GitHub

Once a day Tubarr asks GitHub's API for the newest yt-dlp and deno releases and downloads them from GitHub when there is a new one, with the User-Agent `tubarr/<version>`. GitHub sees your server's address and Tubarr's version. Pointing `[binaries]` at copies you install yourself turns this off.

### Sonarr and Radarr

Your own servers. Tubarr reads the tagged series and films, their episodes and files, and its own indexer and download client, pushes releases, and asks them to move a title to another root folder when you route titles by language. Its draft helper asks Sonarr's and Radarr's own lookups for a title, which they answer from TheTVDB and TMDB.

### Jellyfin

Your own server, only with a `[jellyfin]` section. Tubarr reads the episode lists of the series it serves, asks for an episode's playback info, and merges or splits the audio only versions.

### TheTVDB and TMDB

Only when you set keys in `[drafts]`. The draft helper then searches TheTVDB or TMDB for a title you are checking, read-only. Tubarr makes no other calls to either. The episode titles used for podcasts come from Sonarr.

### Optional services

| Service | When | What it gets |
|---|---|---|
| Discord, or the JSON webhook you set in `card_relay_url` | with `[notify]` set | notices about canary failures and recoveries, cookies use, yt-dlp and deno updates, grabs (with the release names) and a daily count of unmatched uploads by state. The JSON webhook also gets the token from `card_relay_token_file` (see [Configuration](../../setup/configuration/#notify)) |
| Your OpenID Connect provider | with `[auth.oidc]` set | the usual sign-in exchange: Tubarr gets the user's subject, username, name and groups |
| Your bgutil PO token server | with `[fallback] po_token_url` set | yt-dlp's token requests for the videos it looks up |

Nothing else. Sonarr, Radarr, Jellyfin and your players reach Tubarr, not the other way round, except for the calls above.
