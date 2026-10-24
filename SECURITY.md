# Security Policy

## Supported versions

Only the `main` branch (and the container images published from its releases,
`ghcr.io/panevain/observation:latest` and `ghcr.io/panevain/observation-regions:latest`)
receives security fixes. Tagged releases (`v*`) are snapshots of `main`; please
upgrade to the latest one. When a dependency of the latest release gets a security
fix (a crate advisory with a compatible fix, or a Debian fix in an image), a patch
release with it is prepared automatically and published after a maintainer's
approval. The `:devel` images track `develop` and are for testing only.

| Version | Supported |
| ------- | --------- |
| `main` / latest release | yes |
| older tags | no |

## Reporting a vulnerability

Please **do not** open a public issue for security problems.

Report privately through GitHub:

1. Go to the repository's **Security** tab.
2. Choose **Report a vulnerability** (GitHub private vulnerability reporting /
   Security Advisories): <https://github.com/panevain/observation/security/advisories/new>

Include what you found, how to reproduce it, the affected commit or image tag,
and the impact you expect. Proof-of-concept code is welcome; please do not run
it against infrastructure you do not own.

### What to expect

| Step | Target |
| ---- | ------ |
| Acknowledgement of your report | within 3 working days |
| Initial assessment (valid / not valid, severity) | within 7 days |
| Fix or mitigation for critical and high severity issues | within 30 days |
| Fix for medium and low severity issues | next regular release |

This is a small project maintained on a best-effort basis; these are targets,
not guarantees. We will keep you informed, credit you in the advisory unless
you prefer otherwise, and coordinate disclosure with you. Fixes are published
as a GitHub Security Advisory (with a CVE where appropriate).

## Scope

In scope:

- The `observation-server` HTTP service, including its request handling,
  caching and static file serving.
- The optional `observation-regions` agent and its image, including region
  imports, the admin token on region management and the agent token.
- The WebAssembly client served by that server.
- The container image and the build and release pipeline in this repository.

Notes on the threat model:

- The server **proxies third-party OpenStreetMap services** (an Overpass API
  and a tile server, configured with `OBS_OVERPASS_URL` and `OBS_TILE_URL`).
  Server-side request forgery, cache poisoning, request amplification and
  abuse of the proxy to hit upstream services are in scope. Vulnerabilities in
  the upstream services themselves are not; report those to their operators.
- There are **no user accounts, sessions or stored personal data**. Reports
  about authentication or account takeover therefore do not apply. The
  server's cache directory (`OBS_CACHE_DIR`) holds only public map data.
- Operators should set `OBS_CONTACT` so upstream services can reach them, and
  should run the container as its default non-root user behind a TLS-terminating
  reverse proxy.

Out of scope: findings that need physical access, social engineering,
denial of service by raw traffic volume, or issues only present in unsupported
versions or modified deployments.

## Automated security tooling

Every change is checked automatically (see `.github/workflows/`):

- **cargo-deny** (`deny.toml`): RustSec advisories, yanked crates, licence
  allow-list, banned or wildcard dependencies, and allowed crate sources.
- **cargo-audit** (pinned, checksum-verified binary): `Cargo.lock` and the fuzz
  workspace's `fuzz/Cargo.lock`, on every change and daily; yanked crates fail too.
  Accepted advisories need a reason in `deny.toml` and are mirrored in
  `.cargo/audit.toml` and `osv-scanner.toml`.
- **Trivy** (pinned, checksum-verified binary): scans the container image's
  operating system packages and the server binary's crates (the binaries are built
  with `cargo auditable`, which embeds their dependency list; the static client,
  which Trivy does not inspect, is a stub for this scan) and the repository
  (dependencies, Dockerfile misconfiguration, secrets); fails on fixable CRITICAL
  or HIGH findings and uploads SARIF to code scanning.
- **Release watch** (daily): the same checks against the latest release (its
  `Cargo.lock` and both published images). A fix that a patch release can carry
  starts the Hotfix workflow; everything found is tracked in a `Security: vX.Y.Z`
  issue.
- **zizmor** audits the GitHub Actions workflows and the Dependabot configuration
  (hash-pinned install, with its online audits); findings fail the check and are
  uploaded to code scanning.
- **CodeQL** for Rust and GitHub Actions workflows.
- **OpenSSF Scorecard**, published weekly and on branch-protection changes.
- **Dependabot** for Cargo, GitHub Actions, Docker and PyPI updates (version and
  security updates), and a weekly workflow for the pinned tools Dependabot does
  not cover. All GitHub Actions are pinned to full commit SHAs.
- **Release provenance**: both images pushed to GHCR (`observation` and
  `observation-regions`) carry SLSA build provenance and an SBOM, are signed
  keylessly with cosign, and are attested with GitHub artifact attestations. A
  release is published only from a tag on `main` that passes the advisory check,
  a smoke test and the Trivy gate; its GitHub release is immutable. Verify with
  (likewise for `observation-regions`):

  ```sh
  gh attestation verify oci://ghcr.io/panevain/observation:<tag> --repo panevain/observation
  cosign verify ghcr.io/panevain/observation:<tag> \
    --certificate-identity-regexp '^https://github.com/panevain/observation/\.github/workflows/release\.yml@' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com
  ```
