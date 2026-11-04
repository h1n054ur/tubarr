---
title: Quick start
description: From nothing to an episode playing in Jellyfin, with Sonarr.
---

This page takes one show from nothing to an episode playing in Jellyfin. It uses Sonarr. Films with Radarr work the same way, with the Radarr names in place of Sonarr's.

You need Docker, a running Sonarr and Jellyfin, and a show whose official channel uploads full episodes to YouTube.

## 1. Write the config

Follow [Install](../install/) up to the config file. Two lines are enough to start:

```toml
public_url = "http://192.168.1.20:8480"
api_key_file = "/run/secrets/tubarr_api_key"
```

- `public_url` is the address Jellyfin and Sonarr use to reach Tubarr. It goes into every `.strm` file, so it must work from their containers.
- `api_key_file` holds the key Sonarr sends to Tubarr. You enter the same key in Sonarr in step 4.

Leave `complete_dir` at its default, `/downloads/tubarr`, and make sure Sonarr sees the downloads folder at the same `/downloads` path.

## 2. Start Tubarr and sign in

```sh
docker compose up -d
docker compose logs tubarr
```

On first start Tubarr creates its database and logs `tubarr started`. Then it downloads yt-dlp and deno into `/config/bin` in the background, which takes a minute or two. The Health page shows yt-dlp as failing until that is done. While no user exists, it also logs a warning with a setup code, in six groups of four characters:

```text
WARN tubarr::auth: No admin account yet. To claim this Tubarr, open http://192.168.1.20:8480/ and enter the setup code ABCD-EFGH-JKMN-PQRS-TUVW-XYZ2
```

Open `http://192.168.1.20:8480` in a browser. Tubarr shows the setup page for the first administrator. Enter the setup code from the log, then pick a username and password. You are signed in when it is done.

## 3. Connect Sonarr

On Tubarr's Settings page, under Connections, fill in **Sonarr URL** (for example `http://sonarr:8989`, a container name works when both are on the same Docker network) and **Sonarr API key** (in Sonarr under Settings > General > Security), then press **Test and save**. Tubarr checks the URL and key against Sonarr before it saves them, never shows the key again, and starts reading Sonarr at once.

You can set Sonarr in `tubarr.toml` instead, with a `[sonarr]` section and a secret file for the key (see [Sonarr and Radarr](../../setup/sonarr-and-radarr/#tell-tubarr-about-the-arr)). A connection in the config file wins over one saved on the Settings page, and shows there as read only.

## 4. Prepare Sonarr

Tubarr works through a tag. Only series with the tag are served by Tubarr, and Tubarr's indexer and download client only serve those series. The default tag is `youtube` (`[arr] tag`).

Some of the fields below are advanced settings. On each Settings screen in Sonarr, turn on **Show Advanced** at the top to see them.

1. **Tag.** In Sonarr, Settings > Tags, add a tag named `youtube`.
2. **Extra files.** Settings > Media Management, with the advanced settings shown, turn on Import Extra Files and put `srt` in its list, so the captions are imported with each episode.
3. **Download client.** Settings > Download Clients > + > SABnzbd:
   - Name: `Tubarr`
   - Host: the host in your `public_url`, for example `192.168.1.20`
   - Port: `8480`
   - API Key: the key in `secrets/tubarr_api_key`
   - Category: `tv`
   - Tags: `youtube`
4. **Indexer.** Settings > Indexers > + > Newznab, with the advanced settings shown:
   - Name: `Tubarr`
   - URL: your `public_url`, for example `http://192.168.1.20:8480`
   - API Path: `/api`
   - API Key: the same key
   - Categories: `5000` and `5040`
   - Download Client: `Tubarr`
   - Tags: `youtube`

Keep both names as `Tubarr`. Tubarr hands each release to Sonarr pinned to the download client of that name (`[arr] download_client`), and without it nothing is pushed. The indexer's Download Client field covers the grabs Sonarr makes on its own, from its RSS sync or a search: without it Sonarr picks a client by tag and priority, and if you also run a real SABnzbd it could send a Tubarr release there, where it fails. Sonarr's Test button on each form should pass.

Then go back to Tubarr's Settings page and press **Check Sonarr setup**. It reads Sonarr and lists the tag, the indexer, the download client, Import Extra Files with `srt` and the webhook (optional, see [The Sonarr webhook](../../setup/sonarr-and-radarr/#the-sonarr-webhook)), each as In place, Missing or Optional, with what to change. It changes nothing in Sonarr.

## 5. Add the show

In Sonarr, add the series as usual, monitored, with the `youtube` tag and a quality profile that accepts WEB-DL 1080p (`Any` does). Releases are named like scene releases, for example `Show.Name.S01E05.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR`, so Sonarr needs no custom formats.

Tubarr reads the tagged series at start and then every 15 minutes (`[arr] sync_interval`). After the next read the show appears on Tubarr's Shows and films page.

## 6. Tell Tubarr the official channel

Tubarr only looks at the channels you name for a show. On the Shows and films page, open the show and put the channel in **Official channels**, one per line, then save. The channel's id (it starts with `UC`), its `@handle` or a link to it all work, and Tubarr stores the id. On YouTube, the channel's About panel has a Share channel button with Copy channel ID.

Tubarr then looks for the show's uploads on that channel: the channel's feed, its daily list, and a search for the show's name (see [Finding uploads](../../using/mappings-and-matching/#finding-uploads)). The matcher decides which upload is which episode, using the show's title and its aliases from Sonarr, the episode number in the video title, and a duration floor of 25 minutes. Uploads it is not sure about go to the Unmatched page with a reason, where you can pin one to an episode.

## 7. Watch the grab

For each episode it releases for the first time, Tubarr pushes the release to Sonarr if Sonarr has the episode monitored and without a file. Sonarr runs its usual checks and grabs it from Tubarr. The grab writes `<release>.strm`, holding `<public_url>/play/<video id>`, and the captions next to it in `/downloads/tubarr`, and reports the job as done at once. Sonarr then imports both into the show's folder.

Tubarr's Activity page lists each push and grab. Sonarr's Activity > History shows the grab and the import.

## 8. Play it in Jellyfin

Add a Shows library in Jellyfin pointing at the folder where Sonarr keeps the show, as Jellyfin sees it, and scan it. The episode shows up like any other. Press play: Jellyfin opens the `.strm`, fetches `<public_url>/play/<video id>` from Tubarr, and Tubarr serves the video from YouTube as Jellyfin reads it.

Jellyfin must reach `public_url` too. Viewers outside your home network need one more setting, described on the [Jellyfin](../../setup/jellyfin/#viewers-outside-your-network) page.

## If nothing shows up

- No mapping on the Shows and films page: check the `youtube` tag on the series and the Sonarr connection. **Test Sonarr** on Tubarr's Settings page checks the connection, and **Check Sonarr setup** what is missing in Sonarr.
- A mapping but no releases: open the Unmatched page and filter by the show. Each note says why an upload was left out.
- Releases but no grab: run **Check Sonarr setup**, check that Sonarr's indexer and download client are both named `Tubarr`, carry the tag, and pass their tests, and that the episode is monitored and missing.
- A grab but no playback: check that `public_url` works from Jellyfin's container.

[First run](../first-run/) covers what to check on Tubarr's own pages once it runs.
