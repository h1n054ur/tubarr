---
title: Mappings and matching
description: What a mapping is, the rules it carries, and how Tubarr decides which upload is which episode or film.
---

A mapping is Tubarr's record of one show, daily show or film. It ties a title in Sonarr or Radarr to the YouTube channels that publish it, and carries the rules the matcher uses to pick the right upload for each episode. The Shows and films page lists them all, with how many releases and unmatched notes each one has. Click a row to open its drawer, where the rules are edited.

## How it fits together

1. Tag a series in Sonarr or a film in Radarr with `youtube` (`[arr] tag`). The arr sync reads the tagged titles at start and every `sync_interval`, and keeps one mapping per title, with Sonarr's alternate titles as aliases.
2. The watcher finds uploads on each mapping's official channels (see [Finding uploads](#finding-uploads)).
3. The matcher decides which upload is which episode or film and writes releases. Everything it looked at but did not release goes to the Unmatched page with a reason.
4. Tubarr pushes each new release to the arr that wants it (see [How releases reach the arr](../../setup/sonarr-and-radarr/#how-releases-reach-the-arr)). The arr runs its usual checks and grabs it from Tubarr's SABnzbd API.
5. The grab writes `<release>.strm`, holding `<public_url>/play/<video id>`, and the captions next to it, then reports the job as completed at once. The arr imports both into its library folder.
6. Jellyfin opens the `.strm` and reads `/play`, which Tubarr serves from YouTube's DASH files as Jellyfin asks for each byte range (see [How the file is built](../../setup/jellyfin/#how-the-file-is-built)).

## Where mappings come from

Most mappings come from the arr sync. Tubarr reads every series and film tagged `youtube` (`[arr] tag`) in Sonarr and Radarr at start and then every 15 minutes, and keeps one mapping per title. The title, its kind, its year and its TVDB or TMDB id belong to the arr: Tubarr copies them and never changes them. Everything else on the mapping is yours to set.

A title whose tag is removed in the arr stays on the page as inactive, with its rules, and Tubarr stops matching it. Tagging it again makes it active with the same rules.

**Add mapping by hand** on the Settings page makes one for a title the arrs don't tag. It needs a TVDB id for a show or a TMDB id for a film. It has no arr id, so Tubarr pushes nothing for it, and it is mostly useful to try rules out.

There are three kinds:

| Kind | Matched by | Arr |
|---|---|---|
| Show | the episode number in the video title, or a playlist position, or a running number for podcasts | Sonarr, series type Standard |
| Daily show | the air date | Sonarr, series type Daily |
| Film | the film's title | Radarr |

## The rules

### Official channels

Tubarr only looks at the channels you name for a mapping. Nothing is ever matched from any other channel, so a re-upload or a fan channel never gets in. Put each channel in **Official channels**, one per line, as its id, its `@handle`, or a link to the channel or to one of its videos. On save Tubarr looks each one up and stores the channel id, and the drawer shows the channel's name next to it. A channel id starts with `UC` and is 24 characters long. On YouTube, open the channel, then its About panel, then Share channel > Copy channel ID.

A mapping without a channel matches nothing. Films are also looked for on the global `[films] channels`, after their own.

Saving a new channel starts a search for the show's back episodes on it (see [Back episodes](#back-episodes)).

### Episode patterns

With the **Episode regex** field empty, Tubarr reads the episode number with its built-in patterns, the first hit wins:

1. Web series style: `S01E02`, `S01 E01`, `S2 Ep 4`, `S2 | EP6`
2. Drama style: `Episode 12`, `Ep 25`, `Season 2 Episode 3`
3. A bare `E01` between separators: `Show | E01 | Title`

With the built-in patterns a title must also name the show, by its title or one of its aliases. One channel often carries many dramas, and "Episode 12" alone does not say which.

A custom regex replaces the patterns. It needs a named group `ep` for the episode, and may have `season` and `part`. A custom regex is trusted to name the show itself, so the title check is skipped. For example, for a channel that writes `Mere Humsafar Ep 12`:

```
(?i)Mere Humsafar Ep (?P<ep>\d+)
```

Some titles are never released as one episode:

- `Episode 24 & 25`, `24 and 25` or `24 - 25` name two episodes.
- `Ep 1/3` reads as episode 1 of 3 when 3 is that season's episode count in Sonarr, else as two episodes. `Episode 2 of 3` is one episode.
- A title that names two different numbers in a way Tubarr cannot settle is noted as unsure.

### Excludes

Title words that rule a video out, one per line. They are matched as whole words, in any case, with a plural `s` too. These are always excluded for shows: `Teaser`, `Promo`, `Highlights`, `Trailer`, `Best Scene`, `BTS`, `Review`, `Reaction` and `Shorts`. Films have their own list: `Trailer`, `Teaser`, `Promo`, `Song`, `Jukebox`, `Scene`, `Making`, `Review`, `Reaction`, `Interview`, `Climax`, `Dialogue`, `Shorts`, `Status` and `Spoof`, minus any word that is part of the film's own name.

Add your own for a channel's habits, like `Recap`, `Clip` or `Behind the Scenes`.

### Minimum length

Uploads shorter than this are left out. Empty means 25 minutes for shows and daily shows, and `[films] min_duration` (80 minutes) for films. The floor catches the 15 to 17 minute highlight uploads many channels post next to each episode. Lower it for a show whose episodes really are short.

An upload whose length is not known yet waits as `pending` until a lookup gives it.

### Language

The language goes into the release name (`URDU-TUBARR`) and the Newznab language attribute. A new mapping takes the arr's language. Sonarr has no Urdu and reports a Pakistani drama as English, so a mapping still on English or Unknown takes the `language` of its first channel in `[channels]`, at the next sync or as soon as its channels are saved. A language you change in the drawer is yours and Tubarr never touches it again. The badge next to the field says who set it: the arr, the channel, or you.

### Back episodes

A long-running show or podcast can have hundreds or thousands of episodes already on YouTube. **Back episodes to release** keeps only the newest N of the episodes uploaded before the mapping was added. Newest goes by the episode itself, its TheTVDB air date, then its season and episode number. Every episode uploaded after the mapping was added is always released, and so is a pinned one. Empty releases every back episode, and 0 only new ones.

The back episodes left out get an unmatched note in the `capped` state. They are never looked up or released.

How Tubarr finds back episodes in the first place is covered under [Finding uploads](#finding-uploads) below.

### Aliases

The names a title goes by on YouTube. Sonarr's alternate titles are copied in at every sync, marked as coming from the arr. Add your own when a channel spells the show differently, for example `Kabhi Main Kabhi Tum` for a show TheTVDB lists as `Kabhi Mein Kabhi Tum`. The built-in patterns and the daily and podcast matchers accept a title that names the show or any alias.

### Library folder

The arr root folder the title should live in, so it lands in the right Jellyfin library. See [Root folders per language](../../setup/sonarr-and-radarr/#root-folders-per-language).

## Daily shows

Sonarr asks for daily shows by air date, so Tubarr gives each upload a date: the date in its title when it has one, else its broadcast date in the channel's time zone.

- A title date may be up to 3 days before the upload (channels often upload late) or 1 day after it.
- The time zone is the mapping's **Channel time zone** (an IANA name like `America/Chicago`, which follows daylight saving time), else its **Fixed UTC offset** in minutes. With both empty it is Pakistan's +05:00, which suits Pakistani channels and nothing else, so set it for any other daily show.
- One release per air date: a full episode wins over clips, and the earliest full upload over later copies. A second full upload with another title on the same day (an evening edition, say) is noted as unsure, unless your regex keeps only one of them.
- An upload dated by its upload time may move to the day before or after, onto a date Sonarr wants that no other upload claims.

## Matching by number in the title

Video podcasts are often numbered by year on TheTVDB (`S2026E136`) while the podcast's own number appears only in the episode title ("#2569 - Bill Thompson"). **Match by number in the title** reads the `#2569` from the YouTube title (or the `ep` group of your regex) and releases the upload as the episode whose TheTVDB title holds the same number. [Podcasts](../podcasts/) covers it in full.

## Playlists

A show can name an official playlist (`PL...`) in the drawer. **Use playlist for** picks how:

- **finding videos** (the default): the playlist's videos are added to what the matcher sees, and their titles still give the episode number. Useful when a channel posts so much that older episodes fall out of its recent uploads.
- **episode order**: the video's place in the playlist gives the number, for shows whose titles carry none. **Its season** and **First episode** say where the playlist starts. Extras your excludes catch take no number. A short video they miss, or one not looked up yet, makes every number after it unsure until you exclude it or pin around it.

## Episodes in parts

Some channels split an episode or film into `Part 1`, `Part 2` and so on. Tubarr releases the parts as one episode when the set is whole (no gap, all on one channel, the total over the minimum length) and no full upload exists. The `.strm` then holds every part's id, and `/play` plays them back to back as one file.

When each part sits on its own episode number and the numbers run in step (`EP6 Village (Part 1)`, `EP7 Village (Part 2)`), the parts name a story arc, and each episode is released on its own.

## Films

A film is matched on its own channels and on `[films] channels`. Tubarr strips the marketing words from each title ("Full Hindi Dubbed Movie", "(4K)", a cast list) and compares the rest with the film's title and aliases. A title at least `[films] similarity` (0.9) close is a match, as long as the year, when the title has one, is within a year of Radarr's and the upload is long enough. A title a little below the threshold, or a different year, is noted as unsure for a pin and never released.

A dubbed upload carries the dub's language in its release name, taken from the title (`Hindi Dubbed`).

## Videos that cannot play

Each matcher run spends its leftover lookup budget on videos released at an assumed height, so a video that is gone, private or blocked leaves the feed before an arr grabs it. Tubarr serves only H.264 video (1080p, else 720p, else 480p) with AAC audio, never VP9 or AV1, and the release name carries the height it serves.

A new upload that YouTube offers only in VP9 or AV1 for now is marked unplayable and looked up again every six hours from the same budget, and released once H.264 shows up. A grab of a video that turns out to be unplayable writes nothing and lands in the arr's history as Failed, so the arr blocklists the release and looks for another.

[`GET /admin/api/unplayable`](../../reference/admin-api/#playability) lists those videos, and `DELETE /admin/api/unplayable/{video_id}` lets the matcher try one again. `GET /admin/api/releases/check` lists releases, imported ones included, whose video is unplayable or was never looked up, so dead `.strm` files can be found and cleaned up.

## Test against channel

The **Test against channel** button in the drawer runs the title rules over every video Tubarr already knows on the mapping's channels, with any edits you have not saved yet, and shows a verdict for each one:

| Verdict | Meaning |
|---|---|
| matched | the rules accept it as an episode of this show, or this film, of full length |
| unsure | the rules point at the show but cannot say for certain. The matcher would note it |
| excluded | another show, an extra, too short, or no pattern hit, with the reason |

Nothing is saved, and no YouTube lookup is made. Use it to try a regex or an exclude before saving.

The test previews the rules alone. It does not know which episodes Sonarr wants, your pins, or which of two uploads of one episode would win. When no title on the channel names the show at all, it says so and offers a shorter name as an alias. TheTVDB's "The Joe Rogan Experience" against a channel's "Joe Rogan Experience #2569" is the typical case.

The same test is `POST /admin/api/mappings/{id}/test` in the [Admin API](../../reference/admin-api/).

## Finding uploads

Tubarr only matches videos it has seen, and it sees them in four ways. Every way except the feed is one yt-dlp lookup on the shared `[rate_limit]` budget, waits while lookups are paused after a 429 or bot check, and is spread out over time.

| Source | When | Lookups |
|---|---|---|
| RSS feed per channel | every 30 minutes | none, it is a plain HTTP feed of the newest 15 uploads |
| Channel list | once a day per channel | 1 per channel, reads the newest `listing_size` uploads (100 by default) |
| Short channel list while its feed fails | about every 30 minutes per channel, once the feed has failed for longer than that, until a feed read works again | 1 per channel and turn, reads the newest `fallback_listing_size` (15) uploads, at most `fallback_per_hour` (30) an hour across all channels, spaced evenly |
| Official playlist named by a show | once a day per playlist | 1 per playlist, up to 500 videos |
| Channel search for a show without a playlist | at once when the show is mapped, made active again, or its channels, aliases or backfill limit change, then once a day until every wanted episode (or every back episode its limit keeps) is released or `backfill_days` pass. Never with a limit of 0 | 1 per channel and name (the title and up to 2 aliases) per day, 2 when a search comes back full (`backfill_size` results) and a second one for "<name> Episode" follows |

YouTube sometimes answers every channel feed with 404 or 500 for hours, mostly to datacenter addresses. The short lists keep new uploads coming in at about the feed's pace meanwhile, and the Health page marks each channel on them ("feed failing, using listing") with a count above the Watcher table. With more failing channels than the hourly cap allows, each gets its turn less often.

A busy channel (ARY Digital posts dozens of videos a day) pushes a show out of its newest 100 uploads within days, so an older show or its back episodes are found only through a playlist or the search. A typical show on one channel with one alias costs 2 lookups a day for at most 14 days, and none after every episode is out. A search that fails for a passing reason is tried once more after an hour, and the Health page lists each show's backfill. A channel that posts many short clips tagged with the show's name can fill a whole search with them, so the full episodes fall past the results read. A full search is therefore followed by one for "<name> Episode", which ranks the episodes first. Raise a channel's own `listing_size` instead when the gap is only a few weeks: that costs nothing extra, since it is one list per day either way, but a longer list takes longer to read.

Newest, for the back episode limit, goes by the episode itself: its TheTVDB air date, then its season and episode, so a channel list that dates a dozen uploads the same rough "1 month ago" never changes which ones are kept. Only an upload with no episode yet goes by its upload date at match time. A release a capped episode already had is taken back.

## How a run works

The checks for each video on a mapping's channels run in this order: an extra word in the title (see Excludes) or an upload under the minimum length leaves it out with a reason. Then the title must name the show and exactly one episode, or it is noted as unsure. Of what is left, the best upload per episode is released. A [pin](../pins-and-unmatched/) on the Unmatched page replaces that choice.

The matcher runs after each watcher find, each arr sync, each pin and at least every 30 minutes. Each run is a full recompute for each mapping, so running it twice changes nothing. A mapping whose inputs have not changed since its last run (rules, aliases, pins, Sonarr's episodes, the videos on its channels, its releases and notes) is skipped.

Everything the matcher looked at but did not release is kept as an unmatched note with the reason. [Pins and unmatched](../pins-and-unmatched/) explains the notes and how to settle them.
