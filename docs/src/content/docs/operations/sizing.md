---
title: Sizing
description: How much memory, disk and CPU Tubarr needs, measured on a running instance.
---

Tubarr is small most of the time. The one thing that needs room is the short burst when it looks a video up on YouTube.

## Memory

Give the container **1 GiB** of memory with the default config:

```yaml
services:
  tubarr:
    mem_limit: 1g
```

Most of the time Tubarr uses well under 250 MiB. Each stream lookup starts yt-dlp, which runs deno to read YouTube's player script, and the two together take about 400 MiB for a few seconds. With a 512 MiB limit the container gets killed on a lookup.

### Measured

These figures were measured on 2026-10-26 on a running instance with the default `[play]` and `[rate_limit]` settings and `mem_limit: 1g`. Container memory comes from the container's cgroup (`memory.current` sampled every 0.25 s, `memory.peak` for the peaks), and the per-process figures are each process's resident memory. A play was range reads of a 1080p video through `/play`. A lookup was a play of a video with no cached lookup.

| Load | Container memory | Where it goes |
|---|---|---|
| Idle after start | 80 MiB | Tubarr 69 MB |
| One play | 146 MiB | Tubarr 120 MB |
| Two plays, with seeks | 214 to 233 MiB | Tubarr 184 to 199 MB |
| One lookup, no plays | 475 MiB peak | yt-dlp 93 MB, deno about 300 MB |
| One lookup during two plays | 602 MiB peak | Tubarr 187 MB, yt-dlp 91 MB, deno about 300 MB |

Container memory includes 35 to 50 MiB of page cache. Tubarr keeps what it grew to after the plays end (about 200 MB here), because the fragment cache holds on to its contents. Nothing was swapped and nothing was killed.

### What moves the numbers

- **`[rate_limit] processes`**, the number of yt-dlp runs at once (default 1). Each one adds about 400 MiB at its peak, so add that much to the limit for each process over 1.
- **`[play] fragment_cache_bytes`**, the in-memory cache of YouTube fragments (default 96 MiB). Raising it raises Tubarr's steady size by up to the difference, so add the difference to the limit.
- **Plays at the same time.** Tubarr grew by about 50 to 65 MB per play in the runs above, fragment cache included. `[play] max_streams` caps the plays streaming at once (8 by default, one more gets a 503), and `[play] max_builds` the first plays reading their sample tables at once (2 by default). A household with more viewers at once needs more memory, and a higher `max_streams`.

For example, `processes = 2` with the default cache needs about 1.4 GiB.

## Disk

| What | Where | Size |
|---|---|---|
| The database | `tubarr.db` in `data_dir` (`/config`) | grows with the library, and stays bounded by the rules below |
| yt-dlp and deno | `/config/bin` | one release of each, plus the previous one (`<name>.prev`) kept for a rollback. deno is the larger, over 100 MB |
| Sample tables | `[play] index_dir` | about 270 KB per 40 minute 1080p episode, capped at 2 GiB (`index_max_bytes`) |
| Link files and captions | `complete_dir`, then the arr's library | a few KB per episode |

Videos are never stored. Five stores grow with a library's history, and each has one rule that keeps it bounded. The Health page shows how big each one is, in its Storage section.

| Store | Kept while | Default |
|---|---|---|
| Unmatched notes | the video was uploaded within `[matcher] note_retention`, or the note waits on a lookup | 180 days |
| Watched videos | uploaded or last seen within `[matcher] note_retention`, or released, pinned, noted or in a listed playlist | 180 days |
| Download history (grabs) | made within `[arr] grab_retention`. The `.strm` files stay | 30 days |
| Activity events (searches, pushes and play starts) | recorded within `[activity] retention` | 30 days |
| Sample tables | used within `[play] index_max_idle`, and among the newest used that fit in `index_max_bytes` | 180 days, 2 GiB |

Housekeeping runs 10 minutes after start and then every 6 hours. A zero value turns that store's rule off.

The sample tables folder is safe to delete. A play whose table is gone reads it again from YouTube, which makes that one first start slower.

## CPU

Tubarr does not re-encode anything: `/play` maps byte ranges and passes YouTube's data through. A play takes little CPU. The short spikes come from yt-dlp and deno during a lookup. Any small machine that runs Sonarr and Jellyfin comfortably has room for Tubarr.

Jellyfin is a different matter. It does the transcoding when a client needs it, or when a remote viewer has **Force transcoding of remote media sources** on (see [Jellyfin](../../setup/jellyfin/#viewers-outside-your-network)), so size the Jellyfin server for that.

## Network

A play streams the video from YouTube through Tubarr to the client, so Tubarr's download rate during a play is about the video's bitrate, a few Mbit/s for 1080p. For viewers outside your network, Jellyfin then uploads the same stream to them.

YouTube lookups are spread out on purpose (`[rate_limit]`, one token every 10 seconds with a burst of 3 by default). Raising the budget makes YouTube more likely to answer with a bot check.
