# Tubarr image: the tubarr binary with the web UI built in, on Debian slim, as uid 1000.
#
# yt-dlp and deno are not baked in: Tubarr downloads them from their GitHub releases into
# /config/bin on first start and keeps them current there, so a YouTube change needs no new
# image. ffmpeg is not needed: /play joins the tracks itself.
#
# Every base image is multi-arch and cargo-about is fetched for TARGETARCH, so the same file builds
# linux/amd64 and linux/arm64. Tubarr picks the matching yt-dlp and deno builds at runtime.
#
# Builds with BuildKit (docker build). The cache mounts speed up local rebuilds. Builders that ignore
# them rely on the layer cache instead, which is why the dependencies are built in their own layer
# first.

# Keep RUST_VERSION equal to rust-toolchain.toml and BUN_VERSION to the one that wrote web/bun.lock.
ARG RUST_VERSION=1.99.0
ARG BUN_VERSION=1.4.2
ARG DEBIAN_RELEASE=trixie

# ---- web UI ---------------------------------------------------------------------------------
FROM oven/bun:${BUN_VERSION}-slim AS web
WORKDIR /src/web
COPY web/package.json web/bun.lock ./
RUN bun install --frozen-lockfile
COPY web/ ./
RUN bun run build \
    && bun scripts/licenses.ts notices /src/web/notices-web.txt

# ---- optional yt-dlp plugin: the bgutil PO token provider -----------------------------------
# Only used when [fallback] po_token_url is set, with plugin_dir pointing at /opt/yt-dlp-plugins. Keep
# the version in step with the bgutil server it talks to. It is GPL-3.0: its licence goes next to it and
# into the third-party notices. Update BGUTIL_LICENSE_SHA256 with the version.
FROM debian:${DEBIAN_RELEASE}-slim AS plugins
ARG BGUTIL_VERSION=2.0.2
ARG BGUTIL_SHA256=a26b980bf6d8e8400d8f21445daa4d8381bf6183d20f0be309391f86b31e27ef
ARG BGUTIL_LICENSE_SHA256=3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /opt/yt-dlp-plugins \
    && curl -fsSL -o /opt/yt-dlp-plugins/bgutil-ytdlp-pot-provider.zip \
        "https://github.com/Brainicism/bgutil-ytdlp-pot-provider/releases/download/${BGUTIL_VERSION}/bgutil-ytdlp-pot-provider.zip" \
    && echo "${BGUTIL_SHA256}  /opt/yt-dlp-plugins/bgutil-ytdlp-pot-provider.zip" | sha256sum -c - \
    && curl -fsSL -o /opt/yt-dlp-plugins/LICENSE \
        "https://raw.githubusercontent.com/Brainicism/bgutil-ytdlp-pot-provider/${BGUTIL_VERSION}/LICENSE" \
    && echo "${BGUTIL_LICENSE_SHA256}  /opt/yt-dlp-plugins/LICENSE" | sha256sum -c -

# ---- tubarr binary --------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS build
# aws-lc-sys (rustls' crypto) needs cmake and a C compiler. The slim image has gcc but not cmake.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake make gcc g++ libc6-dev curl \
    && rm -rf /var/lib/apt/lists/*
# cargo-about writes the crates' licence texts into the third-party notices.
ARG CARGO_ABOUT_VERSION=0.9.3
ARG CARGO_ABOUT_SHA256_AMD64=3ad1e94ea1e63d69d1961f4fb15ccbbe9575d906111f96d2cb4dce6df48f71b4
ARG CARGO_ABOUT_SHA256_ARM64=eb4d9ff6ebd2518152a8308729431a04b3836e71d2bd5ead7aea69f91a71162e
ARG TARGETARCH
RUN case "${TARGETARCH:-amd64}" in \
        amd64) arch=x86_64 sum="${CARGO_ABOUT_SHA256_AMD64}" ;; \
        arm64) arch=aarch64 sum="${CARGO_ABOUT_SHA256_ARM64}" ;; \
        *) echo "no cargo-about build for ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && name="cargo-about-${CARGO_ABOUT_VERSION}-${arch}-unknown-linux-musl" \
    && curl -fsSL -o /tmp/cargo-about.tar.gz \
        "https://github.com/EmbarkStudios/cargo-about/releases/download/${CARGO_ABOUT_VERSION}/${name}.tar.gz" \
    && echo "${sum}  /tmp/cargo-about.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/cargo-about.tar.gz -C /usr/local/bin --strip-components=1 "${name}/cargo-about" \
    && rm /tmp/cargo-about.tar.gz
WORKDIR /src
ENV CARGO_INCREMENTAL=0 CARGO_TERM_COLOR=always

# Dependencies first, against a stub crate, so this layer stays cached until Cargo.lock changes.
# rust-toolchain.toml stays out: the image already is that toolchain, and the file would make
# rustup fetch rustfmt and clippy.
COPY Cargo.toml Cargo.lock ./
COPY crates/tubarr/Cargo.toml crates/tubarr/Cargo.toml
RUN mkdir -p crates/tubarr/src \
    && echo 'fn main() {}' > crates/tubarr/src/main.rs \
    && touch crates/tubarr/src/lib.rs
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked

COPY crates/ crates/
COPY --from=web /src/web/dist web/dist
# The commit Health shows next to the version. Declared after the dependency layer so a new sha
# does not rebuild the dependencies.
ARG TUBARR_GIT_SHA=
ENV TUBARR_GIT_SHA=${TUBARR_GIT_SHA}
# Drop the stub's own artifacts so cargo rebuilds tubarr whatever the copied files' mtimes are.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    rm -rf target/release/.fingerprint/tubarr-* target/release/build/tubarr-* \
        target/release/deps/tubarr-* target/release/deps/libtubarr-* \
    && cargo build --release --locked \
    && install -m 0755 target/release/tubarr /usr/local/bin/tubarr

# Third-party notices: what the image bundles, then the Rust crates and the web UI's npm packages with
# their licence texts, then the bgutil plugin's GPL-3.0. cargo-about lists only the crates built for
# this image's target (rustc's host, which is what the build above compiled for). Its `cargo metadata`
# still resolves every platform in Cargo.lock, so `cargo fetch` first downloads the crates the build
# skipped (Windows, Android...). That runs in this step, so it works whether or not the builder keeps
# the registry cache mount (not every builder shares it the way BuildKit does).
COPY LICENSE scripts/notices/ ./notices/
COPY --from=web /src/web/notices-web.txt notices/
COPY --from=plugins /opt/yt-dlp-plugins/LICENSE notices/bgutil-LICENSE
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo fetch --locked \
    && cargo about generate --frozen --fail --target "$(rustc -vV | sed -n 's/^host: //p')" \
        -c notices/about.toml -o notices/notices-rust.txt notices/about.hbs \
    && { cat notices/header.txt notices/notices-rust.txt; echo; cat notices/notices-web.txt; \
        printf '\nbgutil-ytdlp-pot-provider: GNU General Public License v3.0\n%s\n' "$(printf '%.0s-' $(seq 72))"; \
        cat notices/bgutil-LICENSE; } > THIRD_PARTY_NOTICES

# ---- runtime --------------------------------------------------------------------------------
FROM debian:${DEBIAN_RELEASE}-slim
# ca-certificates for YouTube, GitHub, Sonarr and Radarr over TLS (yt-dlp and deno use the system
# store, and Tubarr's own client does too). tini reaps the yt-dlp and deno children Tubarr spawns.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 tubarr \
    && useradd --uid 1000 --gid 1000 --home-dir /config --no-create-home --shell /usr/sbin/nologin tubarr \
    && mkdir -p /config /downloads \
    && chown 1000:1000 /config /downloads

COPY --from=plugins /opt/yt-dlp-plugins /opt/yt-dlp-plugins
COPY --from=build /usr/local/bin/tubarr /usr/local/bin/tubarr
COPY --from=build /src/THIRD_PARTY_NOTICES /src/notices/LICENSE /usr/share/doc/tubarr/

USER 1000:1000
ENV TUBARR_CONFIG=/config/tubarr.toml
# /config: tubarr.toml, tubarr.db, the managed yt-dlp and deno in /config/bin.
# /downloads: mount the folder Sonarr and Radarr see here. Grabs write .strm and .srt files into
# complete_dir (default /downloads/tubarr), which must be the same path in their containers.
VOLUME ["/config"]
EXPOSE 8480
HEALTHCHECK --interval=30s --timeout=10s --start-period=30s --retries=3 \
    CMD ["/usr/local/bin/tubarr", "--healthcheck"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/tubarr"]
CMD ["--config", "/config/tubarr.toml"]
