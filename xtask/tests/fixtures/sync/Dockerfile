# syntax=docker/dockerfile:1.7
FROM ghcr.io/jdx/mise:2026.10.25-debian@sha256:0000000000000000000000000000000000000000000000000000000000000000 AS build-tools
FROM rust:1.99.1-slim-trixie AS chef
# Not root, as the real Dockerfile's final stages (65532 is distroless "nonroot").
USER 65532:65532
