#!/usr/bin/env python3
"""Drives the end-to-end check through the Sonarr, Radarr and Tubarr APIs. Started by run.sh, which
sets the E2E_* variables. Prints one PASS or FAIL line per step and writes the evidence JSON."""

import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

WORK = os.environ["E2E_WORK"]
EVIDENCE = os.environ["E2E_EVIDENCE"]
TUBARR = os.environ["E2E_TUBARR"]
# How the arr containers reach Tubarr, the same address its .strm files carry.
INNER = os.environ["E2E_TUBARR_INNER"]
SECRETS = os.path.join(WORK, "tubarr", "secrets")
DATA = os.path.join(WORK, "data")

TVDB_ID = int(os.environ.get("E2E_TVDB_ID", "451183"))  # Kabhi Main Kabhi Tum
TMDB_ID = int(os.environ.get("E2E_TMDB_ID", "370076"))  # Theri
SHOW_CHANNEL = "UCe2eFakeDramaChannel001"
FILM_CHANNEL = "UCe2eFakeFilmsChannel001"

results = []


def key(name):
    with open(os.path.join(SECRETS, f"{name}_api_key"), encoding="utf-8") as f:
        return f.read().strip()


def call(base, path, api_key, method="GET", body=None, headers=None, raw=False, timeout=30):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(base + path, data=data, method=method)
    req.add_header("X-Api-Key", api_key)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        payload = resp.read()
        if raw:
            return resp.status, resp.headers, payload
        return json.loads(payload) if payload else None


def save(name, value):
    with open(os.path.join(EVIDENCE, name), "w", encoding="utf-8") as f:
        if isinstance(value, (bytes, str)):
            f.write(value if isinstance(value, str) else value.decode("utf-8", "replace"))
        else:
            json.dump(value, f, indent=2)


def step(name, ok, detail=""):
    results.append({"step": name, "ok": bool(ok), "detail": detail})
    print(f"{'PASS' if ok else 'FAIL'} {name}{': ' + detail if detail else ''}", flush=True)
    return ok


def wait_for(what, check, timeout=240, every=3):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        try:
            last = check()
            if last:
                return last
        except (urllib.error.URLError, ConnectionError, TimeoutError, json.JSONDecodeError):
            last = None
        time.sleep(every)
    print(f"timed out waiting for {what}", flush=True)
    return None


class Arr:
    def __init__(self, name, base, version_path="/api/v3"):
        self.name, self.base, self.key, self.v = name, base, key(name), version_path

    def get(self, path):
        return call(self.base, self.v + path, self.key)

    def post(self, path, body):
        return call(self.base, self.v + path, self.key, "POST", body)

    def put(self, path, body):
        return call(self.base, self.v + path, self.key, "PUT", body)


def field_values(schema, values):
    for f in schema["fields"]:
        if f["name"] in values:
            f["value"] = values[f["name"]]
    return schema


def setup_arr(arr, root, categories, client_category, tubarr_key):
    wait_for(f"{arr.name} API", lambda: arr.get("/system/status"), timeout=180)
    arr.post("/rootfolder", {"path": root})
    tag = arr.post("/tag", {"label": "youtube"})["id"]

    mm = arr.get("/config/mediamanagement")
    mm["importExtraFiles"] = True
    mm["extraFileExtensions"] = "srt"
    arr.put(f"/config/mediamanagement/{mm['id']}", mm)

    host, port = INNER.split("//", 1)[1].split(":", 1)
    newznab = next(s for s in arr.get("/indexer/schema") if s["implementation"] == "Newznab")
    newznab.update(name="Tubarr", enableRss=True, enableAutomaticSearch=True,
                   enableInteractiveSearch=True, tags=[tag], priority=1)
    field_values(newznab, {"baseUrl": INNER, "apiPath": "/api", "apiKey": tubarr_key,
                           "categories": categories})
    indexer = arr.post("/indexer", newznab)

    sab = next(s for s in arr.get("/downloadclient/schema") if s["implementation"] == "Sabnzbd")
    sab.update(name="Tubarr", enable=True, tags=[tag], priority=1)
    field_values(sab, {"host": host, "port": int(port), "apiKey": tubarr_key, "useSsl": False,
                       "urlBase": "", client_category[0]: client_category[1]})
    client = arr.post("/downloadclient", sab)
    return tag, indexer["id"], client["id"]


def main():
    sonarr = Arr("sonarr", os.environ["E2E_SONARR"])
    radarr = Arr("radarr", os.environ["E2E_RADARR"])
    tubarr_key = key("tubarr")
    try:
        run(sonarr, radarr, tubarr_key)
    except Exception as e:  # noqa: BLE001 - every failure becomes a FAIL line with evidence
        step("driver", False, f"{type(e).__name__}: {e}")
    finally:
        collect(sonarr, radarr)
        save("results.json", results)
    sys.exit(0 if results and all(r["ok"] for r in results) else 1)


def run(sonarr, radarr, tubarr_key):
    t = lambda path, **kw: call(TUBARR, path, tubarr_key, **kw)  # noqa: E731
    if not step("tubarr up", wait_for("tubarr", lambda: t("/api/health"), timeout=30)):
        return

    # 1. Arr setup through their APIs.
    s_tag, *_ = setup_arr(sonarr, f"{DATA}/media/tv", [5000, 5040], ("tvCategory", "tv"), tubarr_key)
    r_tag, *_ = setup_arr(radarr, f"{DATA}/media/movies", [2000, 2040], ("movieCategory", "movies"),
                          tubarr_key)
    s_profile = next(p for p in sonarr.get("/qualityprofile") if p["name"] == "Any")
    lookup = sonarr.get(f"/series/lookup?term=tvdb:{TVDB_ID}")[0]
    lookup.update(qualityProfileId=s_profile["id"], rootFolderPath=f"{DATA}/media/tv",
                  monitored=True, seasonFolder=True, tags=[s_tag],
                  addOptions={"monitor": "all", "searchForMissingEpisodes": False})
    series = sonarr.post("/series", lookup)

    any_profile = next(p for p in radarr.get("/qualityprofile") if p["name"] == "Any")
    yt = dict(any_profile, name="YouTube", language={"id": -1, "name": "Any"})
    yt.pop("id")
    yt_profile = radarr.post("/qualityprofile", yt)
    movie = radarr.get(f"/movie/lookup/tmdb?tmdbId={TMDB_ID}")
    movie.update(qualityProfileId=yt_profile["id"], rootFolderPath=f"{DATA}/media/movies",
                 monitored=True, minimumAvailability="released", tags=[r_tag],
                 addOptions={"searchForMovie": False})
    movie = radarr.post("/movie", movie)
    step("arr setup", series.get("id") and movie.get("id"),
         f"series {series['title']!r} id {series['id']}, film {movie['title']!r} "
         f"({movie['year']}) id {movie['id']}, indexer and client added with tag youtube")

    # 2. Tubarr picks the tagged titles up, then gets channels and the fake uploads.
    def mappings():
        ms = t("/admin/api/mappings")
        show = next((m for m in ms if m["tvdb_id"] == TVDB_ID), None)
        film = next((m for m in ms if m["tmdb_id"] == TMDB_ID), None)
        return (show, film) if show and film else None

    found = wait_for("arr sync mappings", mappings, timeout=150)
    if not step("arr sync created mappings", found):
        return
    show, film = found
    seed_catalog(series["title"], movie["title"], movie["year"])
    show = t(f"/admin/api/mappings/{show['id']}", method="PATCH",
             body={"channel_ids": [SHOW_CHANNEL]})
    step("show language from its channel", show.get("language") == "Urdu",
         f"language {show.get('language')} (source {show.get('language_source')})")
    t(f"/admin/api/mappings/{film['id']}", method="PATCH", body={"channel_ids": [FILM_CHANNEL]})

    def released():
        show_m, film_m = mappings()
        return (show_m, film_m) if show_m["releases"] >= 3 and film_m["releases"] >= 1 else None

    got = wait_for("matcher releases", released, timeout=240, every=5)
    save("tubarr-mappings.json", t("/admin/api/mappings"))
    save("tubarr-health.json", t("/api/health"))
    if not step("watcher and matcher released", got,
                f"show releases {got[0]['releases']}, unmatched {got[0]['unmatched']}, "
                f"film releases {got[1]['releases']}" if got else ""):
        return
    for name, q in [("newznab-tvsearch.xml", f"t=tvsearch&tvdbid={TVDB_ID}&season=1"),
                    ("newznab-movie.xml", f"t=movie&tmdbid={TMDB_ID}")]:
        xml = t(f"/api?{q}&apikey={tubarr_key}", raw=True)[2]
        save(name, xml)
        if name == "newznab-tvsearch.xml":
            text = xml.decode("utf-8", "replace") if isinstance(xml, bytes) else xml
            step("show releases say URDU", "URDU-TUBARR" in text and "ENGLISH-TUBARR" not in text)

    # 3. Searches, grabs, imports.
    episodes = sonarr.get(f"/episode?seriesId={series['id']}")
    wanted = {e["episodeNumber"]: e for e in episodes
              if e["seasonNumber"] == 1 and e["episodeNumber"] in (1, 2, 3, 4)}
    sonarr.post("/command", {"name": "EpisodeSearch",
                             "episodeIds": [wanted[n]["id"] for n in (1, 2, 3)]})
    radarr.post("/command", {"name": "MoviesSearch", "movieIds": [movie["id"]]})

    def imported():
        files = sonarr.get(f"/episodefile?seriesId={series['id']}")
        mfiles = radarr.get(f"/moviefile?movieId={movie['id']}")
        return (files, mfiles) if len(files) >= 3 and mfiles else None

    got = wait_for("imports", imported, timeout=300, every=5)
    if not step("grab and import", got,
                f"{len(got[0])} episode files, {len(got[1])} film file" if got else ""):
        return
    files, mfiles = got
    save("sonarr-episodefiles.json", files)
    save("radarr-moviefiles.json", mfiles)
    for f in files + mfiles:
        q = f["quality"]["quality"]["name"]
        langs = ",".join(lang["name"] for lang in f.get("languages", []))
        step(f"imported {os.path.basename(f['path'])}", q == "WEBDL-1080p" and
             f["path"].endswith(".strm") and os.path.exists(f["path"]),
             f"quality {q}, languages {langs}")
    step("promo not grabbed", not any(f for f in files if "E04" in f["path"]))
    srts = [os.path.join(d, n) for d, _, ns in os.walk(f"{DATA}/media") for n in ns
            if n.endswith(".srt")]
    step("captions imported", srts, ", ".join(os.path.basename(s) for s in srts))

    # 4. Play the imported links.
    for f in files + mfiles:
        with open(f["path"], encoding="utf-8") as fh:
            url = fh.read().strip()
        play_check(os.path.basename(f["path"]), url, tubarr_key)


def seed_catalog(show_title, film_title, film_year):
    now = int(time.time())
    hour = 3600
    v = lambda vid, ch, title, dur, ago, video="video-137.mp4", audio="audio-140.mp4", subs=(): {  # noqa: E731
        "id": vid, "channel_id": ch, "title": title, "duration": dur,
        "published": now - ago * hour, "video": video, "audio": audio, "subs": list(subs)}
    videos = [
        v("e2eEp1aaaaa", SHOW_CHANNEL, f"{show_title} Episode 1 - Fahad Mustafa - Hania Aamir - ARY Digital", 2100, 50, subs=["en"]),
        v("e2eEp2aaaaa", SHOW_CHANNEL, f"{show_title} Episode 2 - Fahad Mustafa - Hania Aamir - ARY Digital", 2160, 40),
        v("e2eEp3pt1aa", SHOW_CHANNEL, f"{show_title} Episode 3 Part 1 - ARY Digital", 1100, 30),
        v("e2eEp3pt2aa", SHOW_CHANNEL, f"{show_title} Episode 3 Part 2 - ARY Digital", 1000, 29,
          video="video-137-part2.mp4", audio="audio-140-part2.mp4"),
        v("e2ePromo4aa", SHOW_CHANNEL, f"{show_title} Episode 4 Teaser - ARY Digital", 60, 20),
        v("e2eTheriaaa", FILM_CHANNEL, f"{film_title} ({film_year}) Full Hindi Dubbed Movie | Vijay, Samantha", 9500, 10, subs=["en"]),
    ]
    with open(os.environ["TUBARR_E2E_CATALOG"], "w", encoding="utf-8") as f:
        json.dump({"videos": videos}, f, indent=2)
    save("catalog.json", {"videos": videos})


def play_check(name, url, tubarr_key):
    # The .strm holds the address the containers use, the host goes through the published port.
    url = url.replace(INNER, TUBARR, 1)
    status, headers, _ = call(url, "", tubarr_key, method="HEAD", raw=True)
    length = int(headers.get("Content-Length", "0"))
    status_full, _, full = call(url, "", tubarr_key, raw=True)
    ok = status == 200 and status_full == 200 and length == len(full) and length > 0
    detail = [f"HEAD {status} length {length}, GET {status_full} {len(full)} bytes"]
    ids = url.rsplit("/", 1)[1].split(",")
    # Ranges at the start, the middle, the end, and a sweep of 1 KiB windows over the whole file,
    # which crosses every fragment and, for a joined release, the join between the parts.
    ranges = [(0, 99), (length // 2 - 50, length // 2 + 49), (length - 100, length - 1)]
    ranges += [(s, min(s + 1023, length - 1)) for s in range(0, length, 1024)]
    bad = 0
    for start, end in ranges:
        code, h, body = call(url, "", tubarr_key, headers={"Range": f"bytes={start}-{end}"}, raw=True)
        if code != 206 or body != full[start:end + 1] or \
                h.get("Content-Range") != f"bytes {start}-{end}/{length}":
            bad += 1
    ok = ok and bad == 0
    detail.append(f"{len(ranges)} ranges, {bad} bad")
    path = os.path.join(EVIDENCE, f"play-{ids[0]}.mp4")
    with open(path, "wb") as f:
        f.write(full)
    probe = subprocess.run(["ffprobe", "-v", "error", "-show_entries",
                            "format=duration:stream=codec_name", "-of", "json", url],
                           capture_output=True, text=True, timeout=60)
    decode = subprocess.run(["ffmpeg", "-v", "error", "-i", path, "-f", "null", "-"],
                            capture_output=True, text=True, timeout=60)
    info = json.loads(probe.stdout or "{}")
    duration = float(info.get("format", {}).get("duration", 0))
    codecs = sorted(s["codec_name"] for s in info.get("streams", []))
    ok = ok and codecs == ["aac", "h264"] and decode.returncode == 0 and not decode.stderr.strip()
    detail.append(f"ffprobe over HTTP {codecs} {duration:.2f}s, decode "
                  f"{'clean' if decode.returncode == 0 and not decode.stderr.strip() else decode.stderr.strip()[:200]}")
    detail.append(f"ids {ids}, sha256 {hashlib.sha256(full).hexdigest()[:16]}")
    step(f"play {name}", ok, "; ".join(detail))


def collect(sonarr, radarr):
    for arr in (sonarr, radarr):
        for name, path in [("history", "/history?pageSize=100&sortKey=date&sortDirection=descending"),
                           ("queue", "/queue?includeUnknownSeriesItems=true"),
                           ("indexers", "/indexer"), ("clients", "/downloadclient"),
                           ("health", "/health")]:
            try:
                value = arr.get(path)
                if name in ("indexers", "clients"):
                    for item in value:
                        item["fields"] = [f for f in item["fields"] if f["name"] != "apiKey"]
                save(f"{arr.name}-{name}.json", value)
            except Exception as e:  # noqa: BLE001
                save(f"{arr.name}-{name}.json", {"error": str(e)})
    listing = []
    for d, _, names in os.walk(DATA):
        for n in names:
            listing.append(os.path.relpath(os.path.join(d, n), DATA))
    save("data-files.json", sorted(listing))


if __name__ == "__main__":
    main()
