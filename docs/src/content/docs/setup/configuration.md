---
title: Configuration
description: Every key in tubarr.toml, with its type, its default and what it does.
---

Tubarr reads one TOML file, `/config/tubarr.toml` by default. Only `public_url` is required to start, and every other key has the default listed below, so keep only the keys you change. A working install needs more than that: the `api_key_file` that Sonarr and Radarr use to reach `/api`, a downloads folder owned by 1000:1000 and mounted at the same path in Sonarr and Radarr, a memory limit (`mem_limit: 1g` in the Compose file), and the Sonarr or Radarr connections if you use them. See [Install](../../getting-started/install/). `config/tubarr.example.toml` in the repository holds the same keys with short comments.

A bad value, an unknown key or a secret file that cannot be read stops Tubarr at start, with a message naming the key, for example ``config key `rate_limit.burst`: must be at least 1``. Restart the container after editing the file: Tubarr reads it once, at start.

## How values are written

| Type | Written as | Notes |
|---|---|---|
| duration | `"90s"`, `"15m"`, `"1h"`, `"30d"`, `"180days"` | `"0s"` where a key says 0 turns something off |
| path | `"/config/bin"` | most folders must be absolute paths |
| URL | `"http://sonarr:8989"` | http or https, no user name, password, query or fragment. A missing trailing `/` is added |
| secret file | `"/run/secrets/sonarr_api_key"` | every key ending in `_file` names a file holding the value, read once at start. Spaces and line breaks around the value are dropped, an empty file is an error |
| network | `"192.168.1.0/24"`, `"fd00::/8"` | CIDR notation, IPv4 or IPv6 |

Secrets never go in `tubarr.toml` itself. With Docker Compose secrets they appear as `/run/secrets/<name>`. Without them, keep the files under `/config/secrets/` and point the `_file` keys there.

## Environment variables

| Variable | Default | What it does |
|---|---|---|
| `TUBARR_CONFIG` | `/config/tubarr.toml` | the config file to read, the same as the `--config` flag. A missing file counts as an empty one, which fails on `public_url` |
| `TUBARR_SETUP_CODE` | unset | the setup code for the first administrator, instead of a random one printed in the log. At least 12 letters or digits, spaces and dashes not counted. Tubarr never logs a code set this way. See [First run](../../getting-started/first-run/) |
| `RUST_LOG` | `info` | how much Tubarr logs: `warn`, `info`, `debug`, or per module like `info,tubarr::play=debug` |

## Top level

| Key | Type | Default | What it does |
|---|---|---|---|
| `public_url` | URL | none, required | how Jellyfin, Sonarr and Radarr reach Tubarr. It is written into every `.strm` file and every NZB link, so it must work from inside their containers. Use the Docker host's address or a name your network resolves, not `localhost`. A path is kept: `http://host/tubarr` gives `http://host/tubarr/play/<id>` |
| `listen` | address | `"0.0.0.0:8480"` | the address and port the HTTP server binds to |
| `data_dir` | path | `"/config"` | holds the SQLite database `tubarr.db` |
| `complete_dir` | path | `"/downloads/tubarr"` | absolute. Grabs write their `.strm` and `.srt` files here, and Tubarr reports it to Sonarr and Radarr as SABnzbd's completed folder, so they must see the same path in their containers |
| `cache_ttl` | duration | `"5h"` | how long a resolved stream lookup stays cached. More than 0 and at most `6h`, since YouTube's stream addresses expire after about six hours |
| `lookup_timeout` | duration | `"60s"` | a yt-dlp run is stopped after this long. More than 0 and at most `10m` |
| `api_key_file` | secret file | unset | the key Sonarr and Radarr send to `/api`, and scripts send as `X-Api-Key` to `/admin/api` and `/api/health`. Without it `/api` refuses every request |

## `[rate_limit]`

The shared budget for YouTube lookups: every stream lookup, channel list, playlist read and search takes a token from one bucket that refills with `lookups` tokens every `interval`. Plays go first.

| Key | Type | Default | What it does |
|---|---|---|---|
| `lookups` | number | `1` | tokens added every `interval`. At least 1 |
| `interval` | duration | `"10s"` | more than 0 |
| `burst` | number | `3` | lookups allowed back to back after a quiet spell. At least 1 |
| `jitter` | duration | `"3s"` | up to this much random delay is added to a lookup that has to wait. At most `interval` |
| `throttle_pause` | duration | `"15m"` | every lookup waits this long after YouTube answers with a 429 or a bot check. More than 0 and at most `24h` |
| `processes` | number | `1` | yt-dlp runs at once. Each one, with its deno, takes about 400 MB at its peak, so raise the container's memory limit with it (see [Sizing](../../operations/sizing/)). At least 1 |

## `[binaries]`

yt-dlp and deno are not in the image. Tubarr downloads both from their GitHub releases and keeps them current.

| Key | Type | Default | What it does |
|---|---|---|---|
| `bin_dir` | path | `"/config/bin"` | where Tubarr downloads and updates both binaries. It lives on the config volume, so updates need no new image |
| `yt_dlp` | path | unset | a yt-dlp installed some other way. Tubarr runs it as it is and never downloads or updates it |
| `deno` | path | unset | the same for deno |
| `yt_dlp_channel` | `"stable"` or `"nightly"` | `"stable"` | which yt-dlp release channel to follow. Nightly builds help when YouTube changes faster than releases come out |
| `check_interval` | duration | `"24h"` | least time between two update checks of the same binary. More than 0 |

The Health page shows both versions and has buttons to check for updates and to roll back to the previous version.

## `[watcher]`

How new uploads on the mapped channels are found. The lower limits keep a typo from flooding YouTube, which flags an address after a burst of requests.

| Key | Type | Default | What it does |
|---|---|---|---|
| `rss_interval` | duration | `"30m"` | how often each channel's RSS feed is read (plain HTTP, no lookup). At least `5m` |
| `rss_jitter` | duration | `"5m"` | random time added to each feed's next read. At most `rss_interval` |
| `rss_spacing` | duration | `"5s"` | least time between two feed reads. At least `1s` |
| `listing_interval` | duration | `"24h"` | how often each channel's full video list is read, spread over the interval. At least `1h` |
| `listing_size` | number | `100` | newest videos a full list reads, 1 to 1000. A channel's own `listing_size` wins |
| `fallback_listing_size` | number | `15` | newest videos a short list reads, 1 to 100. A channel whose feed has failed for longer than `rss_interval` is listed this way about every `rss_interval`, until a feed read works again |
| `fallback_per_hour` | number | `30` | most short lists an hour across all channels, spaced evenly. 0 turns them off, at most 120 |
| `playlist_interval` | duration | `"24h"` | how often each official playlist a mapping names is read. A new playlist goes within minutes. At least `1h` |
| `backfill_days` | number | `14` | days a show without a playlist gets a daily search of its channels for its title and aliases. 0 turns the search off, at most 365 |
| `backfill_interval` | duration | `"24h"` | least time between two searches for the same show. At least `1h` |
| `backfill_spacing` | duration | `"10m"` | least time between two shows' searches. At least `1m` |
| `backfill_size` | number | `400` | results one search reads, 1 to 500 |

## `[play]`

The `/play` endpoint Jellyfin reads.

| Key | Type | Default | What it does |
|---|---|---|---|
| `fragment_cache_bytes` | bytes | `100663296` (96 MiB) | YouTube fragments kept in memory, since probing and seeking ask for the same small ranges again and again. 8 MiB to 1 GiB. Counts toward the memory limit |
| `probe_bytes` | bytes | `65536` (64 KiB) | read from the start of each YouTube file to find its index. 4 KiB to 16 MiB |
| `index_dir` | path | `.play-index` inside `complete_dir` | absolute. The sample tables read from each video's fragments, a few hundred KB per video, so later plays start sooner. Safe to delete: the next play reads them again |
| `prebuild_tables` | true or false | `true` | read a grabbed video's sample tables in the background, so its first play starts sooner. Plays always go first |
| `index_max_bytes` | bytes | `2147483648` (2 GiB) | the most the sample tables may take. Above it the ones used longest ago are removed. 0 sets no cap |
| `index_max_idle` | duration | `"180days"` | a sample table not used for this long is removed. `"0s"` keeps them however old |
| `max_builds` | number | `2` | first plays that may read their sample tables at the same time. More wait for a turn, and plays of the same video share one read. 1 to 8 |
| `max_streams` | number | `8` | plays that may stream at the same time. One more answers 503 with `Retry-After: 5`. 0 sets no cap |

## `[subtitles]`

Captions written next to each `.strm` as `<release>.<code>.srt`. Sonarr and Radarr import them only with Import Extra Files on and `srt` in its list (see [Sonarr and Radarr](../sonarr-and-radarr/)).

| Key | Type | Default | What it does |
|---|---|---|---|
| `languages` | list of codes | `["en", "ur", "hi"]` | two-letter ISO 639-1 codes, each listed once. A file is written for each language YouTube has captions in |
| `automatic` | true or false | `true` | use YouTube's speech-to-text track in the spoken language when no uploaded captions exist. Never machine translations |
| `timeout` | duration | `"15s"` | the lookup and every download together, before the grab reports done. What is not in by then is skipped. More than 0 and at most `60s` |

## `[sonarr]` and `[radarr]`

Optional. Instead of a section here, a connection can be saved on the Settings page, where it is tested before it is kept. A section in this file wins over a saved connection, and the Settings page then shows it read only. With neither, Tubarr serves no titles from that app.

| Key | Type | Default | What it does |
|---|---|---|---|
| `url` | URL | none, required in the section | how Tubarr reaches the app, for example `http://sonarr:8989` |
| `api_key_file` | secret file | none, required in the section | a file holding the app's API key (Settings > General > Security) |

## `[arr]`

Which Sonarr series and Radarr films Tubarr serves, and how it hands them releases.

| Key | Type | Default | What it does |
|---|---|---|---|
| `tag` | text | `"youtube"` | the tag on the titles Tubarr serves, the same tag its indexer and download client carry. Stored in lower case |
| `sync_interval` | duration | `"15m"` | time between two reads of the tagged titles. The first read runs at start. At least `1m` |
| `auto_push` | true or false | `true` | push each episode or film Tubarr releases for the first time to the arr, if it still wants it. `auto_search` is the old name and is still read |
| `push_interval` | duration | `"5s"` | least time between two pushes to the same arr, one release per push. At least `1s` |
| `indexer` | text | `"Tubarr"` | the name of Tubarr's indexer in the arrs, sent with each push |
| `download_client` | text | `"Tubarr"` | the name of Tubarr's download client in the arrs. Every push is pinned to it, and without a client of this name nothing is pushed |
| `grab_retention` | duration | `"30days"` | grabs older than this leave the download history Sonarr and Radarr poll. Their files stay. `"0s"` keeps every grab |

## `[films]`

| Key | Type | Default | What it does |
|---|---|---|---|
| `channels` | list of channel ids | `[]` | official film channels (`UC...`) searched for every film, after the film's own channels |
| `min_duration` | duration | `"80m"` | shorter uploads are clips, songs or trailers. A mapping's own floor wins. At least `1m` |
| `similarity` | number | `0.9` | how close a title must be to the film's name or an alias, by edit distance, 0.5 to 1. Titles a little below are noted as unsure for a pin |

## `[channels.<channel id>]`

One table per official channel, by its channel id, for example `[channels.UC4JCksJF76g_MdzPVBJoC3Q]`. Older files with `[drafts.channels]` still load. Set only one of the two.

| Key | Type | Default | What it does |
|---|---|---|---|
| `network` | text | unset | the TV network the channel belongs to, used by the draft helper |
| `country` | text | `"Pakistan"` | used by the draft helper |
| `language` | text | `"Urdu"` | the original language of what the channel uploads, as an English name. A show whose language is still the arr's English or Unknown (Sonarr has no Urdu) takes its first channel's language for its release names. A language set in the web UI is never changed |
| `utc_offset_mins` | number | `300` | the channel's offset from UTC in minutes, to turn an upload time into an air date for daily shows |
| `timezone` | text | unset | an IANA time zone like `"America/Chicago"`. Wins over `utc_offset_mins` and follows daylight saving time |
| `root_folder` | path | unset | the Sonarr or Radarr root folder the channel's titles live in, and so their Jellyfin library |
| `show_root_folder` | path | unset | the same for shows only, over `root_folder` |
| `film_root_folder` | path | unset | the same for films only, over `root_folder` |
| `listing_size` | number | unset | over `[watcher] listing_size` for this channel, for a channel that uploads dozens of videos a day |
| `assumed_height` | number | unset | over `[matcher] assumed_height` for this channel, for one that uploads below 1080p. 0 waits for a lookup |

## `[root_folders]`

Root folders by language, for titles whose first channel sets none. Language names match the mapping's language in any case.

```toml
[root_folders.shows]
Urdu = "/data/media/pk-dramas"
Hindi = "/data/media/in-dramas"

[root_folders.films]
Hindi = "/data/media/in-films"
```

| Key | Type | Default | What it does |
|---|---|---|---|
| `shows` | language to folder | empty | for shows and daily shows |
| `films` | language to folder | empty | for films |

After each sync, Tubarr asks Sonarr or Radarr to move a title whose root folder differs from the wanted one, with its files, once per wanted folder. The folder must already exist in the arr. A mapping's own root folder in the web UI wins, and `none` turns the moves off for it. [Sonarr and Radarr](../sonarr-and-radarr/#root-folders-per-language) has the details.

## `[matcher]`

| Key | Type | Default | What it does |
|---|---|---|---|
| `interval` | duration | `"30m"` | a run also starts after each watcher find and each arr sync. This is the fallback. At least `1m` |
| `lookups_per_run` | number | `3` | lookups one run makes for videos whose duration is still unknown, from the same budget as plays. At most 20 |
| `assumed_height` | number | `1080` | a video with a known duration is released at once under this height. A later grab or play looks it up and the real height replaces it (same guid). 0 waits for a lookup instead. At most 1080 |
| `note_retention` | duration | `"180days"` | unmatched notes are kept for videos uploaded within this long. Older ones are dropped, except notes waiting on a lookup. `"0s"` keeps every note |

## `[activity]`

The searches, arr pushes and play starts the Activity page lists. They are stored in `tubarr.db` (see [Privacy](../../about/privacy/#what-tubarr-keeps)).

| Key | Type | Default | What it does |
|---|---|---|---|
| `retention` | duration | `"30days"` | events older than this are removed by housekeeping. `"0s"` keeps every event |

## `[newznab]`

| Key | Type | Default | What it does |
|---|---|---|---|
| `report_size` | true or false | `false` | send each release's size. Off, Sonarr and Radarr skip their size check, which uses a global table that expects far bigger files than YouTube's (a 38 minute 1080p episode of 420 MB fails a 570 MB minimum) |

## `[fallback]`

Optional, all off by default. These steps are tried in order only after a plain lookup gets a 429, a bot check or a sign-in request.

| Key | Type | Default | What it does |
|---|---|---|---|
| `po_token_url` | URL | unset | a bgutil PO token HTTP server, for example `http://bgutil:4416` |
| `plugin_dir` | path | unset | absolute. The folder with the bgutil yt-dlp plugin, the same version as the server. The image ships it in `/opt/yt-dlp-plugins`. Only with `po_token_url` |
| `cookies_file` | path | unset | absolute. A Netscape cookies file of a spare YouTube account, the last resort. Never written back: each lookup gets a temporary copy, and each use logs a warning |
| `sticky_for` | duration | `"1h"` | after the PO token step works, lookups start there for this long before trying plain again. The cookies step is never sticky. More than 0 and at most `24h` |
| `min_height` | number | `720` | a fallback answer counts only with H.264 video at least this tall. Plain answers are taken as they are. At most 1080 |

## `[canary]`

A lookup of a known good video every so often, so trouble shows before a play fails. Off without `video_id`.

| Key | Type | Default | What it does |
|---|---|---|---|
| `video_id` | text | unset | an 11 character YouTube video id |
| `interval` | duration | `"1h"` | at least `10m`. Three failures in a row make Tubarr update yt-dlp, at most once a day |
| `jitter` | duration | `"10m"` | random time added to each wait. At most `interval` |

## `[notify]`

Optional notices: the canary failing and recovering, lookups that used the cookies file, yt-dlp and deno updates, grabs (at most one notice per 10 minutes) and a daily count of unmatched uploads. Set a Discord webhook, or a JSON webhook with its token, not both.

| Key | Type | Default | What it does |
|---|---|---|---|
| `discord_webhook_file` | secret file | unset | a file holding a Discord channel webhook URL, `https://discord.com/api/webhooks/<id>/<token>`. Tubarr builds the Discord message itself |
| `card_relay_url` | URL | unset | any HTTP endpoint that takes a JSON `POST`. Tubarr posts each notice there as JSON, described below, and the endpoint decides what to do with it. No user name or password in the URL |
| `card_relay_token_file` | secret file | unset | the token sent to that endpoint as `Authorization: Bearer <token>`. Required with `card_relay_url` |

The JSON body of each notice looks like this:

```json
{"source": "tubarr", "event": "canary_failing", "tone": "failure",
 "title": "YouTube lookups are failing", "status": "3 failed checks in a row",
 "text": ["The hourly test lookup of a known good video keeps failing, so plays will likely fail too."],
 "fields": [{"name": "Why", "value": "YouTube asked for a sign-in check"}],
 "lists": [{"name": "Releases", "items": ["..."]}],
 "footer": "Tubarr · canary", "at": 1791499315, "username": "Tubarr"}
```

`event` is one of `canary_failing`, `canary_recovered`, `cookies_used`, `binary_updated`, `binary_update_failed`, `grabs` and `unmatched`. `tone` is `success`, `failure`, `warning` or `info`, and `at` is Unix seconds. `status`, `text`, `fields` and `lists` are left out when empty. Values such as release names and error messages are sent as they are, so the endpoint should escape them before it shows them. A notice is tried again after a server error or a network error, waits out a 429, and is dropped after any other refusal. Each notice gets up to four tries. If 32 are already waiting to be sent, a new one is dropped.

## `[auth]`

Sign-in for the web UI and `/admin/api`. `/api` keeps its API key, `/healthz` is open, and `/play` is limited by network.

| Key | Type | Default | What it does |
|---|---|---|---|
| `method` | text | `"forms"` | `"forms"` (username and password), `"oidc"` or `"forms+oidc"` |
| `session_ttl` | duration | `"30d"` | a session ends after this long without a request. At least `5m` |
| `session_max_age` | duration | `"90d"` | and this long after sign-in, however active. At least `session_ttl`, at most 400 days |
| `cookie_secure` | true or false | unset | the `Secure` flag on the session cookie. Unset: on when `ui_url` (or `public_url`) is https |
| `local_bypass` | true or false | `false` | clients on loopback or a private network skip sign-in. The client is the TCP peer, or, when the peer is in `trusted_proxies`, the address that proxy names in `X-Forwarded-For`. A trusted proxy is never let in as itself. Behind a reverse proxy that is not in `trusted_proxies`, every request looks local, so keep it off there |
| `ui_url` | URL | `public_url` | where people open the web UI, when it is not `public_url`. The OpenID Connect redirect is built from it |
| `play_allowed_networks` | list of networks | loopback and private ranges | who may fetch `/play`. Unset or empty: `127.0.0.0/8`, `::1/128`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `100.64.0.0/10`, `fc00::/7`, `fe80::/10` and `169.254.0.0/16`. `["0.0.0.0/0", "::/0"]` allows everyone |
| `trusted_proxies` | list of networks | `[]` | reverse proxies whose `X-Forwarded-For` header names the client. The right-most address in it that is not itself a trusted proxy counts. That client address decides `play_allowed_networks`, `local_bypass` and the sign-in and setup code throttles, so each real client behind the proxy is judged on its own |

`/play` also answers only for videos Tubarr released, pinned or grabbed. Any other video id gets a 404, so the endpoint cannot be used to relay arbitrary YouTube videos. [Jellyfin](../jellyfin/#who-may-fetch-play) explains both limits.

### `[auth.oidc]`

OpenID Connect sign-in (Authentik, Keycloak and the like). Required when `method` includes `oidc`, refused otherwise. Register the redirect `<ui_url>/auth/oidc/callback` with the provider.

| Key | Type | Default | What it does |
|---|---|---|---|
| `issuer_url` | text | none, required | exactly as the provider's `iss`, trailing slash included |
| `client_id` | text | none, required | |
| `client_secret_file` | secret file | none, required | |
| `scopes` | list | `["openid", "profile", "email", "groups"]` | must include `openid` |
| `allowed_groups` | list | `[]` | only members of these groups get in. Empty lets in anyone the provider signs in |
| `groups_claim` | text | `"groups"` | the ID token claim that lists the groups |
| `button_label` | text | `"Sign in with SSO"` | the text on the sign-in button |
| `auto_create_users` | true or false | true when `allowed_groups` is set, else false | create a user on their first sign-in |

## `[hooks]`

Optional. Sonarr's Connect webhook at `/hooks/sonarr`, which keeps the audio only versions and the episode images of mappings that turn them on. Off without `sonarr_token_file`.

| Key | Type | Default | What it does |
|---|---|---|---|
| `sonarr_token_file` | secret file | unset | the token Sonarr sends as `Authorization: Bearer` |
| `library_paths` | list of path pairs | `[]` | `[as Sonarr sees it, as Tubarr sees it]` pairs, for example `[["/mnt/media", "/media"]]`. Required with `sonarr_token_file`. Tubarr only touches files under these |

## `[drafts]`

Optional keys for the draft helper, which checks TheTVDB and TMDB for a title that is missing there. Its searches are read-only.

| Key | Type | Default | What it does |
|---|---|---|---|
| `tvdb_api_key_file` | secret file | unset | a TheTVDB v4 project API key |
| `tvdb_pin_file` | secret file | unset | the subscriber PIN, needed only with a user-supported TheTVDB key. Only with `tvdb_api_key_file` |
| `tmdb_read_token_file` | secret file | unset | a TMDB API read access token (the long one) |

## `[jellyfin]`

Optional. Merges each audio only file into its episode as a version (Jellyfin 10.11 does not group TV episode versions by name). Off without this section.

| Key | Type | Default | What it does |
|---|---|---|---|
| `url` | URL | none, required in the section | for example `http://jellyfin:8096` |
| `api_key_file` | secret file | none, required in the section | an API key from Dashboard > API Keys. Merging versions is an admin call, and Jellyfin's API keys have admin rights |
| `user_id` | text | the first administrator | the user Tubarr asks for an episode's playback info as. A Jellyfin user id, as in the user's address under Dashboard > Users |
| `library_paths` | list of path pairs | `[]` | `[as Tubarr sees it, as Jellyfin sees it]` pairs, when Jellyfin mounts the library elsewhere |
| `merge_retry` | duration | `"30s"` | the first wait for Jellyfin's scan, doubled each time. More than 0 and at most `1h` |
| `merge_attempts` | number | `7` | looks before a merge is given up on, about 30 minutes with the default wait. 1 to 12 |

## `[backup]`

A copy of the database taken before an upgrade changes its layout, so the upgrade can be undone (see [Backups and upgrades](../../operations/backups-and-upgrades/)). The copy is made only when the new version has database changes to apply, and lands next to the database as `tubarr.db.pre-<from>-to-<to>.<UTC time>.bak`.

| Key | Type | Default | What it does |
|---|---|---|---|
| `before_migrations` | true or false | `true` | take the copy. `false` upgrades without one |
| `keep` | number | `3` | copies kept, the newest first. Older ones are removed. At least 1 |

## `[metrics]`

`GET /metrics`, counters and gauges in the Prometheus text format. Off by default, and answers 404 while off.

| Key | Type | Default | What it does |
|---|---|---|---|
| `enabled` | true or false | `false` | serve `/metrics` |
| `token_file` | secret file | unset | the token a scraper sends as `Authorization: Bearer <token>`. Without it the API key from `api_key_file` is the token. With neither, only clients `[auth] play_allowed_networks` lets through may read it |

[Monitoring](../../operations/monitoring/) has a Prometheus scrape config, every metric and some useful queries.

A missing or wrong token answers 401. A client outside the allowed networks, when no token is set, gets 403. The labels never hold video ids, titles or paths.

## `[network]`

An outbound proxy for YouTube traffic, off by default. YouTube often answers cloud and VPS addresses with a bot check, so a host there can send Tubarr's YouTube requests out through a network it is allowed to use instead. When set, every request toward YouTube and Google's video hosts goes through it: yt-dlp gets it in its environment (`HTTPS_PROXY`, `ALL_PROXY` and the like, never on its command line), and so do the channel feeds, channel page lookups, episode thumbnails and the `/play` media and caption fetches. Sonarr, Radarr, Jellyfin, the notify webhook, OpenID Connect sign-in, the drafts lookups (through Sonarr and Radarr, and TVDB and TMDB directly when their keys are set) and the yt-dlp and deno downloads from GitHub never use it.

| Key | Type | Default | What it does |
|---|---|---|---|
| `proxy` | URL | unset | the proxy, for example `socks5h://10.0.0.2:1080` or `http://proxy:3128`. `http://`, `https://`, `socks5://` and `socks5h://` work |
| `proxy_file` | secret file | unset | a file holding the same URL, for a proxy that needs a user name and password, like `socks5h://user:password@10.0.0.2:1080` |

With `socks5://` Tubarr's own requests look up YouTube's host names on this host and hand the proxy an address (yt-dlp treats `socks5://` as `socks5h://` either way). With `socks5h://` the proxy looks them up itself. Use `socks5h://` unless the proxy cannot resolve names.

Set one of the two, not both. A URL that does not parse, or a scheme not in the list, stops Tubarr at start. The user name and password never appear in the log, on the Health page or in error messages: those show only the scheme, host and port. With a proxy set, `GET /api/health` lists it as `outbound_proxy`. yt-dlp still reaches `localhost`, `127.0.0.1`, `::1` and the host of `[fallback] po_token_url` directly.

Tubarr ignores the `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` environment variables (which Docker can set for every container) for Sonarr, Radarr, Jellyfin, the notify webhook, OpenID Connect sign-in and the drafts lookups, so those always go direct. The yt-dlp and deno downloads from GitHub follow those variables, and so does the YouTube traffic while `[network]` is unset.

## Keys for tests only

A few keys point Tubarr at other servers and exist only for the test suite: `[binaries] github_api`, `[watcher] feed_url`, `[hooks] thumbnail_url`, `[drafts] tvdb_url` and `[drafts] tmdb_url`. Leave them unset.

## Not in this file

Show, daily show and film mappings, their channels and rules, aliases, pins and users live in the database, not in `tubarr.toml`. Edit them in the web UI or through the admin API.
