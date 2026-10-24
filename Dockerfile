# syntax=docker/dockerfile:1.7

# ---------------------------------------------------------------------------
# Stage 1: chef -- toolchain, trunk and cargo-chef (rarely changes).
#   * Rust 1.99.1 on Debian 13 (trixie) -- must match rust-toolchain.toml.
#   * Pinned by digest (multi-arch index); the tag is kept for readability.
#     Refresh with: docker buildx imagetools inspect rust:1.99.1-slim-trixie
#     (Dependabot refreshes the digest; the tag moves with rust-toolchain.toml.)
#
# Layer caching strategy: dependencies are compiled from a cargo-chef recipe
# (only Cargo.toml/Cargo.lock data), so the expensive Bevy/axum dependency
# build is a regular image layer that the GHA layer cache keeps until the
# dependency graph changes. Source-only changes rebuild just our crates.
# ---------------------------------------------------------------------------
FROM rust:1.99.1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS chef

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG TRUNK_VERSION=0.22.0
# sha256 of the release tarballs: GitHub's digests of the assets of
# https://github.com/trunk-rs/trunk/releases/tag/v0.22.0, which match the published
# *.tar.gz.sha256 files. scripts/bump-pins.py updates these with the version.
ARG TRUNK_SHA256_AMD64=f78c1989240c56592d8cfc4df5cc27399a3ff66a3f9cc819e16f1e3d809ce7c2
ARG TRUNK_SHA256_ARM64=a05b519ad8ba8b3935772586c2f08f93dd8f9ce06c74c03e89d3c2d03cb628b1
ARG CARGO_CHEF_VERSION=0.1.78
ARG CARGO_AUDITABLE_VERSION=0.7.7

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

# cargo-chef and cargo-auditable, pinned and built from crates.io with their own lockfiles.
# cargo-auditable embeds the dependency list in the native binaries (a few KB, kept by
# `strip`), so Trivy, the image SBOM and `cargo audit bin` can see which crates they contain.
RUN cargo install --locked --version "${CARGO_CHEF_VERSION}" cargo-chef \
    && cargo install --locked --version "${CARGO_AUDITABLE_VERSION}" cargo-auditable \
    && rm -rf /usr/local/cargo/registry \
    && cargo chef --version && cargo auditable --version

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

# `cargo auditable build` rebuilds only the workspace crates; the cooked dependencies stay fresh.
COPY . .
RUN set -eux; \
    cargo auditable build --release --locked -p observation-server; \
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
RUN cargo auditable build --release --locked -p observation-regions \
    && cp target/release/observation-regions /observation-regions

# Overpass API from the release tarball, sha256-verified. Releases:
# https://dev.overpass-api.de/releases/
FROM debian:trixie-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad AS overpass-build

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

FROM debian:trixie-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad AS regions

LABEL org.opencontainers.image.title="observation-regions" \
      org.opencontainers.image.description="Region agent for Observation: local Overpass databases built from Geofabrik extracts" \
      org.opencontainers.image.source="https://github.com/panevain/observation" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"

# `apt-get upgrade` takes the Debian security fixes published since the base image was
# built. Release builds skip the layer cache for this stage (no-cache-filters in
# release.yml), so every published region agent image has them.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
    && apt-get update \
    && apt-get upgrade -y \
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
# Stage 4: runtime -- distroless (Debian 13), glibc, runs as nonroot (uid/gid 65532).
#   Pinned by digest; refresh with:
#   docker buildx imagetools inspect gcr.io/distroless/cc-debian13:nonroot
#   Distroless stops rebuilding a Debian release's images once the next one is out
#   (cc-debian12 was frozen in August 2026), so move to the new suffix when that happens.
# ---------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e AS runtime-base

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
