#!/usr/bin/env python3
"""Demo data for scripts/demo.sh: a catalog for the fake YouTube, then a seeded Tubarr.

    demo_seed.py catalog <catalog.json>        # the few videos /play can really stream
    demo_seed.py channels                      # the [channels] config of the made-up channels
    demo_seed.py radarr <port> <films.json>    # a fake Radarr that takes Tubarr's pushes
    demo_seed.py seed <tubarr_url> <db> <key_file> <films.json>  # mappings, notes, releases, drafts
    demo_seed.py activity <tubarr_url> <key_file>    # searches, plays and pushes, after a restart

Mappings, aliases, drafts, the admin user, searches, plays and pushes go through Tubarr's own API.
What the API cannot create (inactive mappings, matcher notes, releases, grabs, channel videos,
backfill state, unplayable videos) is written into the database with plain SQL. Tubarr runs
without Sonarr. The fake Radarr tags the seeded films, so they get a Radarr id and can be pushed,
but lists them unmonitored, so the matcher has no wanted titles and leaves the seeded notes and
releases alone. Everything here is made up, and the same every run (fixed random seed).
"""

import json
import os
import random
import sqlite3
import string
import sys
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

USERNAME = "demo"
PASSWORD = "demo-password"
NOW = int(time.time())
DAY = 86400
rng = random.Random(20261024)


def channel_id(name):
    r = random.Random(name)
    return "UC" + "".join(r.choice(string.ascii_letters + string.digits + "_-") for _ in range(22))


def video_id():
    return "".join(rng.choice(string.ascii_letters + string.digits + "_-") for _ in range(11))


CHANNELS = {
    "ARY Digital": ("Urdu", 300),
    "HUM TV": ("Urdu", 300),
    "Har Pal Geo": ("Urdu", 300),
    "Green Entertainment": ("Urdu", 300),
    "Express TV": ("Urdu", 300),
    "Shemaroo Movies": ("Hindi", 330),
    "Goldmines Bollywood": ("Hindi", 330),
    "Rajshri": ("Hindi", 330),
    "Channel 4 Entertainment": ("English", 0),
    "Long Table Talks": ("English", 0),
    "Tea Break Comedy Club": ("English", 0),
}
CH = {name: channel_id(name) for name in CHANNELS}

# The catalog's playable videos: the play fixtures, served by the fake YouTube.
PLAYABLE = ["dmPlay00001", "dmPlay00002", "dmPlay00003", "dmPlay00004"]

SHOWS = [
    # title, channel, language, episodes released, extras
    ("Kabhi Main Kabhi Tum", "ARY Digital", "Urdu", 34, {"backfill_limit": 10}),
    ("Jaan Nisar", "Har Pal Geo", "Urdu", 65, {"backfill_limit": 20}),
    ("Ishq Murshid", "HUM TV", "Urdu", 31, {"excludes": ["Teaser", "Promo", "OST"]}),
    ("Mann Mast Malang", "Har Pal Geo", "Urdu", 40, {}),
    ("Sunn Mere Dil", "Har Pal Geo", "Urdu", 22, {"backfill_limit": 5}),
    ("Tere Bin", "Har Pal Geo", "Urdu", 58, {"backfill_limit": 12}),
    ("Bismil", "ARY Digital", "Urdu", 18, {}),
    ("Iqtidar", "Green Entertainment", "Urdu", 27, {"min_duration_secs": 1800}),
    ("Meem Se Mohabbat", "HUM TV", "Urdu", 28, {}),
    ("Pagal Khana", "Green Entertainment", "Urdu", 12, {}),
    ("Kaffara", "Har Pal Geo", "Urdu", 90, {"playlist_id": "PLdemoKaffara000000000000000000000"}),
    ("Mohabbat Ki Akhri Kahani: The Long Road Back Home to Lahore", "Express TV", "Urdu", 9, {}),
    ("Noor Jahan", "ARY Digital", "Urdu", 26, {"backfill_limit": 0}),
    ("Faraar", "Green Entertainment", "Urdu", 14, {}),
    ("Kuch Ankahi", "ARY Digital", "Urdu", 0, {}),
    ("Taj Mahal Diaries", "Rajshri", "Hindi", 16, {}),
    ("Yeh Rishta Kya Kehlata Hai Classic Rewind", "Rajshri", "Hindi", 41, {}),
    ("Grand Designs: The Streets", "Channel 4 Entertainment", "English", 6, {}),
    ("Baby Baji Ki Bahuwein", "ARY Digital", "Urdu", 73, {}),
    ("Jhok Sarkar", "HUM TV", "Urdu", 3, {}),
]
DAILIES = [
    ("Good Morning Pakistan", "ARY Digital", "Urdu", 60),
    ("Hasna Mana Hai with Tabish Hashmi", "Har Pal Geo", "Urdu", 45),
]
FILMS = [
    ("Teefa in Trouble", 2018, "Urdu", "ARY Digital"),
    ("Parwaaz Hai Junoon", 2018, "Urdu", "HUM TV"),
    ("Hum Aapke Hain Koun..!", 1994, "Hindi", "Rajshri"),
    ("Maine Pyar Kiya", 1989, "Hindi", "Rajshri"),
    ("Vivah", 2006, "Hindi", "Rajshri"),
    ("Dilwale Dulhania Le Jayenge (Remastered Director's Cut With Bonus Songs)", 1995, "Hindi", "Shemaroo Movies"),
    ("The Legend of Maula Jatt", 2022, "Punjabi", "Goldmines Bollywood"),
    ("Joyland", 2022, "Urdu", "Green Entertainment"),
]
# Video podcasts, made with the podcast preset: numbered in the title, matched by that number.
PODCASTS = [
    # title, channel, language, newest running number, episodes released
    ("The Long Table with Sam Okafor", "Long Table Talks", "English", 412, 10),
    ("Tea Break Comedy Hour", "Tea Break Comedy Club", "English", 187, 8),
]
GUESTS = ["Priya Raman", "Tom Gallagher", "Dr. Helen Marsh", "Kwame Asante", "Lucy Fenwick",
          "Omar Siddiqui", "Grace Liu", "Callum Reid", "Nadia Petrova", "Ben Whitlock"]
TOPICS = ["building a bakery from nothing", "why we stopped reading", "the science of sleep",
          "a life in stand-up", "growing up between two cities", "what running taught me",
          "the worst gig ever", "learning to cook at forty"]
INACTIVE = {"Faraar", "Taj Mahal Diaries", "Maine Pyar Kiya", "Hasna Mana Hai with Tabish Hashmi"}
# Uploads of titles with no mapping, drafted through the API the way a new title is.
DRAFT_SHOWS = [("Dil Ka Kya Karein", "HUM TV", 8), ("Ae Ishq-e-Junoon", "ARY Digital", 12),
               ("Rang Mahal", "Har Pal Geo", 5)]
DRAFT_FILMS = [("Tich Button", 2022, "Urdu", "ARY Digital"), ("Uunchai", 2022, "Hindi", "Rajshri"),
               ("Jawani Phir Nahi Ani", 2015, "Urdu", "ARY Digital")]
CAST = ["Fahad Mustafa", "Hania Aamir", "Wahaj Ali", "Yumna Zaidi", "Ayeza Khan", "Danish Taimoor",
        "Sajal Aly", "Bilal Abbas Khan", "Durefishan Saleem", "Feroze Khan", "Saba Qamar"]


def ep_title(show, channel, ep, when, kind="Episode"):
    """A YouTube title the way the channels write them: long, with cast and dates."""
    a, b = rng.sample(CAST, 2)
    date = time.strftime("%-d %B %Y", time.gmtime(when))
    sub = rng.choice(["[Eng Sub]", "- [ CC ]", "(Eng Sub)", ""])
    tail = rng.choice([f"- {channel}", f"- {channel} Drama", f"| {channel}", "- Presented by Ariel & Surf Excel"])
    return f"{show} {kind} {ep} {sub} {a} - {b} - {date} {tail}".replace("  ", " ")


def pod_title(show, n, when, clip=False):
    """A podcast upload's title: the running number, a guest, sometimes a topic."""
    guest = rng.choice(GUESTS)
    if clip:
        return f"{guest} on the worst advice they ever got | {show} Clip"
    topic = rng.choice([f" on {t}" for t in TOPICS] + [""])
    return f"{show} #{n} - {guest}{topic}"


def cmd_catalog(path):
    videos = [
        {"id": vid, "channel_id": CH["ARY Digital"], "title": ep_title("Kabhi Main Kabhi Tum", "ARY Digital", 30 + i, NOW - i * DAY),
         "duration": 2280, "published": NOW - i * DAY, "video": "video-137.mp4", "audio": "audio-140.mp4",
         "subs": ["en", "ur"]}
        for i, vid in enumerate(PLAYABLE)
    ]
    with open(path, "w", encoding="utf-8") as f:
        json.dump({"videos": videos}, f, indent=1)


class Api:
    def __init__(self, base, key):
        self.base, self.key = base.rstrip("/"), key

    def call(self, path, method="GET", body=None, headers=None, key=True):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.base + path, data=data, method=method)
        if key:
            req.add_header("X-Api-Key", self.key)
        if data is not None:
            req.add_header("Content-Type", "application/json")
        for k, v in (headers or {}).items():
            req.add_header(k, v)
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                payload = resp.read()
                return resp.status, payload
        except urllib.error.HTTPError as e:
            return e.code, e.read()

    def json(self, path, method="GET", body=None):
        status, payload = self.call(path, method, body)
        if status >= 300:
            sys.exit(f"demo seed: {method} {path} answered {status}: {payload[:300]!r}")
        return json.loads(payload) if payload else None


def cmd_channels():
    country = {"Urdu": "Pakistan", "Hindi": "India", "English": "United Kingdom"}
    for name, (language, offset) in CHANNELS.items():
        print(f'\n[channels.{CH[name]}]\nnetwork = "{name}"\ncountry = "{country[language]}"\n'
              f'language = "{language}"\nutc_offset_mins = {offset}')


def cmd_seed(base, db_path, key_file, films_path):
    with open(key_file, encoding="utf-8") as f:
        api = Api(base, f.read().strip())
    # demo.sh sets TUBARR_SETUP_CODE for Tubarr, which then takes it instead of a random code.
    status, body = api.call("/auth/setup", "POST", {"username": USERNAME, "password": PASSWORD,
                                                   "display_name": "Demo Admin",
                                                   "setup_code": os.environ["TUBARR_SETUP_CODE"]},
                            headers={"X-Requested-With": "tubarr"}, key=False)
    if status >= 300:
        sys.exit(f"demo seed: setup answered {status}: {body[:200]!r}")

    mappings = []  # (id, kind, title, channel, language, extra)
    tvdb = 380000
    for title, channel, language, released, extra in SHOWS:
        tvdb += rng.randint(100, 4000)
        body = {"kind": "show", "title": title, "tvdb_id": tvdb, "language": language,
                "channel_ids": [CH[channel]], **extra}
        m = api.json("/admin/api/mappings", "POST", body)
        mappings.append((m["id"], "show", title, channel, language, released))
    for title, channel, language, newest, released in PODCASTS:
        tvdb += rng.randint(100, 4000)
        m = api.json("/admin/api/mappings", "POST", {
            "kind": "show", "title": title, "tvdb_id": tvdb, "language": language,
            "channel_ids": [CH[channel]], "preset": "podcast"})
        mappings.append((m["id"], "podcast", title, channel, language, (newest, released)))
    for title, channel, language, released in DAILIES:
        tvdb += rng.randint(100, 4000)
        m = api.json("/admin/api/mappings", "POST", {
            "kind": "daily", "title": title, "tvdb_id": tvdb, "language": language,
            "channel_ids": [CH[channel]], "utc_offset_mins": CHANNELS[channel][1],
            "excludes": ["Highlights", "Promo"]})
        mappings.append((m["id"], "daily", title, channel, language, released))
    tmdb = 400000
    radarr = []
    for title, year, language, channel in FILMS:
        tmdb += rng.randint(100, 9000)
        m = api.json("/admin/api/mappings", "POST", {
            "kind": "film", "title": title, "tmdb_id": tmdb, "language": language,
            "channel_ids": [CH[channel]], "min_duration_secs": 4800})
        mappings.append((m["id"], "film", title, channel, language, year))
        if title not in INACTIVE:
            radarr.append({"id": len(radarr) + 1, "title": title, "year": year, "tmdbId": tmdb,
                           "originalLanguage": {"id": 0, "name": language}, "monitored": False,
                           "hasFile": False, "tags": [1]})
    for mid, kind, title, *_ in mappings[:6]:
        if kind == "show":
            api.json(f"/admin/api/mappings/{mid}/aliases", "PUT",
                     {"aliases": [title.upper(), f"{title} Drama", f"{title} Full Episode"]})

    db = sqlite3.connect(db_path, timeout=30)
    with db:
        seed_sql(db, mappings)
        draft_films = draft_sources(db)
    db.close()
    for title, channel, _ in DRAFT_SHOWS:
        api.json("/admin/api/drafts/show", "POST", {"channel_id": CH[channel], "title": title})
    for vid in draft_films:
        api.json("/admin/api/drafts/film", "POST", {"video_id": vid})
    # The fake Radarr reads this on every call, so the next start's sync gives the films their ids.
    with open(films_path, "w", encoding="utf-8") as f:
        json.dump(radarr, f, indent=1)


def cmd_activity(base, key_file):
    with open(key_file, encoding="utf-8") as f:
        api = Api(base, f.read().strip())
    activity(api)
    pushes(api)
    # An unchanged edit wakes the matcher, so the Health page has a run. Without Sonarr and with every
    # film unmonitored in the fake Radarr it has no wanted titles and changes nothing.
    first = api.json("/admin/api/mappings")[0]
    api.json(f"/admin/api/mappings/{first['id']}", "PATCH", {"language": first["language"]})
    counts = {
        "mappings": len(api.json("/admin/api/mappings")),
        # Both lists are paged: count by their totals. `ignored=true` lists hidden notes as well.
        "notes": api.json("/admin/api/unmatched?ignored=true&limit=1")["total"],
        "grabs": api.json("/admin/api/activity?limit=1")["total"],
        "events": api.json("/admin/api/activity/events?limit=1")["total"],
        "pushes": push_events(api),
        "drafts": drafts(api),
    }
    print("demo seed: " + ", ".join(f"{v} {k}" for k, v in counts.items()), flush=True)


NOTE_REASONS = {
    "unsure": [
        "the title names the show but no episode number, pin it to its episode",
        'the title is close to "{show}" but not the same, pin it if it is this show',
        "S01E{ep:02} is pinned to another video",
    ],
    "duplicate": ["another upload of S01E{ep:02}, kept {other}"],
    "partial": [
        "part {part} of S01E{ep:02}, waiting for the other parts' lookups",
        "part {part} of S01E{ep:02} on another channel",
        "part {part} of S01E{ep:02}, the 2 parts add up to 31 min, under the 35 min floor",
    ],
    "pending": [
        "duration unknown until the resolver looks the video up",
        "video height unknown until the resolver looks the video up",
    ],
    "capped": [
        "uploaded before the show was added and the backfill limit keeps only the newest {limit} "
        "back episodes, pin it or raise the limit to release it",
    ],
}


def seed_sql(db, mappings):
    for title in INACTIVE:
        db.execute("UPDATE mappings SET active = 0 WHERE title = ?", (title,))
    limits = {t: e.get("backfill_limit") for t, _, _, _, e in SHOWS}
    playable = list(PLAYABLE)
    release_n = 0
    for mid, kind, title, channel, language, released in mappings:
        cid = CH[channel]
        if kind == "podcast":
            podcast_sql(db, mid, title, cid, language, *released)
            continue
        if kind == "film":
            vid = video_id()
            when = NOW - rng.randint(30, 900) * DAY
            name = f"{title} ({released}) Full Movie | {language} Superhit Film | 4K Remastered | {channel}"
            add_video(db, vid, cid, name, 8400 + rng.randint(0, 3000), when)
            if rng.random() < 0.75:
                add_release(db, mid, vid, name, language, when, year=released)
                release_n += 1
            else:
                note(db, mid, vid, "pending", None, None, None, NOTE_REASONS["pending"][0])
            dup = video_id()
            add_video(db, dup, cid, f"{title} Full Movie HD (Old Print) {channel} Classics", 7900, when - 400 * DAY)
            note(db, mid, dup, "duplicate", None, None, None, f"another upload of the film, kept {vid}")
            continue
        for ep in range(1, released + 1):
            when = NOW - (released - ep) * (DAY if kind == "daily" else 7 * DAY // 2) - 3600 * 20
            vid = playable.pop() if playable and kind == "show" else video_id()
            name = ep_title(title, channel, ep, when)
            add_video(db, vid, cid, name, 2100 + rng.randint(0, 900), when)
            air = time.strftime("%Y-%m-%d", time.gmtime(when)) if kind == "daily" else None
            add_release(db, mid, vid, name, language, when, ep=None if air else ep, air_date=air)
            release_n += 1
        notes_for(db, mid, kind, title, channel, released, limits.get(title))
    backfills(db, mappings)
    grabs(db)
    unplayable(db)


def podcast_sql(db, mid, title, cid, language, newest, released):
    """Weekly numbered episodes, TVDB style (a season per year), and the notes a podcast gets."""
    for i in range(released):
        n = newest - i
        when = NOW - i * 7 * DAY - 3600 * 14
        vid = video_id()
        name = pod_title(title, n, when)
        add_video(db, vid, cid, name, 3600 + rng.randint(0, 5400), when)
        # TVDB numbers a weekly podcast by week of the year, the running number is only in titles.
        year, week = (int(x) for x in time.strftime("%Y %W", time.gmtime(when)).split())
        add_release(db, mid, vid, name, language, when, ep=week + 1, season=year)
    for i in range(rng.randint(5, 9)):
        when = NOW - rng.randint(0, 60) * DAY - rng.randint(0, DAY)
        vid = video_id()
        state = rng.choice(["capped", "capped", "duplicate", "pending"])
        n = newest - released - i if state == "capped" else newest - rng.randint(0, released - 1)
        name = pod_title(title, n, when)
        add_video(db, vid, cid, name, None if state == "pending" else 4200, when)
        reason = {
            "capped": NOTE_REASONS["capped"][0].format(limit=10),
            "duplicate": f"another upload of #{n}, kept {video_id()}",
            "pending": NOTE_REASONS["pending"][0],
        }[state]
        note(db, mid, vid, state, None, None, None, reason)
    # Clips: under the podcast's 20 min floor and named Clip, so they are left out without a note.
    for _ in range(3):
        when = NOW - rng.randint(0, 30) * DAY
        add_video(db, video_id(), cid, pod_title(title, 0, when, clip=True), 300 + rng.randint(0, 600), when)


def notes_for(db, mid, kind, title, channel, released, limit):
    cid = CH[channel]
    n = rng.randint(8, 22)
    for _ in range(n):
        state = rng.choices(["unsure", "duplicate", "partial", "pending", "capped"],
                            weights=[4, 3, 2, 2, 3 if limit is not None else 0])[0]
        ep = rng.randint(1, max(released, 1) + 3)
        vid = video_id()
        when = NOW - rng.randint(0, 120) * DAY - rng.randint(0, DAY)
        part = rng.randint(1, 3) if state == "partial" else None
        word = {"partial": f"Episode {ep} Part {part}", "unsure": "Mega Episode",
                "capped": f"Episode {ep}"}.get(state, f"Episode {ep}")
        name = ep_title(title, channel, word.split()[-1], when, " ".join(word.split()[:-1]) or "Episode")
        if state == "unsure" and rng.random() < 0.4:
            name = f"{title} Last Episode Promo Reaction & Review | Fans Can't Believe What Happened | {channel}"
        add_video(db, vid, cid, name, None if state == "pending" else 2400, when)
        reason = rng.choice(NOTE_REASONS[state]).format(
            show=title, ep=ep, part=part, other=video_id(), limit=limit)
        season = None if kind == "daily" else 1
        note(db, mid, vid, state, season if state != "unsure" else None,
             ep if state != "unsure" else None, part, reason, ignored=rng.random() < 0.08)


def draft_sources(db):
    """Uploads of the titles to draft. Returns the film uploads' video ids."""
    for title, channel, episodes in DRAFT_SHOWS:
        for ep in range(1, episodes + 1):
            when = NOW - (episodes - ep) * 7 * DAY // 2 - 3600 * 20
            add_video(db, video_id(), CH[channel], ep_title(title, channel, ep, when), 2400, when)
    films = []
    for title, year, language, channel in DRAFT_FILMS:
        vid = video_id()
        name = f"{title} ({year}) Full Movie | {language} Comedy Film | {channel}"
        add_video(db, vid, CH[channel], name, 7800 + rng.randint(0, 2400), NOW - rng.randint(5, 60) * DAY)
        films.append(vid)
    return films


def add_video(db, vid, cid, title, duration, when):
    db.execute(
        "INSERT OR IGNORE INTO channel_videos (video_id, channel_id, title, published_at, duration_secs, "
        "source, first_seen, last_seen, match_state, match_updated_at) VALUES (?,?,?,?,?,?,?,?,?,?)",
        (vid, cid, title, when, duration, rng.choice(["rss", "listing"]), when + 600, NOW, "new", NOW))


def add_release(db, mid, vid, title, language, when, ep=None, air_date=None, year=None, season=1):
    height = rng.choice([1080, 1080, 1080, 720, 480])
    size = rng.randint(180_000_000, 2_400_000_000) if rng.random() < 0.85 else None
    db.execute(
        "INSERT INTO releases (mapping_id, video_id, season, episode, air_date, year, video_title, "
        "language, height, size, published_at, created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
        (mid, vid, season if ep else None, ep, air_date, year, title, language, height, size, when,
         when + 1800))


def note(db, mid, vid, state, season, episode, part, reason, ignored=False):
    when = NOW - rng.randint(60, 30 * DAY)
    db.execute(
        "INSERT OR IGNORE INTO unmatched (mapping_id, video_id, state, season, episode, part, reason, "
        "created_at, updated_at, ignored) VALUES (?,?,?,?,?,?,?,?,?,?)",
        (mid, vid, state, season, episode, part, reason, when, when, int(ignored)))


def backfills(db, mappings):
    shows = [(mid, title) for mid, kind, title, *_ in mappings if kind in ("show", "podcast")]
    errors = [None, None, None, "yt-dlp timed out after 60s",
              "HTTP Error 429: Too Many Requests (YouTube pushed back, lookups paused for 15m)",
              "ERROR: [youtube:tab] This channel does not have a search tab"]
    for i, (mid, title) in enumerate(shows):
        started = NOW - rng.randint(1, 13) * DAY
        db.execute(
            "INSERT OR REPLACE INTO mapping_backfill (mapping_id, key, started_at, last_run_at, "
            "last_error, last_found, done) VALUES (?,?,?,?,?,?,?)",
            (mid, f"demo:{mid}", started, NOW - rng.randint(600, 20 * 3600), errors[i % len(errors)],
             rng.choice([0, 12, 87, 400, 1234]), int(i % 3 == 0)))


def grabs(db):
    rows = db.execute(
        "SELECT r.video_id, m.title, m.kind, r.season, r.episode, r.air_date, r.year, r.height, r.language "
        "FROM releases r JOIN mappings m ON m.id = r.mapping_id ORDER BY r.published_at DESC LIMIT 160"
    ).fetchall()
    for i, (vid, title, kind, season, ep, air, year, height, language) in enumerate(rows):
        dotted = ".".join(title.replace(":", "").replace("'", "").split())
        if kind == "film":
            name = f"{dotted}.{year}.{height}p.WEB-DL.AAC2.0.H.264.{language.upper()}-Tubarr"
            category, folder = "movies", "/downloads/tubarr/movies"
        elif air:
            name = f"{dotted}.{air.replace('-', '.')}.{height}p.WEB-DL.AAC2.0.H.264.{language.upper()}-Tubarr"
            category, folder = "tv", "/downloads/tubarr/tv"
        else:
            name = f"{dotted}.S{season:02}E{ep:02}.{height}p.WEB-DL.AAC2.0.H.264.{language.upper()}-Tubarr"
            category, folder = "tv", "/downloads/tubarr/tv"
        failed = i % 11 == 5
        db.execute(
            "INSERT INTO grabs (nzo_id, release_name, video_id, category, status, storage_path, "
            "created_at, fail_message) VALUES (?,?,?,?,?,?,?,?)",
            (f"SABnzbd_nzo_demo{i:05d}", name, vid, category, "failed" if failed else "completed",
             f"{folder}/{name}", NOW - i * 5400 - rng.randint(0, 3000),
             "the video is unplayable: Sign in to confirm your age. This video may be inappropriate "
             "for some users." if failed else None))


def unplayable(db):
    reasons = ["Video unavailable. This video is private",
               "Sign in to confirm your age. This video may be inappropriate for some users.",
               "no H.264 1080p or 720p video-only format and no AAC audio-only format",
               "Video unavailable. This video has been removed by the uploader"]
    for vid, in db.execute("SELECT video_id FROM releases ORDER BY id DESC LIMIT 7 OFFSET 40").fetchall():
        db.execute("INSERT OR IGNORE INTO unplayable_videos (video_id, reason, checked_at, retry) VALUES (?,?,?,?)",
                   (vid, rng.choice(reasons), NOW - rng.randint(0, 3 * DAY), rng.choice([0, 1])))


def activity(api):
    """Real Newznab searches and /play starts, which the in-memory Activity log records."""
    db_rows = api.json("/admin/api/mappings")
    shows = [m for m in db_rows if m["kind"] == "show"]
    films = [m for m in db_rows if m["kind"] == "film"]
    k = api.key
    queries = ["/api?t=tvsearch&cat=5000,5030,5040&extended=1&offset=0&limit=100"]
    for m in shows:
        queries.append(f"/api?t=tvsearch&tvdbid={m['tvdb_id']}&season=1&ep={rng.randint(1, 40)}&cat=5000,5040")
        if rng.random() < 0.4:
            queries.append(f"/api?t=tvsearch&tvdbid={m['tvdb_id']}&season=1&cat=5000")
    for m in films:
        queries.append(f"/api?t=movie&tmdbid={m['tmdb_id']}&cat=2000,2040&extended=1")
    queries += ["/api?t=search&q=Kabhi%20Main%20Kabhi%20Tum%20Mega%20Episode",
                "/api?t=movie&imdbid=tt0112870&cat=2000",
                "/api?t=search&q=a%20title%20nobody%20has%20uploaded%20anywhere%20at%20all"]
    rng.shuffle(queries)
    for q in queries:
        api.call(q + f"&apikey={k}", key=False)
    clients = ["Lavf/61.7.100", "Jellyfin Media Player/1.11.1", "Jellyfin-Server/10.11.2",
               "Infuse-Direct/8.1.4 (Apple TV)", "Swiftfin/1.3.0 (iOS 19.1)"]
    for vid in PLAYABLE + PLAYABLE[:2] + ["dmGoneVid01", "dmPlay00001,dmPlay00002"]:
        api.call(f"/play/{vid}", headers={"Range": "bytes=0-4095", "User-Agent": rng.choice(clients)},
                 key=False)


def drafts(api):
    return api.json("/admin/api/drafts?limit=1")["total"]


def push_events(api):
    """How many pushes the Activity page lists."""
    return api.json("/admin/api/activity/events?kind=arr_push&limit=1")["total"]


def pushes(api):
    """The catch-up push sends each released film to the fake Radarr, one a second."""
    queued = api.json("/admin/api/arr/push-released", "POST")["queued"]
    for _ in range(4 * queued + 20):
        if push_events(api) >= queued:
            return
        time.sleep(0.5)
    sys.exit(f"demo seed: only {push_events(api)} of {queued} pushes were sent")


# The fake Radarr's answers to pushes, in turn, as Radarr's decision engine words them.
PUSH_ANSWERS = [
    [{"approved": True}],
    [{"approved": False, "temporarilyRejected": True,
      "rejections": ["Delay Profile: waiting 6 hours for a preferred protocol"]}],
    [{"approved": True}],
    [{"approved": False, "rejections": ["Not enough free space"]}],
    [{"approved": False, "rejections": ["Release is blocklisted"]}],
    [],
]


class FakeRadarr(BaseHTTPRequestHandler):
    """The calls Tubarr makes to Radarr: tag, film list, one film, root folders, clients, push."""

    films_path = ""
    pushed = 0

    def log_message(self, fmt, *args):
        sys.stderr.write("fake-radarr " + (fmt % args) + "\n")

    def answer(self, body, code=200):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def films(self):
        if not os.path.exists(self.films_path):
            return []
        with open(self.films_path, encoding="utf-8") as f:
            return json.load(f)

    def do_GET(self):
        path = self.path.split("?")[0]
        one = path.removeprefix("/api/v3/movie/")
        if path == "/api/v3/tag":
            return self.answer([{"id": 1, "label": "youtube"}])
        if path == "/api/v3/movie":
            return self.answer(self.films())
        if one.isdigit():
            film = next((f for f in self.films() if f["id"] == int(one)), None)
            # Monitored only here: a push reads one film, the matcher reads the list.
            return self.answer({**film, "monitored": True}) if film else self.answer({}, 404)
        if path in ("/api/v3/movie/lookup", "/api/v3/rootfolder"):
            return self.answer([])
        if path == "/api/v3/downloadclient":
            return self.answer([{"id": 3, "name": "Tubarr", "protocol": "usenet", "enable": True}])
        self.answer({"message": "NotFound"}, 404)

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        if self.path.split("?")[0] != "/api/v3/release/push":
            return self.answer({"message": "NotFound"}, 404)
        answer = PUSH_ANSWERS[FakeRadarr.pushed % len(PUSH_ANSWERS)]
        FakeRadarr.pushed += 1
        self.answer(answer)


def cmd_radarr(port, films_path):
    FakeRadarr.films_path = films_path
    ThreadingHTTPServer(("127.0.0.1", int(port)), FakeRadarr).serve_forever()


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "catalog":
        return cmd_catalog(sys.argv[2])
    if len(sys.argv) == 2 and sys.argv[1] == "channels":
        return cmd_channels()
    if len(sys.argv) == 4 and sys.argv[1] == "radarr":
        return cmd_radarr(sys.argv[2], sys.argv[3])
    if len(sys.argv) == 6 and sys.argv[1] == "seed":
        return cmd_seed(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5])
    if len(sys.argv) == 4 and sys.argv[1] == "activity":
        return cmd_activity(sys.argv[2], sys.argv[3])
    sys.exit(__doc__)


if __name__ == "__main__":
    main()
