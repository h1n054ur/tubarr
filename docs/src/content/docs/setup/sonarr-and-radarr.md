---
title: Sonarr and Radarr
description: Connecting Sonarr and Radarr to Tubarr as an indexer and a download client.
---

To Sonarr and Radarr, Tubarr looks like two things at once: a Newznab indexer that offers releases, and a SABnzbd download client that "downloads" them. A grab writes a small `.strm` link file and the captions instead of a video, and the arr imports those like any download. A tag ties it together: only titles with the tag are served by Tubarr, and Tubarr's indexer and client only serve those titles.

The steps are the same in Sonarr and Radarr. Where a value differs, both are given. Every form below has a Test button, which should pass before you save.

## Tell Tubarr about the arr

The recommended way is Tubarr's Settings page. Under Connections, each arr has a URL field and an API key field. The API key is in each arr under Settings > General > Security. Press **Test and save**: Tubarr tests the URL and key against the arr's system status before it saves them in its database, never shows the key again, and starts using them at once. The arr sync runs right after a save. **Remove** forgets a saved connection, and Tubarr stops asking that arr. No restart and no secret file are needed.

The alternative is the config file. Add the arr to `tubarr.toml`, put its API key in a secret file (see [Install](../../getting-started/install/#docker-compose) for the Compose secrets), and restart Tubarr:

```toml
[sonarr]
url = "http://sonarr:8989"
api_key_file = "/run/secrets/sonarr_api_key"

[radarr]
url = "http://radarr:7878"
api_key_file = "/run/secrets/radarr_api_key"
```

An arr set in the config file wins over one saved on the Settings page, and the page shows it as read only. Either way, **Test Sonarr** and **Test Radarr** on the Settings page check the connection.

Tubarr reads the tagged titles at start and then every 15 minutes (`[arr] sync_interval`), and keeps one mapping per title, with Sonarr's alternate titles as aliases.

## 1. The tag

Settings > Tags (or the tag field on any form), add a tag named `youtube`. The name is `[arr] tag` in Tubarr's config. Arrs store tags in lower case, so keep it lower case.

## 2. Import extra files

Settings > Media Management, show the advanced settings, then under Importing:

- Import Extra Files: on
- Import Extra Files list: `srt`

Without this the arr imports the `.strm` but leaves the captions behind.

## 3. Root folders

Settings > Media Management > Root Folders. Jellyfin libraries are folders, so titles you want in their own library (Pakistani dramas apart from English shows, for example) need their own root folder in the arr. Add every folder you plan to use. Tubarr never creates one. [Root folders per language](#root-folders-per-language) below explains how Tubarr can move titles there for you.

## 4. The indexer

Settings > Indexers > + > Newznab (under Usenet). Show the advanced settings.

| Field | Sonarr | Radarr |
|---|---|---|
| Name | `Tubarr` | `Tubarr` |
| Enable RSS | on | on |
| Enable Automatic Search | on | on |
| Enable Interactive Search | on | on |
| URL | your `public_url`, for example `http://192.168.1.20:8480` | the same |
| API Path | `/api` | `/api` |
| API Key | the key in Tubarr's `api_key_file` | the same |
| Categories | `5000`, `5040` (TV, TV HD) | `2000`, `2040` (Movies, Movies HD) |
| Tags | `youtube` | `youtube` |

Leave the rest at its defaults. The name must match `[arr] indexer` (default `Tubarr`): Tubarr sends it with every release it pushes.

Add the download client in the next step first, then come back and set the indexer's **Download Client** field (an advanced setting in current Sonarr and Radarr versions) to `Tubarr`. Releases Tubarr pushes are already pinned to its client, but a grab from the arr's own RSS sync or search picks a client by tag and priority. If you also run a real SABnzbd without tags, the arr could otherwise send a Tubarr release there, where it fails.

## 5. The download client

Settings > Download Clients > + > SABnzbd. Show the advanced settings.

| Field | Sonarr | Radarr |
|---|---|---|
| Name | `Tubarr` | `Tubarr` |
| Enable | on | on |
| Host | the host in your `public_url`, for example `192.168.1.20` | the same |
| Port | `8480`, or the port in `public_url` | the same |
| Use SSL | off, unless `public_url` is https | the same |
| URL Base | empty, or the path in `public_url` if it has one | the same |
| API Key | the key in Tubarr's `api_key_file` | the same |
| Username, Password | empty | empty |
| Category | `tv` | `movies` |
| Tags | `youtube` | `youtube` |

The name must match `[arr] download_client` (default `Tubarr`). Tubarr pins every release it pushes to the client of that name, so the arr never hands a Tubarr release to your real download client, and without a client of that name nothing is pushed.

Grabs land in `complete_dir/tv` and `complete_dir/movies` (`/downloads/tubarr/tv` with the defaults), and Tubarr reports the job as completed at once. The arr must see that folder at the same path as Tubarr does, since it imports from the path Tubarr reports.

## 6. Quality profiles and languages

Releases are named the way scene releases are, so the arrs parse them without custom formats:

- `Show.Name.S01E05.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR` for a numbered episode
- `Show.Name.2026.10.14.720p.WEB-DL.AAC2.0.H.264.URDU-TUBARR` for a daily show
- `Theri.2016.1080p.WEB-DL.AAC2.0.H.264.HINDI-TUBARR` for a film

The resolution is the standard tier for the frame (a cinematic 1920x804 is `1080p`), and Tubarr serves 1080p at most. The quality is always WEB-DL.

**Sonarr.** Any profile that allows WEB-DL at the heights Tubarr serves works, for example the built-in `Any` or `HD-1080p`. Sonarr has no Urdu, so it reads `URDU` as an unknown word and falls back to the series language. Nothing is rejected for it.

**Radarr.** Radarr's quality profiles also carry a language. A dubbed upload carries its dub's language (`HINDI` above for a Tamil film), so with the profile set to the film's original language Radarr would refuse it. Give the films Tubarr serves a profile with Language set to `Any`. The end-to-end test copies `Any` into a new profile named `YouTube` with Language `Any` for this.

You need no custom formats for Tubarr. If your profiles score custom formats with a minimum score above 0, check that a `-TUBARR` WEB-DL release still reaches it.

Release sizes are left out of the feed on purpose (`[newznab] report_size`), because the arrs' global size table expects far bigger files than YouTube's and would refuse most episodes.

## 7. Check the setup

On Tubarr's Settings page, press **Check Sonarr setup** or **Check Radarr setup**. Tubarr reads the arr and lists each thing it needs there, as In place, Missing or Optional, with what to change:

- the tag (`[arr] tag`)
- a Newznab indexer named like `[arr] indexer`, with the tag
- a SABnzbd download client named like `[arr] download_client`, enabled and with the tag
- Import Extra Files on, with `srt` in its list
- Sonarr only: a webhook to `hooks/sonarr`, optional unless `[hooks] sonarr_token_file` is set (see [The Sonarr webhook](#the-sonarr-webhook))

The check is read only: it never changes anything in the arr.

## 8. Add a title

Add the series or film as usual, monitored, with the `youtube` tag and the profile above. After the next sync it appears on Tubarr's Shows and films page. Give it its official channels there, and Tubarr starts looking for uploads (see the [Quick start](../../getting-started/quick-start/)).

## How releases reach the arr

The arrs' RSS sync reads only an indexer's newest releases. A back episode found by a search or a playlist, or a film added without a search, would never be grabbed on its own, and an arr search asks every indexer it has. So Tubarr does not wait for the arr: with `[arr] auto_push` on (the default) it hands each episode or film it releases for the first time to the arr through the arr's release push API.

A push goes out only while the arr has the episode or film monitored and without a file. It carries Tubarr's indexer name and is pinned to the Tubarr download client. The arr then runs its usual checks (quality, profile, delay) and grabs it or not. Pushes go out one at a time, at most one per `push_interval` (5 seconds) per arr. A push the arr could not take because it was down, or answered with a 5xx or 429, is tried again later.

Tubarr's Activity page lists each push as grabbed, pending, rejected (with the arr's reasons) or failed. Its **Push released** button is the catch-up: it queues every episode and film Tubarr has a release for, and skips what the arr already has or does not monitor.

The arr's own searches and RSS sync still work too: Tubarr's indexer answers them from the same releases.

## Root folders per language

Tubarr can keep each title in the right root folder, and so the right Jellyfin library. A title's wanted folder is, in this order:

1. the mapping's own root folder, set in the web UI (`none` turns the moves off for it)
2. its first channel's folder in `[channels]` (`show_root_folder` or `film_root_folder`, else `root_folder`)
3. the `[root_folders]` entry for its language

```toml
[channels.UC4JCksJF76g_MdzPVBJoC3Q]
language = "Urdu"
root_folder = "/data/media/pk-dramas"

[root_folders.films]
Hindi = "/data/media/in-films"
```

After each sync, and when a mapping's channels, language or folder change, Tubarr asks the arr to move a title whose root folder differs, with its files, the same way the arr's own editor does. It asks once per wanted folder, so a move you undo in the arr stays undone until the wanted folder changes. A folder the arr does not have is logged as a warning and shown as `missing` on the mapping.

## The Sonarr webhook

Only needed for two optional features of show and daily show mappings: **Audio only version**, which writes a second link file holding the episode's audio alone, and **Episode images from YouTube**, which saves the YouTube thumbnail next to each episode. Sonarr tells Tubarr about imports, renames and deletes through a webhook, and Tubarr keeps those files in step.

Make a token for the webhook and add the `[hooks]` section:

```sh
openssl rand -hex 32 > secrets/sonarr_hook_token
```

```toml
[hooks]
sonarr_token_file = "/run/secrets/sonarr_hook_token"
library_paths = [["/mnt/media", "/media"]]   # [as Sonarr sees it, as Tubarr sees it]
```

Tubarr needs the media library mounted read-write, and `library_paths` maps the paths Sonarr reports onto Tubarr's mount. A path under none of the pairs is refused and nothing changes. Tubarr only ever removes or overwrites a file that holds one of its own links.

Then in Sonarr, Settings > Connect > + > Webhook:

| Field | Value |
|---|---|
| Name | `Tubarr` |
| Notification triggers | On Import, On Upgrade, On Rename, On Episode File Delete, On Series Delete. On Episode File Delete For Upgrade may stay on too |
| Tags | `youtube`, so other series never call it |
| URL | your `public_url` followed by `hooks/sonarr`, for example `http://192.168.1.20:8480/hooks/sonarr` |
| Method | POST |
| Headers | `Authorization` with the value `Bearer <token>`, the token in `sonarr_token_file` |

Sonarr's Test button sends a test event, which Tubarr answers with 200. Radarr needs no webhook: both features are for TV episodes.

[Jellyfin](../jellyfin/#audio-only-versions) covers how the audio only versions show up there, and [Podcasts](../../using/podcasts/) when to turn the features on.

## Prowlarr

Add Tubarr to Sonarr and Radarr directly, as above, not through Prowlarr. [Prowlarr](../prowlarr/) explains why.
