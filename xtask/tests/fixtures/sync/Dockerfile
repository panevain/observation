# syntax=docker/dockerfile:1.7
FROM ghcr.io/jdx/mise:2026.10.25-debian@sha256:0000000000000000000000000000000000000000000000000000000000000000 AS build-tools
FROM rust:1.99.1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7 AS chef
# Not root, as the real Dockerfile's final stages (65532 is distroless "nonroot").
USER 65532:65532
