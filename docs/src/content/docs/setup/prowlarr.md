---
title: Prowlarr
description: Why Tubarr is added to Sonarr and Radarr directly, not through Prowlarr.
---

Tubarr is not supported through Prowlarr. Add it to Sonarr and Radarr directly as a Newznab indexer, as [Sonarr and Radarr](../sonarr-and-radarr/) describes, and keep using Prowlarr for your other indexers. We have not tested Tubarr behind Prowlarr, and the setup below depends on settings that Prowlarr manages on its own.

## Why not

Prowlarr is built for general indexers: it searches them for any title and syncs one copy of each indexer into every app. Tubarr is a different kind of source.

- **It only knows the titles you tag.** Tubarr releases nothing until a title carries the `youtube` tag in Sonarr or Radarr and has its official channels set. A search from Prowlarr itself, or from an app that has no tagged titles, finds nothing.
- **The arr's copy of the indexer needs its own settings.** Tubarr's indexer has to carry the tag and use the Tubarr download client, so that only tagged titles search it and its releases never go to your real download client. An indexer that Prowlarr syncs into an arr gets the settings Prowlarr gives it, and Prowlarr writes them again on each sync.
- **Pushes name the indexer.** Tubarr hands releases to the arr with the indexer name in `[arr] indexer` (default `Tubarr`). Prowlarr names the indexers it syncs after its own entry, usually with ` (Prowlarr)` added, so the names no longer match unless you change one of them.
- **Nothing is gained.** Prowlarr saves you from typing an indexer's settings into each app. Tubarr needs the same few fields in at most two apps, and its download client has to be added to each app by hand either way.

## If you already run Prowlarr

Nothing changes for your other indexers. Add Tubarr in Sonarr and Radarr by hand, and do not add it in Prowlarr. As far as we know, Prowlarr only manages the indexers it added itself and leaves one added by hand alone, even with full sync on.

If you have already added Tubarr in Prowlarr, remove it there, let Prowlarr sync, and check that Sonarr and Radarr each have exactly one Tubarr indexer, the one you added by hand, with the `youtube` tag.

If you try Tubarr through Prowlarr anyway and get it working, we would like to hear what you had to change.
