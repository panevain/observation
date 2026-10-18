# syntax=docker/dockerfile:1.7

# ---------------------------------------------------------------------------
# Stage 1: builder
#   * Rust 1.97.1 on Debian 12 (bookworm) -- must match rust-toolchain.toml.
#   * Pinned by digest (multi-arch index); the tag is kept for readability.
#     Refresh with: docker buildx imagetools inspect rust:1.97.1-slim-bookworm
#     (Dependabot's docker ecosystem bumps both tag and digest.)
# ---------------------------------------------------------------------------
FROM rust:1.97.1-slim-bookworm@sha256:2775a09d208ff0d7c1f50490c45b62db929e87ba1dcbc3f2132ac71a704bcdd3 AS builder

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG TRUNK_VERSION=0.21.14
# sha256 of the release tarballs, taken from
# https://github.com/trunk-rs/trunk/releases/tag/v0.21.14 (the published
# *.tar.gz.sha256 files) and cross-checked against a locally computed hash.
ARG TRUNK_SHA256_AMD64=f2b4680cd239693a646a2795e4633c625328d7b2a044fbe749fa3a2fe9e7036b
ARG TRUNK_SHA256_ARM64=b1d8e60e454f7fc182d9a4d95d1506ffbae947d8ba90f8f6f02da93b60f980f9

# TRUNK_SKIP_VERSION_CHECK: trunk would otherwise query the crates.io API for a
# newer trunk release on every build (builder stage only; not in the runtime image).
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

COPY . .

# Client (Bevy -> wasm32) via trunk. Output: /build/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/root/.cache/trunk \
    trunk build --release --locked

# Server binary. The target dir is a cache mount, so copy the artifact out.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/target \
    set -eux; \
    cargo build --release --locked -p observation-server; \
    mkdir -p /out/cache; \
    cp target/release/observation-server /out/observation-server

# ---------------------------------------------------------------------------
# Stage 2: runtime -- distroless, glibc, runs as nonroot (uid/gid 65532).
#   Pinned by digest; refresh with:
#   docker buildx imagetools inspect gcr.io/distroless/cc-debian12:nonroot
# ---------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

LABEL org.opencontainers.image.title="observation" \
      org.opencontainers.image.source="https://github.com/panevain/observation" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"

WORKDIR /app
COPY --from=builder --chown=65532:65532 /out/observation-server /app/observation-server
COPY --from=builder --chown=65532:65532 /build/dist /app/dist
# Writable tile/Overpass cache for the nonroot user (no shell in this image,
# so the directory is created in the builder and copied across).
COPY --from=builder --chown=65532:65532 /out/cache /var/cache/observation

ENV OBS_BIND=0.0.0.0:8080 \
    OBS_STATIC_DIR=/app/dist \
    OBS_CACHE_DIR=/var/cache/observation

EXPOSE 8080
# Numeric uid:gid (== distroless "nonroot") so Kubernetes can verify runAsNonRoot.
USER 65532:65532

# No HEALTHCHECK: the image has no shell or curl. Probe GET /healthz from the
# orchestrator (Kubernetes httpGet, compose/ECS health checks, etc.).
ENTRYPOINT ["/app/observation-server"]
