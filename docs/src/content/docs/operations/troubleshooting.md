---
title: Troubleshooting
description: Common problems, what causes them and how to fix them.
---

Start with the Health page. It says why its status is not ok, and shows yt-dlp and deno, the YouTube lookups and the limiter, the canary, the watcher and the matcher. The Activity page shows every grab, push and play start with its result. Most problems show up on one of the two.

## Logs

```sh
docker compose logs -f tubarr
```

Tubarr logs at `info` by default. Set `RUST_LOG` in the container's environment for more detail, and restart:

| Value | What you get |
|---|---|
| `info` | the default: start, syncs, releases, grabs, pushes, warnings |
| `debug` | everything, which is a lot. Use it for a short while only |
| `info,tubarr::play=debug` | debug for playback alone, the same works for `tubarr::matcher`, `tubarr::watcher`, `tubarr::resolver` and `tubarr::arr` |

At `debug` the log holds video ids and the addresses of refused plays, so check it before you share it.

## A show never appears on the Shows and films page

- Check the tag. The series must carry the `youtube` tag (`[arr] tag`) in Sonarr, in lower case.
- Check the connection. Settings > **Test Sonarr** (or **Test Radarr**) in Tubarr tests `[sonarr] url` and the API key. A container name in the URL only works when both containers share a Docker network.
- Wait for the sync, or restart. Tubarr reads the tagged titles at start and then every 15 minutes (`[arr] sync_interval`).

## Nothing is matched

Open the Unmatched page, filter by the show, and read the reasons. If the list is empty too, Tubarr has not seen any upload yet:

- The mapping needs its **Official channels**. Each line can be a channel id (it starts with `UC`), a handle like `@channel`, or a channel or video link. Tubarr turns handles and links into channel ids when you save. Without a channel nothing is matched.
- The newest uploads come in through the channel's feed within 30 minutes. Older episodes need the channel search or a playlist, which run once a day. The Health page lists each show's search and when it runs next.
- **Test against channel** in the mapping drawer shows what the rules make of every video Tubarr knows on the channels.

Common reasons on the Unmatched page and their fixes:

| Reason | Fix |
|---|---|
| the title does not name the show | the channel spells it another way: add that spelling as an alias |
| no episode number | the channel's titles use a pattern Tubarr doesn't know: set an episode regex, or use the show's playlist in episode order |
| shorter than the minimum length | lower **Minimum length** if the episodes really are short |
| an exclude word | the word is in the defaults or your excludes. Remove it from yours, or pin the video |
| two episodes in one title | a joint upload, never released as one episode. Pin it if one episode is enough |

[Mappings and matching](../../using/mappings-and-matching/) explains every rule, and [Pins and unmatched](../../using/pins-and-unmatched/) the states.

## Sonarr does not grab

Tubarr releases an episode and pushes it to Sonarr, but Sonarr does not take it.

- Look at the Activity page. A push that Sonarr rejected is listed with Sonarr's reasons, which usually name the problem: a quality profile that doesn't allow WEB-DL at that height, a minimum custom format score, a delay profile holding it (shown as pending).
- Both the indexer and the download client in Sonarr must be named `Tubarr` (`[arr] indexer` and `[arr] download_client`), carry the `youtube` tag and pass their Test buttons. Without a download client of that name, nothing is pushed at all.
- Tubarr pushes only while the episode is monitored and has no file. An episode Sonarr already has is skipped.
- `api_key_file` must be set, since the links Tubarr hands Sonarr carry the key.
- For films, Radarr's quality profile also carries a language. Give the films Tubarr serves a profile with Language set to `Any` (see [Sonarr and Radarr](../../setup/sonarr-and-radarr/#6-quality-profiles-and-languages)).
- A push the arr could not take because it was down is tried again later. **Push released** on the Activity page queues every release again, and skips what the arr already has or does not monitor.

## Health says Tubarr cannot write to a folder

Tubarr checks the folders it writes to (`complete_dir`, `[play] index_dir` and `[binaries] bin_dir`) at start and on every Health check, and logs a warning for each one it cannot write to. Without `complete_dir` every grab fails. The usual cause is a folder Docker created as root because it did not exist when the container started. Give the host folder to the user the message names, `sudo chown -R 1000:1000 /srv/media/downloads/tubarr` in the [install](../../getting-started/install/) examples. The Health page clears on its next check, with no restart.

## Sonarr grabs but imports nothing

- Sonarr must see `complete_dir` at the same path Tubarr does. Tubarr reports the path of each grab, and Sonarr imports from there.
- Captions missing: turn on Import Extra Files with `srt` in its list, in Sonarr's Media Management settings.
- A grab of a video that turns out not to play writes nothing and lands in Sonarr's history as failed. Sonarr then blocklists the release and looks for another.

## Plays fail

Jellyfin shows a playback error, or the episode never starts. Tubarr answers each play with a status code, which shows on the Activity page next to the play:

| Status | Cause | Fix |
|---|---|---|
| 403 | the player's address is not in `[auth] play_allowed_networks` | add the network Jellyfin fetches from. Behind a reverse proxy, list the proxy in `trusted_proxies` (see [Jellyfin](../../setup/jellyfin/#who-may-fetch-play)) |
| 404 | the video id is not one Tubarr released, pinned or grabbed, or the video is gone, private, members only or a livestream still processing | for a `.strm` you wrote by hand, pin the video first. For a video that is gone, delete the episode file and let Sonarr find another |
| 403, with an age restriction in the log | YouTube wants a signed-in account for this video | only the cookies step can play it, see below |
| 451 | the video is blocked in your country | nothing to fix on Tubarr's side |
| 503, with `Retry-After` | YouTube is throttling lookups, the lookup queue is full, or `[play] max_streams` plays (8 by default) are already streaming | wait and play again. See [YouTube lookups fail](#youtube-lookups-fail). If it happens whenever many people watch at once, raise `max_streams` (see [Sizing](../sizing/)) |
| 504 | yt-dlp did not finish in time | try again. If it keeps happening, see below |
| 502 | yt-dlp failed, or YouTube's stream changed under the play | try again. The log says which |

If nothing reaches Tubarr at all (no play on the Activity page), Jellyfin or the client cannot reach `public_url`. Run `curl -I <public_url>/healthz` from inside Jellyfin's container.

### Plays fail only outside your network

Viewers at home can play, viewers outside cannot. Their client fetches the `.strm` URL itself, and your `public_url` is a private address it cannot reach. Turn on **Force transcoding of remote media sources** for those users in Jellyfin, as described on the [Jellyfin](../../setup/jellyfin/#viewers-outside-your-network) page.

### A video Tubarr never released returns 404

`/play` only serves videos Tubarr released, pinned or grabbed. Any other id gets a 404 before Tubarr asks YouTube anything, so `/play` cannot be used to stream arbitrary YouTube videos. `.strm` files written before this rule existed keep playing: their ids are carried over from the earlier releases, pins and grabs, and from the sample tables on disk. To play a video Tubarr did not release, pin it to the episode on the Unmatched page or with `POST /admin/api/pins`.

## YouTube lookups fail

Every lookup goes through yt-dlp, on a shared budget. When YouTube answers with a 429 or a bot check ("Sign in to confirm you're not a bot"), every lookup pauses for `[rate_limit] throttle_pause` (15 minutes), and the Health page shows the pause. Plays fail with 503 meanwhile.

In order of what to try:

1. **Wait it out.** Short spells of bot checks come and go. Don't raise the `[rate_limit]` budget: more lookups make it worse.
2. **Update or roll back yt-dlp.** YouTube changes often, and yt-dlp follows within days. **Check now** on the Health page fetches the newest. If lookups broke right after an update, **Roll back** puts the previous version back. Set `[canary] video_id` so Tubarr notices on its own: three failed canary lookups in a row make it check for a yt-dlp update at once.
3. **A PO token server.** YouTube asks some addresses, datacenter ones above all, for a proof of origin token. Run the bgutil PO token server next to Tubarr and set `[fallback] po_token_url` and `plugin_dir` (the image ships the plugin in `/opt/yt-dlp-plugins`, use the same version for the server). Tubarr then tries a token after a plain lookup is refused, and stays with it for `sticky_for`.
4. **A cookies file, as the last resort.** `[fallback] cookies_file` points at a Netscape cookies file exported from a signed-in YouTube account. Tubarr uses it only after the plain lookup and the PO token step have failed, logs a warning each time, and never writes the file back.

:::caution
Use a spare account for the cookies file, never your own. YouTube can see that the account is used from a server, and may limit, challenge or close it. Treat the file like a password: anyone who has it is signed in as that account.
:::

## The Health page shows no yt-dlp or deno version

Tubarr could not download them. Check that `/config` is writable by uid 1000 and that the container can reach GitHub. Behind a network that blocks GitHub, install both yourself and point `[binaries] yt_dlp` and `deno` at them. The same applies to a machine that is neither x86_64 nor arm64.

## The container restarts or gets killed

`docker inspect tubarr --format '{{.State.OOMKilled}}'` says `true` when the container ran out of memory. Each lookup runs yt-dlp with deno, which together take about 400 MiB for a few seconds, so a 512 MiB limit is too small. Give it 1 GiB, and more with a larger fragment cache or more yt-dlp processes. [Sizing](../sizing/) has the measured figures.

A container that restarts with no OOM kill: read the log from just before the restart. A bad config value stops Tubarr at start with a message naming the key.

## Locked out

- **Lost the setup code.** The code for the first administrator is printed in the log at every start, as long as no user exists. Restart Tubarr and read the log again.
- **Lost the only password.** Users and their passwords live in the database. With another admin, remove and add the user again on the Settings page. With no other admin, the API key still works for the admin API (`X-Api-Key`): `POST /admin/api/users` adds a new admin.
- **Too many failed sign-ins.** Sign-in is throttled per address and per username. Wait a while and try again.

## Still stuck

Open an issue in the repository with the version from the Health page, what you expected, what happened, and the log lines around it at `debug`. Leave out your `public_url` if it is a public address, and check the log for anything you'd rather not share.
