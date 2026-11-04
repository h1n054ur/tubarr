---
title: Mappings and matching
description: What a mapping is, the rules it carries, and how Tubarr decides which upload is which episode or film.
---

A mapping is Tubarr's record of one show, daily show or film. It ties a title in Sonarr or Radarr to the YouTube channels that publish it, and carries the rules the matcher uses to pick the right upload for each episode. The Shows and films page lists them all, with how many releases and unmatched notes each one has. Click a row to open its drawer, where the rules are edited.

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

Tubarr only matches videos it has seen. It sees them in four ways:

| Source | How often | YouTube lookups |
|---|---|---|
| The channel's RSS feed | every 30 minutes | none, it is a plain feed of the newest 15 uploads |
| The channel's upload list | once a day | 1 per channel, the newest 100 uploads (`listing_size`) |
| An official playlist named by a show | once a day | 1 per playlist |
| A channel search for a show without a playlist | when the show is mapped or its channels or aliases change, then daily for up to 14 days (`[watcher] backfill_days`) until every wanted episode is out | 1 per channel and name a day |

A busy channel pushes a show out of its newest 100 uploads within days, so older shows and back episodes are found by a playlist or the search. When a channel's feed keeps failing, a short upload list stands in for it until the feed works again, and the Health page marks the channel.

## How a run works

The matcher runs after each watcher find, each arr sync, each pin and at least every 30 minutes. Each run is a full recompute for each mapping, so running it twice changes nothing. A mapping whose inputs have not changed since its last run (rules, aliases, pins, Sonarr's episodes, the videos on its channels, its releases and notes) is skipped.

Everything the matcher looked at but did not release is kept as an unmatched note with the reason. [Pins and unmatched](../pins-and-unmatched/) explains the notes and how to settle them.
