---
title: Pins and unmatched
description: Why an upload was left out, and how to settle it with a pin or hide it.
---

When the matcher is not sure, it releases nothing. A wrong episode in your library is worse than a missing one, because Sonarr would import it and stop looking. Instead, every upload the matcher looked at for a mapping but did not release gets a note on the Unmatched page, with the reason. From there you can pin the upload to the right episode, or hide the note.

## States

Each note has a state, shown as a badge next to its reason:

| State | Meaning | What to do |
|---|---|---|
| `unsure` | the title or the facts do not say for certain which episode it is, for example a title that names no number, two numbers, or a film title a little below the similarity threshold | pin it if you know, or ignore it |
| `pending` | waiting for facts, usually the video's length or picture size. The next run checks again | nothing. Tubarr looks it up from the matcher's lookup budget |
| `duplicate` | another upload of the same episode won | nothing, unless the wrong one won: then pin the one you want |
| `partial` | one part of an episode split over several videos, where the set is not whole yet (a part is missing or too short) | wait for the missing part, or pin a full upload |
| `capped` | an older back episode the mapping's **Back episodes to release** limit leaves out. Never looked up or released | raise the limit, or pin it if you want this one episode |

Ignoring a note is not a state. It hides the note from the list and keeps its state.

The Health page counts the notes waiting for a match on active mappings. Duplicates and capped notes are left out of that count, since nothing waits on them.

## Finding a note

The page shows 25 notes at a time, newest first, with a pager below. Above the list:

- **Search** finds text in the video title, the show's title, the video id or the channel id, in any case.
- **Reason** shows one state.
- **Mapping** shows the notes of one show or film.
- **Show** picks between the notes you have not ignored and all of them.

The filters are part of the page's address, so a filtered view can be bookmarked or reloaded.

Each row has the video's title, its mapping, length, upload date and the reason, with **Pin**, **Ignore** and a link to the video on YouTube.

## Pinning

**Pin** on a note opens a small dialog that asks where the video belongs:

- for a show, a season and an episode, filled in with what the matcher read from the title when it read anything
- for a daily show, an air date as `YYYY-MM-DD`, as Sonarr lists the episode
- for a film, nothing: the video becomes the film

A pin wins over the matcher. Saving it wakes the matcher, which releases the video for that episode within about half a minute. A new pin for the same episode replaces the old one. A pin also works on a video with no note, and on an older upload whose note was dropped (see [How long notes are kept](#how-long-notes-are-kept)): use `POST /admin/api/pins` in the [Admin API](../../reference/admin-api/#unmatched-and-pins) with the video id.

A pinned episode is released even when it is older than the mapping's back episode limit. Removing a pin (`DELETE /admin/api/pins/{id}`) hands the episode back to the matcher on its next run. The web UI has no list of pins yet: `GET /admin/api/pins?mapping_id=<id>` lists them.

Pins are for one video at a time. When the same mistake repeats for a whole show, a rule fixes it for good: an alias for a name the channel uses, an exclude for a word on its extras, or an episode regex for its own title style. Try the change with **Test against channel** in the mapping drawer (see [Mappings and matching](../mappings-and-matching/#test-against-channel)).

## Ignoring

**Ignore** hides a note you have looked at and do not want to act on, a clip that looks like an episode for example. The note stays in the database with its state and stays hidden on later runs. Pick **All, with ignored** under **Show** to see ignored notes again, and **Show again** on one to bring it back.

## How long notes are kept

A note is kept while its video was uploaded within `[matcher] note_retention`, 180 days by default. Older uploads get no note, whatever their state, except `pending` ones, which still wait on a lookup. The rule goes by the upload date alone, so a dropped note does not come back on a later run.

Half a year leaves plenty of time to pin an unsure upload, and keeps a long back catalogue's capped and duplicate notes from filling the table. Set `note_retention = "0s"` to keep every note. A pin still works on any video, however old.

## When the list is long

A long list for one show usually has a single cause. Filter by the mapping and read the reasons:

- Many `unsure` with "does not name the show": the channel writes the show's name differently from TheTVDB. Add the channel's spelling as an alias.
- Many clips among the `unsure` notes: add the word the channel puts on its clips to the excludes.
- Many `pending`: lookups are slow or failing. Check the Health page and [Troubleshooting](../../operations/troubleshooting/#youtube-lookups-fail).
- Many `capped`: that is the back episode limit doing its job. Raise it if you want more of them.
