# syntax=docker/dockerfile:1.7

# ---------------------------------------------------------------------------
# Stage 1: chef -- toolchain, trunk and cargo-chef (rarely changes).
#   * Rust 1.99.1 on Debian 12 (bookworm) -- must match rust-toolchain.toml.
#   * Pinned by digest (multi-arch index); the tag is kept for readability.
#     Refresh with: docker buildx imagetools inspect rust:1.99.1-slim-bookworm
#     (Dependabot's docker ecosystem bumps both tag and digest.)
#
# Layer caching strategy: dependencies are compiled from a cargo-chef recipe
# (only Cargo.toml/Cargo.lock data), so the expensive Bevy/axum dependency
# build is a regular image layer that the GHA layer cache keeps until the
# dependency graph changes. Source-only changes rebuild just our crates.
# ---------------------------------------------------------------------------
FROM rust:1.99.1-slim-bookworm@sha256:86234e857a413004348b471d11a4ebe1aa92d2eae2b6b143cac68fd1ac3c48be AS chef

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG TRUNK_VERSION=0.21.14
# sha256 of the release tarballs, taken from
# https://github.com/trunk-rs/trunk/releases/tag/v0.21.14 (the published
# *.tar.gz.sha256 files) and cross-checked against a locally computed hash.
ARG TRUNK_SHA256_AMD64=f2b4680cd239693a646a2795e4633c625328d7b2a044fbe749fa3a2fe9e7036b
ARG TRUNK_SHA256_ARM64=b1d8e60e454f7fc182d9a4d95d1506ffbae947d8ba90f8f6f02da93b60f980f9
ARG CARGO_CHEF_VERSION=0.1.78

# TRUNK_SKIP_VERSION_CHECK: trunk would otherwise query the crates.io API for a
# newer trunk release on every build (builder stages only; not in the runtime image).
ENV CARGO_TERM_COLOR=always \
    CARGO_INCREMENTAL=0 \
    TRUNK_SKIP_VERSION_CHECK=true

# curl + ca-certificates: fetch trunk (and, at build time, trunk fetches the
# wasm-bindgen / wasm-opt versions matching Cargo.lock).
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl

# trunk: prebuilt release binary, sha256-verified.
RUN set -eux; \
    case "$(dpkg --print-architecture)" in \
      amd64) arch=x86_64;  sha="${TRUNK_SHA256_AMD64}" ;; \
      arm64) arch=aarch64; sha="${TRUNK_SHA256_ARM64}" ;; \
      *) echo "unsupported architecture" >&2; exit 1 ;; \
    esac; \
    f="trunk-${arch}-unknown-linux-gnu.tar.gz"; \
    curl --proto '=https' --tlsv1.2 -fsSL -o "/tmp/${f}" \
      "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/${f}"; \
    echo "${sha}  /tmp/${f}" | sha256sum -c -; \
    tar -xzf "/tmp/${f}" -C /usr/local/bin trunk; \
    rm "/tmp/${f}"; \
    trunk --version

WORKDIR /build

# Install exactly what rust-toolchain.toml declares (channel, components and
# the wasm32-unknown-unknown target).
COPY rust-toolchain.toml ./
RUN rustup toolchain install --profile minimal --no-self-update \
    && rustup target add wasm32-unknown-unknown \
    && rustc --version && cargo --version

# cargo-chef, pinned and built from crates.io with its own lockfile.
RUN cargo install --locked --version "${CARGO_CHEF_VERSION}" cargo-chef \
    && rm -rf /usr/local/cargo/registry \
    && cargo chef --version

# ---------------------------------------------------------------------------
# Stage 2: planner -- reduce the workspace to a dependency recipe.
# The recipe only changes when manifests or Cargo.lock change, so the cook
# layers below stay cached across source-only commits.
# ---------------------------------------------------------------------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---------------------------------------------------------------------------
# Stage 3a: server-build -- the native server binary (no Bevy, quick).
# ---------------------------------------------------------------------------
FROM chef AS server-build

COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --locked -p observation-server --recipe-path recipe.json

COPY . .
RUN set -eux; \
    cargo build --release --locked -p observation-server; \
    mkdir -p /out/cache; \
    cp target/release/observation-server /out/observation-server

# ---------------------------------------------------------------------------
# Stage 3b: client-build -- Bevy -> wasm32 via trunk (the slow part).
# Skipped by the `runtime-prebuilt` target below, which takes a dist/ built
# elsewhere through the `client-dist` named build context (CI's docker and
# Trivy jobs pass a stub dist/).
# ---------------------------------------------------------------------------
FROM chef AS client-build

COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --profile wasm-release --target wasm32-unknown-unknown --locked \
      -p observation-client --recipe-path recipe.json

COPY . .
# Output: /build/dist. Trunk's tool downloads (wasm-bindgen, wasm-opt) are cached.
RUN --mount=type=cache,target=/root/.cache/trunk \
    trunk build --release --locked

# ---------------------------------------------------------------------------
# Region agent image (target `regions`): Overpass API binaries, osmium and the
# observation-regions agent, which imports Geofabrik extracts into local
# Overpass databases. Optional; see compose.yaml.
#   docker build --target regions -t observation-regions .
# ---------------------------------------------------------------------------
FROM chef AS regions-build

COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --locked -p observation-regions --recipe-path recipe.json

COPY . .
RUN cargo build --release --locked -p observation-regions \
    && cp target/release/observation-regions /observation-regions

# Overpass API from the release tarball, sha256-verified. Releases:
# https://dev.overpass-api.de/releases/
FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS overpass-build

SHELL ["/bin/bash", "-o", "pipefail", "-c"]
ARG OVERPASS_VERSION=0.7.62.11
ARG OVERPASS_SHA256=ff9ab837034c25a1956ff638e9af66a7f36f6cbaffc97d71429d1aa8b0a929ba

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates curl g++ make libexpat1-dev zlib1g-dev liblz4-dev

WORKDIR /src
RUN set -eux; \
    f="osm-3s_v${OVERPASS_VERSION}.tar.gz"; \
    curl --proto '=https' --tlsv1.2 -fsSL -o "/tmp/${f}" "https://dev.overpass-api.de/releases/${f}"; \
    echo "${OVERPASS_SHA256}  /tmp/${f}" | sha256sum -c -; \
    tar -xzf "/tmp/${f}" --strip-components=1; \
    ./configure --prefix=/opt/overpass --enable-lz4 CXXFLAGS="-O2"; \
    make -j"$(nproc)"; \
    mkdir -p /out/bin; \
    cp bin/osm3s_query bin/update_database /out/bin/; \
    mkdir -p /out/data

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS regions

LABEL org.opencontainers.image.title="observation-regions" \
      org.opencontainers.image.description="Region agent for Observation: local Overpass databases built from Geofabrik extracts" \
      org.opencontainers.image.source="https://github.com/panevain/observation" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates libexpat1 zlib1g liblz4-1 osmium-tool

COPY --from=overpass-build /out/bin /opt/overpass/bin
COPY --from=regions-build /observation-regions /usr/local/bin/observation-regions
COPY --from=overpass-build --chown=65532:65532 /out/data /data

ENV OBS_REGIONS_BIND=0.0.0.0:8090 \
    OBS_REGIONS_DATA_DIR=/data \
    OBS_REGIONS_OVERPASS_BIN=/opt/overpass/bin

VOLUME /data
EXPOSE 8090
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/observation-regions"]

# ---------------------------------------------------------------------------
# Stage 4: runtime -- distroless, glibc, runs as nonroot (uid/gid 65532).
#   Pinned by digest; refresh with:
#   docker buildx imagetools inspect gcr.io/distroless/cc-debian12:nonroot
# ---------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS runtime-base

LABEL org.opencontainers.image.title="observation" \
      org.opencontainers.image.source="https://github.com/panevain/observation" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"

WORKDIR /app
COPY --from=server-build --chown=65532:65532 /out/observation-server /app/observation-server
# Writable tile/Overpass cache for the nonroot user (no shell in this image,
# so the directory is created in the builder and copied across).
COPY --from=server-build --chown=65532:65532 /out/cache /var/cache/observation

ENV OBS_BIND=0.0.0.0:8080 \
    OBS_STATIC_DIR=/app/dist \
    OBS_CACHE_DIR=/var/cache/observation

EXPOSE 8080
# Numeric uid:gid (== distroless "nonroot") so Kubernetes can verify runAsNonRoot.
USER 65532:65532

# No HEALTHCHECK: the image has no shell or curl. Probe GET /healthz from the
# orchestrator (Kubernetes httpGet, compose/ECS health checks, etc.).
ENTRYPOINT ["/app/observation-server"]

# ---------------------------------------------------------------------------
# Targets. The last stage is the default, so a plain `docker build .` compiles
# the client from source (used by releases, for hermetic provenance).
#   --target runtime-prebuilt --build-context client-dist=<dir>
# skips the wasm build and copies <dir> (a finished trunk dist/) instead.
# ---------------------------------------------------------------------------
FROM runtime-base AS runtime-prebuilt
COPY --from=client-dist --chown=65532:65532 / /app/dist

FROM runtime-base AS runtime
COPY --from=client-build --chown=65532:65532 /build/dist /app/dist
