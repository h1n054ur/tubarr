#!/usr/bin/env python3
"""Drive a running Tubarr with concurrent plays and searches and record /metrics into a CSV.

Each worker loops: a playback start (Range from byte 0) of a demo video, a seek into the middle
of it, and a Newznab search. Every `--every` seconds one row goes to the CSV with Tubarr's
resident memory, its counters and the client side latency of the plays in that interval.

    scripts/soak/soak.py --url http://127.0.0.1:18480 --key-file .demo/secrets/tubarr_api_key \\
        --workers 2 --minutes 5 --out soak.csv

The ids default to the demo's playable videos (scripts/demo_seed.py PLAYABLE). Only the standard
library is used.
"""

import argparse
import csv
import random
import statistics
import sys
import threading
import time
import urllib.error
import urllib.request

DEMO_IDS = ["dmPlay00001", "dmPlay00002", "dmPlay00003", "dmPlay00004", "dmPlay00001,dmPlay00002"]
COLUMNS = [
    "elapsed_s", "rss_mib", "plays", "plays_failed", "play_p50_ms", "play_p95_ms",
    "first_byte_avg_ms", "lookups_ok", "lookups_failed", "lookups_cached", "lookup_avg_ms",
    "ytdlp_processes", "play_mib_sent", "searches",
]


class Stats:
    def __init__(self):
        self.lock = threading.Lock()
        self.latencies = []
        self.failed = 0
        self.searches = 0

    def take(self):
        with self.lock:
            out = (self.latencies, self.failed, self.searches)
            self.latencies, self.failed, self.searches = [], 0, 0
            return out


def get(url, headers=None, timeout=60):
    request = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def worker(args, key, stop, stats):
    rng = random.Random()
    while not stop.is_set():
        vid = rng.choice(args.ids)
        started = time.monotonic()
        try:
            status, _ = get(f"{args.url}/play/{vid}", {"Range": "bytes=0-65535"})
        except OSError:
            status = 0
        took = (time.monotonic() - started) * 1000
        with stats.lock:
            if 200 <= status < 300:
                stats.latencies.append(took)
            else:
                stats.failed += 1
        try:
            status, _ = get(f"{args.url}/play/{vid}", {"Range": "bytes=200000-265535"})
            get(f"{args.url}/api?t=tvsearch&cat=5000&limit=20&apikey={key}")
            with stats.lock:
                stats.searches += 1
        except OSError:
            pass
        stop.wait(args.pause)


def scrape(args, key):
    status, body = get(f"{args.url}/metrics", {"Authorization": f"Bearer {key}"}, timeout=30)
    if status != 200:
        sys.exit(f"soak: /metrics answered {status}, is [metrics] enabled?")
    values = {}
    for line in body.decode().splitlines():
        if line and not line.startswith("#"):
            name, _, value = line.rpartition(" ")
            values[name] = float(value)
    return values


def percentile(values, q):
    if not values:
        return ""
    if len(values) == 1:
        return round(values[0])
    return round(statistics.quantiles(values, n=100)[q - 1])


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--url", required=True)
    parser.add_argument("--key-file", required=True, help="the API key, sent as the metrics token")
    parser.add_argument("--workers", type=int, default=2)
    parser.add_argument("--minutes", type=float, default=5)
    parser.add_argument("--every", type=float, default=15, help="seconds between CSV rows")
    parser.add_argument("--pause", type=float, default=1, help="seconds each worker rests per loop")
    parser.add_argument("--ids", nargs="+", default=DEMO_IDS)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    args.url = args.url.rstrip("/")
    with open(args.key_file) as f:
        key = f.read().strip()

    stats, stop = Stats(), threading.Event()
    threads = [threading.Thread(target=worker, args=(args, key, stop, stats), daemon=True)
               for _ in range(args.workers)]
    start = time.monotonic()
    last = scrape(args, key)
    for t in threads:
        t.start()
    rows = []
    with open(args.out, "w", newline="") as f:
        out = csv.writer(f)
        out.writerow(COLUMNS)
        end = start + args.minutes * 60
        while time.monotonic() < end:
            time.sleep(min(args.every, max(0.0, end - time.monotonic())))
            now = scrape(args, key)
            latencies, failed, searches = stats.take()
            d = lambda name: now.get(name, 0) - last.get(name, 0)  # noqa: E731
            fb_count = d("tubarr_play_first_byte_seconds_count")
            lk_count = d("tubarr_lookup_duration_seconds_count")
            row = [
                round(time.monotonic() - start),
                round(now.get("process_resident_memory_bytes", 0) / 2**20, 1),
                len(latencies) + failed,
                failed,
                percentile(latencies, 50),
                percentile(latencies, 95),
                round(d("tubarr_play_first_byte_seconds_sum") / fb_count * 1000) if fb_count else "",
                int(d('tubarr_lookups_total{outcome="ok"}')),
                int(d('tubarr_lookups_total{outcome="failed"}')),
                int(d('tubarr_lookups_total{outcome="cache_hit"}')),
                round(d("tubarr_lookup_duration_seconds_sum") / lk_count * 1000) if lk_count else "",
                int(now.get("tubarr_ytdlp_processes", 0)),
                round(d("tubarr_play_bytes_total") / 2**20, 1),
                searches,
            ]
            out.writerow(row)
            f.flush()
            rows.append(row)
            last = now
    stop.set()
    for t in threads:
        t.join(timeout=70)

    rss = [r[1] for r in rows]
    plays = sum(r[2] for r in rows)
    failed = sum(r[3] for r in rows)
    print(f"soak: {args.workers} workers for {args.minutes} min, {plays} play starts, {failed} failed")
    if rss:
        print(f"soak: RSS {min(rss)} to {max(rss)} MiB, last {rss[-1]} MiB")
    print(f"soak: rows in {args.out}")


if __name__ == "__main__":
    main()
