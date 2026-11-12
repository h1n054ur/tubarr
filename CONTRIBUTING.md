# Contributing

Thanks for your interest in Tubarr. Bug reports, fixes and small improvements are welcome. For a larger change, please open an issue first so we can agree on the approach before you spend time on it.

Security problems go through [SECURITY.md](SECURITY.md), not public issues.

## Getting set up

You need:

- Rust, at the version pinned in `rust-toolchain.toml` (`rustup` picks it up)
- [Bun](https://bun.sh) for the web UI in `web/`, version 1.4.2, the one that wrote `web/bun.lock` (also `BUN_VERSION` in the `Dockerfile`)
- Python 3 for the demo stack and the end-to-end check
- Docker and ffmpeg, only for the end-to-end check

## Build and test

The Rust service:

```sh
cargo build
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

The web UI:

```sh
cd web
bun install --frozen-lockfile
bun run typecheck && bun run test
bun run build      # writes web/dist, which the next cargo build embeds
bun run dev        # live reload, proxies /admin/api, /api/health and /auth to a Tubarr on localhost:8480
```

`cargo build` does not need Bun: without a build in `web/dist` the binary answers with a short "UI not built" page. Tubarr serves the UI at `/` (any GET no other route answers gets `index.html`, so the UI's own routes work on reload, the login page included). The UI talks to `/admin/api`.

To run the service itself:

```sh
cargo run -- --config ./tubarr.toml   # or set TUBARR_CONFIG
curl http://localhost:8480/healthz    # ok
umask 077                             # the header file below is readable only by you
printf 'X-Api-Key: %s\n' "$(cat /path/to/tubarr_api_key)" > ~/.tubarr-header   # the file api_key_file names
curl -H @"$HOME/.tubarr-header" http://localhost:8480/api/health   # JSON: binaries, limiter, canary, fallback steps, watcher, resolver
```

The key goes in a header file so it stays out of the process list, where anyone on the host could read it. For a local run, point `data_dir` somewhere writable.

Please run these before you open a pull request: `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings` and `cargo test --all --locked` for the service, and `bun run typecheck`, `bun run test` and `bun run build` in `web/`. Public pull requests run read-only Rust checks, Web checks and Harness checks on hosted runners, without repository secrets. Rust checks include the licence and advisory checks below. Web checks use the frozen lockfile and check package licences.

Harness checks run offline fake-service lifecycle and evidence regressions, plus workflow checks:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s e2e -p test_state.py -v
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s e2e -p test_artifacts.py -v
bun test scripts/check-workflows.test.ts
```

These do not run Docker or prove real end-to-end behavior. Public image publication requires the same commit's core checks and hosted Real E2E job to pass first, including on release dry runs. Real E2E builds the source binary and uses Docker with real Sonarr and Radarr metadata lookups. Unavailable metadata keeps that gate failing.

## See your change on demo data

```sh
scripts/demo.sh
```

It needs cargo, bun and python3, and Ctrl-C stops everything. It builds a debug Tubarr, starts it on `localhost:8480` with a fresh config and database under `.demo/` (git-ignored), seeds it and starts `vite`. Open the printed URL (`http://127.0.0.1:5173/`, `WEB_PORT` changes it, `TUBARR_PORT` the 8480) and sign in as `demo` / `demo-password`, a local test user the script creates. YouTube is the e2e fake YouTube and fake yt-dlp (`YT_PORT`, default 8482), Radarr is a small fake in `demo_seed.py` (`ARR_PORT`, default 8483), and no Sonarr or Jellyfin is configured, so nothing leaves the machine.

`scripts/demo_seed.py` makes the data, the same on every run: 30 mappings (Urdu, Hindi, English and Punjabi shows, two daily shows, films, four inactive, long titles) with releases and aliases, a few hundred unmatched notes in every state (some ignored), 160 grabs (some failed), unplayable videos, backfill state with errors, nine channels on the Health page, real Newznab searches, `/play` starts and pushes to Radarr on the Activity page, and three show and three film drafts on the Drafts page. Mappings, aliases, drafts, the user, searches, plays and pushes go through Tubarr's API. The rest (notes, releases, grabs, inactive flags, backfill state) is plain SQL, since no API writes it. The fake Radarr tags the films, so they get Radarr ids and the catch-up push sends them (each answer a different decision), but lists them unmonitored. With no Sonarr either, the matcher has no wanted titles, so it leaves the seeded data alone. For a change that touches what people see, please look at the affected screens there before you open the pull request.

## End-to-end check

`e2e/run.sh` runs the whole path against real Sonarr and Radarr, offline from YouTube. It starts three throwaway containers on one Docker network: Sonarr, Radarr, and the release Tubarr binary next to a fake YouTube (`e2e/fake_youtube.py` serves channel feeds, the fMP4 test fixtures with Range and captions, `e2e/fake-yt-dlp` answers listings and lookups). `e2e/drive.py` then sets up both arrs through their APIs (the `youtube` tag, Tubarr as Newznab indexer and SABnzbd client, extra files with `srt`, a Radarr profile with language Any), adds one series and one film, gives Tubarr its channels through the admin API, seeds the fake feeds (episodes, a teaser, a Part 1 and Part 2 pair, the film), runs the arr searches and checks the imports and the `/play` answers (HEAD, Range windows over the whole file, ffprobe over HTTP, a clean decode).

```sh
cargo build --release
e2e/run.sh /path/to/evidence     # PASS or FAIL per step, exit 0 only when all pass
KEEP=1 e2e/run.sh                # leave it running for a look, then: e2e/run.sh down
```

It needs Docker, Python 3, ffmpeg and internet access for the arrs' TVDB and TMDB lookups. The evidence folder (default `target/e2e-evidence`) contains selected textual API responses, results, bounded service logs and `lifecycle.json` with the original exit status. Generated API keys and their encoded forms are redacted before evidence is written or details reach the console. If credential sources cannot be read safely, terminal service inspection and log collection stop. The original failure status and cleanup still apply.

The hosted release job uploads only the selected sanitized evidence folder on failure or cancellation, retained for three days. It does not upload on success or include raw work directories, configs, databases, key files or binary playback captures. Collection runs before teardown, but cannot be guaranteed after SIGKILL.

State and work live under `$XDG_STATE_HOME/tubarr/e2e`, or `$HOME/.local/state/tubarr/e2e` when XDG state home is unset or relative. Application directories must be owned by you with mode 0700, and lifecycle files use mode 0600. Overlapping runs and `down` during an active run fail promptly with exit 75. A retained run also blocks a new start. `KEEP=1` retains a run only once the driver has started, including a driver failure, not a setup failure or interruption. After KEEP or an abrupt exit, use `e2e/run.sh down` to recover. Cleanup checks every resource's ownership before removing any, and keeps state if cleanup fails so you can retry. It never reads the old temporary state file. The offline harness checks above do not replace the real end-to-end check, which is worth running for changes to the indexer, download client, grab or `/play` code.

`scripts/soak/run.sh [workers] [minutes]` is a short soak on the demo stack, described under [Monitoring](https://h1n054ur.github.io/tubarr/operations/monitoring/).

## Layout

| Path | What |
|---|---|
| `crates/tubarr` | the service: `lib.rs` holds the router, `config.rs` the TOML config, `main.rs` the binary |
| `crates/tubarr/src/watcher` | finding uploads: channel feeds, lists, playlists and backfill searches |
| `crates/tubarr/src/matcher` | numbered, daily, playlist-ordered and film matching, parts and release sets |
| `crates/tubarr/src/newznab`, `src/sabnzbd` | the indexer and download client APIs Sonarr and Radarr talk to |
| `crates/tubarr/src/arr` | the tagged title sync, root folder moves and the release push |
| `crates/tubarr/src/play` | the `/play/{id}` relay: the virtual muxed MP4 with Range |
| `crates/tubarr/src/resolver`, `src/binaries` | yt-dlp lookups with the fallback steps, and the managed yt-dlp and deno |
| `crates/tubarr/src/subtitles` | the captions written next to each `.strm` (WebVTT to SRT) |
| `crates/tubarr/src/store` | the SQLite repositories |
| `crates/tubarr/src/admin` | `/admin/api`, the JSON API behind the web UI, and the embedded UI itself |
| `crates/tubarr/src/auth` | sign-in: users, sessions, password and OpenID Connect login, the `[auth]` config |
| `crates/tubarr/migrations` | SQL migrations, embedded in the binary |
| `crates/tubarr/tests` | integration tests against the router and the store |
| `.github/workflows` | public core checks, gated releases and docs publication |
| `deny.toml`, `web/scripts/licenses.ts` | the licences allowed for Rust crates and npm packages, checked in CI |
| `scripts/soak` | a short soak on the demo stack: concurrent plays and searches, `/metrics` sampled into a CSV |
| `scripts/notices` | the header and cargo-about setup for the image's third-party notices |
| `config/tubarr.example.toml` | every config key with its default |
| `e2e/` | the end-to-end check against real Sonarr and Radarr with a fake YouTube |
| `web/` | the web UI: Bun, Vite, React, TanStack Router and Query, Tailwind CSS with the Tubarr palette |

## Dependencies and licences

Tubarr is AGPL-3.0-only, and every dependency has to be under a licence that can be combined with it.

yt-dlp and deno run as separate programs and are not linked. The image carries `/usr/share/doc/tubarr/THIRD_PARTY_NOTICES` with the licences of the Rust crates and npm packages built in, and of the bgutil PO token plugin (GPL-3.0) it bundles in `/opt/yt-dlp-plugins`.

- Rust crates: `deny.toml` lists the allowed licences, and `cargo deny check` also fails on any known RustSec advisory. Run it with [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) if you add or update a crate.
- npm packages: `bun scripts/licenses.ts check` in `web/` does the same for the UI.

If a new dependency needs a licence that is not on the list, say so in the pull request rather than adding it quietly, so it can be read and agreed on.

## Commits and pull requests

- Commit messages follow `type(scope): plain sentence`, for example `fix(play): answer 416 for a range past the end`. Common types are `feat`, `fix`, `docs`, `test`, `refactor` and `chore`.
- Keep a pull request to one change, with tests for new behaviour and the README or docs updated when a config key, API or command changes.
- Match the style of the code around your change. `cargo fmt` and the lints decide formatting.
- There is no contributor licence agreement. By opening a pull request you agree that your contribution is licensed under the AGPL-3.0-only, the same as the rest of the project.
