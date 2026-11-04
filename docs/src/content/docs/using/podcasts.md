---
title: Podcasts
description: Video podcasts in Sonarr and Jellyfin, matched by the episode number in their titles.
---

Many podcasts publish every episode in full on YouTube, and TheTVDB lists the bigger ones as series. To Sonarr and Jellyfin a podcast is an ordinary show. What differs is how Tubarr matches its episodes, and a few extras that make it nicer to listen to.

## How podcast episodes are numbered

TheTVDB often numbers a podcast by year: The Joe Rogan Experience episode `S2026E136` is titled "#2569 - Bill Thompson". The podcast's own running number appears only in the episode title. The YouTube upload carries the same number: "Joe Rogan Experience #2569 - Bill Thompson".

With **Match by number in the title** on, Tubarr reads the `#2569` from each video title and releases the upload as the episode whose TheTVDB title holds the same number. The number is two to five digits after a `#`. If a channel writes it another way (`Ep. 2569`, `No 2569`), put a regex with a named group `ep` in the mapping's **Episode regex** and Tubarr reads the number from that group.

Some cases are handled on their own:

- **TheTVDB has not named the episode yet.** New episodes often get their title on TheTVDB a few days late. An upload whose number is above every number TheTVDB lists goes by air date instead, with the [daily show rules](../mappings-and-matching/#daily-shows), over the episodes that have no number yet.
- **Clips with the same number.** Channels often post short clips titled with the episode's number. An upload under the minimum length, or under half the length of the longest upload with that number, never takes the episode.
- **A number Sonarr does not want.** A number TheTVDB lists on an unmonitored episode stays on that episode, noted and never released.
- **Doubtful numbers.** A number TheTVDB lists twice, or a title with two numbers, waits for you to [pin](../pins-and-unmatched/) the right episode.

## The podcast preset

A podcast needs a few settings that differ from a drama. The podcast preset sets them in one go:

| Setting | Value |
|---|---|
| Match by number in the title | on |
| Back episodes to release | the newest 10 |
| Minimum length | 20 minutes |
| Excludes | `Clip` and `Best of` added to the defaults (which already hold `Highlights`, `Trailer` and `Shorts`) |
| Episode images from YouTube | on |
| Channel time zone | the channel's time zone from `[channels]` |

Match by number in the title, Episode images from YouTube and Minimum length are always set to the values above, replacing what was there. The other fields keep what you already filled in: your own excludes stay and the two words are added to them, and Back episodes to release and the time zone are filled only when empty. Change any of them afterwards in the drawer.

There are two ways to use it:

- For a show the arr synced, open its drawer and press **Use podcast defaults**, then save.
- For a mapping made by hand on the Settings page, pick **Podcast (#N in the title)** under **Starting rules**.

The preset needs the channel's time zone, because the air date fallback above works by the channel's day. Tubarr never guesses it: if the first channel has no entry in `[channels]` and you give no time zone, it asks for one. Pakistan's +05:00, the default for dramas, would put a US podcast's evening upload on the wrong day.

A show with **Match by number in the title** on counts as a podcast: the Shows and films page marks it with a Podcast badge, and its kind filter has a Podcasts choice (`kind=podcast` in the API).

## Time zones

Give the channel an IANA time zone, which follows daylight saving time:

```toml
[channels.UCzQUP1qoWDoEbmsQxvdjxgQ]
language = "English"
country = "United States"
timezone = "America/Chicago"
```

or set **Channel time zone** in the drawer. A fixed `utc_offset_mins` works too, but drifts by an hour twice a year in places with daylight saving time. With the zone set, an episode uploaded at 9 pm in Chicago keeps its Chicago date when it is already the next day in UTC.

## Back episodes

A long-running podcast can have thousands of episodes on YouTube. Releasing them all would mean thousands of lookups and pushes to Sonarr, so the preset keeps only the newest 10 of the episodes uploaded before the mapping was added. Every new episode is always released. Raise **Back episodes to release**, or empty it for all of them, if you want more. The ones left out show on the Unmatched page as `capped`.

## Episode titles

The release name only says which episode it is (`Joe.Rogan.Experience.S2026E136...`). With episode renaming on, Sonarr names the file with the episode title from TheTVDB, as for any show, so the episode shows up in Jellyfin as "#2569 - Bill Thompson".

## Audio only versions

Many people listen to podcasts rather than watch them. With **Audio only version** on, Tubarr keeps a second link file next to each episode Sonarr imports, `Show - S2026E136 - Title - Audio only.strm`, which plays the episode's audio alone as an M4A. It uses far less bandwidth than the video.

This needs the [Sonarr webhook](../../setup/sonarr-and-radarr/#the-sonarr-webhook). With the `[jellyfin]` section set, Tubarr merges the two files into one episode in Jellyfin, with **Audio only** as a version to pick in the player. Without it the audio file shows up as an episode of its own. [Jellyfin](../../setup/jellyfin/#audio-only-versions) has the details.

Turning the option on writes the audio versions for every episode Sonarr already has, and turning it off removes them.

## Episode images

TheTVDB rarely has stills for podcast episodes, so Jellyfin would show them without an image. With **Episode images from YouTube** on (the preset turns it on), Tubarr saves each imported episode's YouTube thumbnail next to it as `<episode>-thumb.jpg`, which Jellyfin reads as the episode image. It also needs the Sonarr webhook.

Turning it on fetches the images for the episodes Sonarr already has. Turning it off leaves the images in place.

## A library of their own

Keep podcasts apart from your TV shows with their own Sonarr root folder and Jellyfin library: set `show_root_folder` on the podcast's channel in `[channels]`, or the mapping's own library folder in the drawer. See [Root folders per language](../../setup/sonarr-and-radarr/#root-folders-per-language).
