#!/usr/bin/env python3
"""A stand-in for YouTube for the end-to-end check: channel feeds, media files with Range, captions.

The catalog (JSON, path in TUBARR_E2E_CATALOG) lists the videos. Every request reads it again, so
the driver can add videos while the server runs. Run as a server:

    fake_youtube.py serve <port> <media_dir>

The fake yt-dlp (fake-yt-dlp) reads the same catalog and points its format URLs here.
"""

import json
import os
import re
import sys
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse
from xml.sax.saxutils import escape

from evidence import redact_text, run_secrets


def catalog():
    with open(os.environ["TUBARR_E2E_CATALOG"], encoding="utf-8") as f:
        return json.load(f)


def iso(ts):
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S+00:00")


def feed(channel_id, videos):
    entries = []
    for v in sorted(videos, key=lambda v: v["published"], reverse=True)[:15]:
        t = escape(v["title"])
        entries.append(
            f"""  <entry>
    <id>yt:video:{v['id']}</id>
    <yt:videoId>{v['id']}</yt:videoId>
    <yt:channelId>{channel_id}</yt:channelId>
    <title>{t}</title>
    <link rel="alternate" href="https://www.youtube.com/watch?v={v['id']}"/>
    <published>{iso(v['published'])}</published>
    <updated>{iso(v['published'])}</updated>
    <media:group><media:title>{t}</media:title></media:group>
  </entry>"""
        )
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns:media="http://search.yahoo.com/mrss/" xmlns="http://www.w3.org/2005/Atom">
  <id>yt:channel:{channel_id}</id>
  <yt:channelId>{channel_id}</yt:channelId>
  <title>Fake channel</title>
{chr(10).join(entries)}
</feed>
"""


VTT = "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nFirst line\n\n00:00:02.500 --> 00:00:05.000\nSecond line\n"


class Handler(BaseHTTPRequestHandler):
    media_dir = "."

    def log_message(self, fmt, *args):
        work = os.environ.get("E2E_WORK")
        secrets = run_secrets(work) if work else ()
        text = "fake-youtube " + (fmt % args) + " range=" + str(self.headers.get("Range")) + "\n"
        sys.stderr.write(redact_text(text, secrets))

    def send(self, code, body, ctype, extra=None):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/feeds/videos.xml":
            channel = parse_qs(url.query).get("channel_id", [""])[0]
            videos = [v for v in catalog()["videos"] if v["channel_id"] == channel]
            return self.send(200, feed(channel, videos).encode(), "application/atom+xml; charset=UTF-8")
        m = re.fullmatch(r"/subs/([\w-]{11})\.(\w+)\.vtt", url.path)
        if m:
            return self.send(200, VTT.encode(), "text/vtt")
        m = re.fullmatch(r"/media/([\w.-]+)", url.path)
        if m:
            return self.media(os.path.join(self.media_dir, m.group(1)))
        self.send(404, b"not found", "text/plain")

    def media(self, path):
        if not os.path.isfile(path):
            return self.send(404, b"no such file", "text/plain")
        with open(path, "rb") as f:
            data = f.read()
        total = len(data)
        rng = self.headers.get("Range")
        if not rng:
            return self.send(200, data, "video/mp4", {"Accept-Ranges": "bytes"})
        m = re.fullmatch(r"bytes=(\d*)-(\d*)", rng.strip())
        if not m or (m.group(1) == "" and m.group(2) == ""):
            return self.send(416, b"", "text/plain", {"Content-Range": f"bytes */{total}"})
        if m.group(1) == "":
            start, end = max(0, total - int(m.group(2))), total - 1
        else:
            start = int(m.group(1))
            end = min(int(m.group(2)), total - 1) if m.group(2) else total - 1
        if start >= total or start > end:
            return self.send(416, b"", "text/plain", {"Content-Range": f"bytes */{total}"})
        self.send(206, data[start : end + 1], "video/mp4",
                  {"Accept-Ranges": "bytes", "Content-Range": f"bytes {start}-{end}/{total}"})


def main():
    if len(sys.argv) != 4 or sys.argv[1] != "serve":
        sys.exit("usage: fake_youtube.py serve <port> <media_dir>")
    Handler.media_dir = sys.argv[3]
    server = ThreadingHTTPServer(("127.0.0.1", int(sys.argv[2])), Handler)
    server.serve_forever()


if __name__ == "__main__":
    main()
