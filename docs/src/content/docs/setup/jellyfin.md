---
title: Jellyfin
description: Libraries, playback, remote viewers and audio only versions in Jellyfin, and which media servers work.
---

Tubarr is built and tested with Jellyfin 10.11. Jellyfin needs no plugin: it plays the `.strm` files Sonarr and Radarr import like any other episode or film.

## How playback works

A `.strm` file is a text file holding one URL. Each one Tubarr writes holds `<public_url>/play/<video id>`, for example `http://192.168.1.20:8480/play/dQw4w9WgXcQ`. When someone presses play, the URL is fetched and Tubarr answers with one MP4 file, built on the fly from YouTube's separate video and audio streams. Nothing is re-encoded and nothing is stored. Seeking works like in any MP4, with byte ranges.

Who fetches the URL depends on the client:

- Many clients ask Jellyfin to fetch it and stream the result to them. Then only Jellyfin needs to reach Tubarr.
- Some clients get the URL from Jellyfin and fetch it themselves. Then the device you watch on needs to reach Tubarr too.

On your home network both work, as long as `public_url` is an address your devices can reach. Outside it, the second kind fails, which is what [Viewers outside your network](#viewers-outside-your-network) is about.

## Libraries

Point Jellyfin libraries at the folders where Sonarr and Radarr keep their titles, as Jellyfin sees them. Jellyfin does not need Tubarr's downloads folder.

| Content | Library type | Folder |
|---|---|---|
| Shows and daily shows | Shows | the Sonarr root folder(s) Tubarr's shows live in |
| Films | Movies | the Radarr root folder(s) |
| Video podcasts | Shows | their own Sonarr root folder, see [Podcasts](#podcasts) |

The default metadata settings work: Jellyfin identifies Tubarr's titles the same way as any other title Sonarr and Radarr manage. The captions Tubarr writes, `<release>.<code>.srt`, are picked up as external subtitles.

A library is a set of folders, so content you want apart (dramas in one language apart from your English shows, for example) needs its own root folder in the arr. Tubarr can move each title to the right root folder for you: see [Root folders per language](../sonarr-and-radarr/#root-folders-per-language).

After Sonarr imports a new episode, Jellyfin shows it at its next library scan, or at once with real time monitoring on the library.

## Who may fetch /play

Jellyfin fetches `/play` as a plain URL, so `/play` takes no sign-in. Two limits keep it from being an open relay:

- **Only released videos.** `/play` answers only for videos Tubarr released, pinned or grabbed. Any other video id gets a 404 before Tubarr asks YouTube anything.
- **Only allowed networks.** `[auth] play_allowed_networks` lists who may fetch it. Left unset, that is loopback and the private ranges: `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `100.64.0.0/10` (Tailscale and NetBird), IPv4 link-local `169.254.0.0/16`, and IPv6 unique local (`fc00::/7`) and link-local (`fe80::/10`) addresses. Anyone else gets a 403.

The default fits most homes: Jellyfin in a Docker network, and devices on your LAN, all have private addresses. Tubarr logs a warning at start while the default is in use, as a reminder to set your own list:

```toml
[auth]
play_allowed_networks = ["192.168.1.0/24", "172.18.0.0/16"]   # your LAN and Jellyfin's Docker network
```

Set it to the networks Jellyfin and your LAN clients fetch from. If Jellyfin runs on a server with a public address, add that address. `["0.0.0.0/0", "::/0"]` allows everyone, which we don't recommend.

If a reverse proxy sits between Jellyfin and Tubarr, every request comes from the proxy's address. List the proxy in `[auth] trusted_proxies` and Tubarr judges the client named in its `X-Forwarded-For` header instead:

```toml
[auth]
trusted_proxies = ["172.18.0.10/32"]
```

Only list proxies you run. Tubarr believes their `X-Forwarded-For` header and nobody else's.

## Viewers outside your network

:::caution
This is the setting most people miss on the first day. Without it, viewers outside your home network get an error on every Tubarr episode, while everything works at home.
:::

Your `public_url` is usually a private address like `http://192.168.1.20:8480`. A client that fetches the `.strm` URL itself (see [How playback works](#how-playback-works)) cannot reach that address from outside your network, and the play fails, even though Jellyfin itself is reachable.

The fix is a per-user setting in Jellyfin. In Jellyfin, open Dashboard, Users, pick the user, and under Media Playback turn on Force transcoding of remote media sources. Save, then have them start the episode again.

With it on, Jellyfin never hands that user's client the URL. It fetches `/play` itself and streams the result to the client, the same way it relays live TV. Since Tubarr's streams are already H.264 video with AAC audio, Jellyfin can usually pass them through with little work. It still runs them through ffmpeg, and transcodes for real when the client cannot play H.264 or the user's bitrate limit is lower, so the Jellyfin server needs the usual transcoding setup.

The setting applies to the user wherever they watch, at home too. Turn it on for each user who ever watches from outside, and leave it off for the others.

Making `public_url` reachable from the internet instead is not a good fix: it would put `/play` on the internet, and `play_allowed_networks` would have to allow everyone.

## Audio only versions

A show or daily show mapping can turn on **Audio only version** in its drawer. For each episode Sonarr imports, Tubarr then writes a second link file next to it: `Show - S01E02 - Title - Audio only.strm`, holding the episode's audio alone (`/play/<id>/audio`), for listening without the picture. This needs the [Sonarr webhook](../sonarr-and-radarr/#the-sonarr-webhook).

Jellyfin 10.11 groups files named `<same name> - <label>` as versions of a film, but not of a TV episode. Without help, the audio only file shows up as an episode of its own. With the `[jellyfin]` section set, Tubarr merges the two into one episode, with **Audio only** as a version to pick in the player:

```toml
[jellyfin]
url = "http://jellyfin:8096"
api_key_file = "/run/secrets/jellyfin_api_key"   # Dashboard > API Keys > +
# library_paths = [["/media", "/data/media"]]    # [as Tubarr sees it, as Jellyfin sees it], when they differ
```

Merging versions is an admin call, and Jellyfin's API keys have admin rights. Tubarr waits until Jellyfin has scanned both files, asks Jellyfin to probe the episode so the video version stays first, merges the two, and reads the episode back to check. It looks again after 30 seconds, then waits twice as long each time, and gives up after about 30 minutes (`merge_retry`, `merge_attempts`). The queue survives restarts, and the Health page shows its size and the last error.

A merge that was given up on (Jellyfin had not scanned the files in time) is queued again with the mapping's audio versions action, `POST /admin/api/mappings/<id>/audio-versions`. Turning the option off, deleting an episode file or renaming one takes the versions apart at once, so no episode keeps a link to a missing file. Renamed files are merged again once Jellyfin has scanned them.

## Episode images

TheTVDB rarely has stills for podcast episodes, so Jellyfin shows them without an image. A show or daily show with **Episode images from YouTube** on gets each imported episode's YouTube thumbnail saved next to it as `<episode>-thumb.jpg`, a name Jellyfin reads as a local episode image. This also needs the [Sonarr webhook](../sonarr-and-radarr/#the-sonarr-webhook). Leave it off for regular shows, whose stills Jellyfin already finds.

## Podcasts

Video podcasts are ordinary Sonarr series to Jellyfin. A separate Shows library for them keeps them out of your TV shows: give them their own Sonarr root folder (a channel's `show_root_folder`, or the mapping's own folder) and point the library there. The audio only versions and episode images above are made for them. [Podcasts](../../using/podcasts/) covers matching episodes by their number and the rest.

## Which media servers work

Every episode and film reaches your library as a `.strm` link file, so a media server has to play those for Tubarr to work.

| Server | Status |
|---|---|
| Jellyfin | Works, tested with 10.11. Remote viewers need Force transcoding of remote media sources, see [Viewers outside your network](#viewers-outside-your-network) |
| Plex | Does not work. Plex does not play `.strm` files |
| Emby | Untested. Emby plays `.strm` files in general, so playback may work, but nobody has tried it with Tubarr yet. The audio only merge and the remote viewer setting above are Jellyfin's and do not apply |
| Kodi | Untested. Kodi plays `.strm` files natively, so playback may work, but nobody has tried it with Tubarr yet |

If you try Tubarr with Emby or Kodi, we would like to hear how it went.
