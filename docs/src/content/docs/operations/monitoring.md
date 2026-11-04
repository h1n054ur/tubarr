---
title: Monitoring
description: Prometheus metrics at /metrics, what each one counts, and some useful queries.
---

Tubarr can serve its counters in the Prometheus text format at `GET /metrics`. It is off until you turn it on:

```toml
[metrics]
enabled = true
token_file = "/run/secrets/metrics_token"   # sent as Authorization: Bearer
```

Without `token_file` the API key (`api_key_file`) is the token. With neither, only clients `[auth] play_allowed_networks` lets through may scrape, the same rule as `/play`. A scrape costs no YouTube lookups: it reads counters, counts the rows of three tables and lists the sample table folder. The keys are listed under [`[metrics]`](../../setup/configuration/#metrics). For notices to Discord or a webhook instead, see [`[notify]`](../../setup/configuration/#notify).

```yaml
scrape_configs:
  - job_name: tubarr
    scrape_interval: 60s
    metrics_path: /metrics
    authorization:
      credentials_file: /etc/prometheus/tubarr_metrics_token
    static_configs:
      - targets: ["tubarr:8480"]
```

Counters start from zero on each restart. Labels are fixed and few: no video ids, titles or paths.

| Metric | Type | What it counts |
|---|---|---|
| `tubarr_lookups_total{outcome}` | counter | Stream lookups: `cache_hit` (no yt-dlp run), `ok`, `failed` |
| `tubarr_lookup_duration_seconds` | histogram | Fresh lookups, from the budget token to the answer, every fallback step included |
| `tubarr_lookup_budget_tokens` | gauge | Lookup budget tokens ready now |
| `tubarr_lookup_budget_queued` | gauge | Lookups waiting for a token |
| `tubarr_lookup_budget_paused_seconds` | gauge | Time left on a pause after YouTube pushed back |
| `tubarr_lookup_budget_waits_total` | counter | Lookups that had to wait for a token |
| `tubarr_lookup_budget_wait_seconds_total` | counter | Time spent waiting for tokens |
| `tubarr_ytdlp_processes` | gauge | yt-dlp processes running now |
| `tubarr_play_requests_total{status}` | counter | `/play` answers by class: `2xx`, `3xx`, `4xx`, `5xx` |
| `tubarr_play_starts_total` | counter | `/play` requests that start playback (no `Range`, or one from byte 0) |
| `tubarr_play_first_byte_seconds` | histogram | From a playback start to its first body bytes, a stand-in for time to first frame |
| `tubarr_play_bytes_total` | counter | Media bytes `/play` sent in its 2xx answers |
| `tubarr_play_table_builds_total` | counter | Sample tables read from YouTube's `moof`s |
| `tubarr_play_table_cache_hits_total` | counter | Sample tables found in `[play] index_dir` |
| `tubarr_matcher_run_duration_seconds` | histogram | Matcher runs |
| `tubarr_matcher_failures_total` | counter | Matcher runs that failed |
| `tubarr_matcher_released_total` | counter | Releases the matcher made |
| `tubarr_matcher_matched`, `tubarr_matcher_unchanged` | gauge | Mappings the last run matched, and left alone |
| `tubarr_unmatched_notes`, `tubarr_channel_videos`, `tubarr_grabs`, `tubarr_activity_events` | gauge | Rows in each store (see [Sizing](../sizing/#disk)) |
| `tubarr_play_index_files`, `tubarr_play_index_bytes` | gauge | Sample table files in `[play] index_dir` |
| `tubarr_arr_syncs_total{app,result}` | counter | Syncs with `sonarr` and `radarr`, `ok` or `failed` |
| `tubarr_arr_pushes_total{app,outcome}` | counter | Pushes by outcome: `grabbed`, `pending`, `rejected`, `failed` |
| `process_resident_memory_bytes` | gauge | Tubarr's own resident memory (yt-dlp and deno not included) |

Some useful queries:

- Lookup success: `rate(tubarr_lookups_total{outcome="ok"}[1h]) / rate(tubarr_lookups_total{outcome=~"ok|failed"}[1h])`
- Failed plays: `rate(tubarr_play_requests_total{status="5xx"}[1h])`
- The slowest starts: `histogram_quantile(0.95, rate(tubarr_play_first_byte_seconds_bucket[1h]))`

## A short soak

For a project checkout, `scripts/soak/run.sh [workers] [minutes]` starts the demo stack (it serves `/metrics` with the API key as the token), drives it with concurrent plays and searches and writes memory, play latency and the counters to a CSV under `.demo-soak/` every 15 seconds.
