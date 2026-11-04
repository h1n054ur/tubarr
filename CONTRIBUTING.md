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
```

Please run these before you open a pull request: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test --all` for the service, and `bun run typecheck` and `bun run test` in `web/` for the UI. The same checks, plus the licence and advisory checks below, run in the project's CI before a change is merged.

## See your change on demo data

```sh
scripts/demo.sh
```

This builds a debug Tubarr, seeds a fresh database under `.demo/` and starts the web dev server with a fake YouTube and a fake Radarr, so nothing leaves your machine. Sign in as `demo` / `demo-password`. The README section "See the UI on demo data" has the details. For a change that touches what people see, please look at the affected screens there before you open the pull request.

The end-to-end check (`e2e/run.sh`, see the README) runs the whole path against real Sonarr and Radarr containers. It is worth running for changes to the indexer, download client, grab or `/play` code.

## Dependencies and licences

Tubarr is AGPL-3.0-only, and every dependency has to be under a licence that can be combined with it.

- Rust crates: `deny.toml` lists the allowed licences, and `cargo deny check` also fails on any known RustSec advisory. Run it with [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) if you add or update a crate.
- npm packages: `bun scripts/licenses.ts check` in `web/` does the same for the UI.

If a new dependency needs a licence that is not on the list, say so in the pull request rather than adding it quietly, so it can be read and agreed on.

## Commits and pull requests

- Commit messages follow `type(scope): plain sentence`, for example `fix(play): answer 416 for a range past the end`. Common types are `feat`, `fix`, `docs`, `test`, `refactor` and `chore`.
- Keep a pull request to one change, with tests for new behaviour and the README updated when a config key, API or command changes.
- Match the style of the code around your change. `cargo fmt` and the lints decide formatting.
- There is no contributor licence agreement. By opening a pull request you agree that your contribution is licensed under the AGPL-3.0-only, the same as the rest of the project.
