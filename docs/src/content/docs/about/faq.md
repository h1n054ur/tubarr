---
title: FAQ
description: Common questions about Tubarr, YouTube's terms, media servers and running it at home.
---

## Is this allowed by YouTube?

No. YouTube's terms rule out reaching the service by automated means and downloading or passing on videos outside the ways YouTube itself offers. Tubarr does both: it looks videos up with yt-dlp and relays the streams to your media server. It saves no video files, but that does not change the answer. YouTube usually responds to tools like this by blocking addresses and asking for bot checks rather than going after individual users. That is what tends to happen, and nobody can promise it stays that way. You decide whether to run it, and it is your responsibility. [Terms and personal use](../terms/) says more. None of this page is legal advice.

## Can my Google account get banned?

Not through Tubarr's normal lookups, which don't sign in to anything. The risk comes with the optional cookies fallback (`[fallback] cookies_file`). With it, lookups that YouTube pushes back on are retried signed in as the account the cookies belong to. YouTube can see that, and accounts used this way have been flagged, had their cookies expire within hours, or been suspended for a while or for good. The yt-dlp project gives the same warning.

If you turn it on:

- Use a spare account made for this, never your main Google account or one tied to Gmail, Drive, photos or payments.
- Don't use an account with channel memberships or YouTube Premium.
- Treat the cookies file like a password. Anyone who has it can act as that account.
- Expect to replace the account now and then.

Tubarr sends a Discord card each time the cookies step is used, if cards are set up, so you can see how often it happens.

## Can I share my instance with friends?

Tubarr is built for one household. Every play from a friend's Jellyfin client is a stream your server fetches from YouTube and passes on, from your IP address. That spends your lookup budget, makes bot checks and blocks on your address more likely, and moves you further from personal use towards running a relay for other people, which is harder to square with YouTube's terms. Please keep `/play` on your own network (`play_allowed_networks`), don't expose it to the internet, and don't run a public instance. Public instances are not supported. If friends want the same shows, the honest answer is that they run their own.

## Why no tips on getting around geo-blocks?

Some videos are blocked in some countries. Tubarr marks them as unplayable and moves on. The project's docs will not explain how to get around regional restrictions, with a VPN or anything else. Those restrictions are often there for rights reasons, and guides to get past them would change what the project is for. Tubarr will keep reporting geo-blocked videos clearly so you know why one did not play.

## Does it download videos?

No. A grab writes a small `.strm` link file and the video's captions as `.srt`, nothing more. When someone presses play, Tubarr looks up the stream on YouTube and relays it as one MP4, built on the fly from the separate video and audio streams, without re-encoding. The video itself is never written to disk.

What it does keep are small sample tables for each video it has played (a few hundred KB each, in `[play] index_dir`), so later plays start sooner. They are capped at 2 GiB by default and safe to delete. [Jellyfin](../../setup/jellyfin/#how-playback-works) explains playback and [Sizing](../../operations/sizing/#disk) lists what takes disk space.

## Does Plex work?

No. Plex does not play `.strm` files, and that is how every Tubarr episode and film reaches your library. Jellyfin works and is what Tubarr is tested with. Emby and Kodi are untested. See [Which media servers work](../../setup/jellyfin/#which-media-servers-work).

## Which Jellyfin clients work remotely?

At home every client works, as long as `public_url` is an address your devices can reach. Outside your network, clients that fetch the `.strm` URL themselves fail, because they cannot reach Tubarr's private address. The fix is one Jellyfin setting per user (Dashboard, Users, the user, Media Playback, Force transcoding of remote media sources), which makes Jellyfin fetch the stream and pass it on to every client. [Viewers outside your network](../../setup/jellyfin/#viewers-outside-your-network) has the steps.

## What does it cost in memory?

Give the container 1 GiB with the default config. Tubarr itself usually stays well under 250 MiB, but each YouTube lookup runs yt-dlp and deno, which take about 400 MiB together for a few seconds. With a 512 MiB limit the container gets killed on a lookup. [Sizing](../../operations/sizing/) has the measured figures and what changes them.

## Can I use it without Sonarr or Radarr?

Not in a useful way. Tubarr starts without them, but its mappings come from the titles you tag in Sonarr and Radarr, and releases only reach your library when one of them grabs and imports them. You can add a mapping by hand on the Settings page to try rules out against a channel, but nothing gets imported without an arr. You need at least one of the two: Sonarr for shows, Radarr for films. See [Sonarr and Radarr](../../setup/sonarr-and-radarr/).
