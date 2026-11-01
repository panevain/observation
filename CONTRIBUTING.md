# Contributing

## Branching model (git-flow)

| Branch | Purpose | Created from | Merges into | How |
| --- | --- | --- | --- | --- |
| `main` | Released code only. Every commit is a release or hotfix merge. | – | – | Protected; PRs from `release/*` and `hotfix/*` only |
| `develop` | Integration branch and the GitHub default branch | `main` (once) | – | Protected; all other work lands here by PR |
| `feature/<topic>`, `bugfix/<topic>` | Day-to-day work | `develop` | `develop` | PR, squash or merge commit |
| `release/<x.y.z>` | Stabilising a release (version bump, docs, fixes only) | `develop` | `main` | PR, merge commit; the back-merge into `develop` is opened for you (`backmerge/v<x.y.z>`) |
| `hotfix/<x.y.z>` | Urgent fix to a release; security patch releases are opened by the Hotfix workflow | `main` | `main` | PR, merge commit; back-merge as for releases |

Add user-visible changes under `[Unreleased]` in [CHANGELOG.md](CHANGELOG.md).

Dependabot and the Pins workflow target `develop` (see `.github/dependabot.yml` and `.github/workflows/pins.yml`). Every crate version a pull request adds to `Cargo.lock` or `fuzz/Cargo.lock`, including the ones a bump pulls in, must have been on crates.io for 7 days (the **`cargo-cooldown`** check, `.config/cargo-cooldown.toml`; `mise -E ci run cooldown` runs it against `origin/develop`). Fixes for RustSec vulnerabilities are exempt; anything else that cannot wait needs an exemption there with a reason. New versions of crates the server side uses must also pass **`cargo-vet`**: audited by us or by an imported audit set (Mozilla, Google and others), or exempted; [supply-chain/README.md](supply-chain/README.md) says how to review or exempt one. A Dependabot PR that fails it waits for that audit; once someone pushes it, the auto-merge already turned on completes when the checks pass. Dependabot patch updates and base image digest refreshes merge themselves once their checks pass (**Dependabot auto-merge**, every 4 hours); minor and major updates need a review. To hold a Dependabot PR, add the **`hold`** label: it is not auto-merged, and auto-merge is switched off at the next run (within 4 hours) if it was already on. Remove the label to let it merge. Each run's summary lists every open Dependabot PR and why it was or was not set to auto-merge; run the workflow by hand with **dry run** to see that list without changing anything.

### Releasing

Releases are automatic: the **Prepare release** workflow runs every Tuesday and releases whatever `develop` has.

1. **Prepare release** picks the version from the `[Unreleased]` section of `CHANGELOG.md` (`scripts/release-plan.py`): **minor** if it has Added, Changed, Deprecated or Removed entries (Removed is **major** from 1.0.0), **patch** if it has only Fixed or Security entries, or none but dependency or pin updates changed shipped files since the last release (the changelog then lists those commits). If nothing shipped changed (only `docs/`, `.github/` or Markdown), there is no release that week. It skips the week while a `release/*` or `hotfix/*` branch exists, `main` waits for its tag, or the last release has not been back-merged into `develop` yet. It then creates `release/0.2.0` from `develop`, bumps `version` in the root `Cargo.toml`, updates `Cargo.lock`, dates the `CHANGELOG.md` entry, regenerates the bundle-size treemap (`docs/images/wasm-bundle-treemap.svg`, a 15-20 minute build), and opens the PR into `main` with auto-merge on.
   - **By hand** (Actions → Prepare release → Run workflow): leave the inputs empty for the same result now, give a **version** (`0.2.0`, or `1.0.0` for a breaking release) to choose it, or pick **prerelease: dev** for a test build `X.Y.Z-dev.N` (N counts up from 1; made even when nothing changed). A pre-release does not go through `main`: the bot commits the version bump on top of `develop` and pushes only the `vX.Y.Z-dev.N` tag (no branch, PR or back-merge), and Release publishes it. So `main` always holds the latest stable release, and a hotfix can start at any time. A pre-release leaves `[Unreleased]` in place, and its GitHub release uses those entries as notes; it ships `develop`'s treemap.
   - **Without the workflow**: `git switch -c release/0.2.0 origin/develop`, make the same edits (`scripts/set-version.sh 0.2.0`, `scripts/release-plan.py changelog --version 0.2.0`), and open the PR.
2. The PR `release/0.2.0` → `main` merges itself (merge commit) when its checks pass. To hold a release, disable auto-merge on it; stabilising fixes go on the branch.
3. The rest is automatic. **Release tag** tags the merge commit `v0.2.0`; **Release** publishes `ghcr.io/panevain/observation` and `ghcr.io/panevain/observation-regions` as `0.2.0` and `latest` (pre-releases get only their version tag), each with a cosign signature, provenance and an SBOM, and a GitHub release with the changelog section (marked pre-release for `X.Y.Z-dev.N`); and Release tag opens `backmerge/v0.2.0` → `develop`, which merges itself once its checks pass and brings the version, changelog and treemap back. If the back-merge conflicts (usually `CHANGELOG.md` or `Cargo.lock`), merge `develop` into `backmerge/v0.2.0`, resolve, and push.

Hotfixes work the same way, starting from `main` with `hotfix/<x.y.z>`; Release tag waits for an approval before tagging them (below).

### Security patch releases

**Release watch** checks the latest release every day: cargo-audit on its `Cargo.lock` (then `cargo update` to see which advisories have a semver-compatible fix) and Trivy on its two images. If a patch release would fix something (a crate advisory, or a fixable critical or high OS vulnerability that a rebuild on the current base images clears), it starts **Hotfix**, which opens `hotfix/<x.y.z+1>` → `main` with those updates, re-pinned base images, the version bump and a `### Security` changelog entry. The PR merges itself when its checks pass. Release tag then waits for **one approval**: open its run (Actions → Release tag), choose **Review deployments**, tick `release` and approve; the tag, images, GitHub release and back-merge follow as above.

Everything Release watch finds, fixable or not, is kept in an issue titled `Security: v<x.y.z>`, which it closes when there is nothing left. Advisories with no compatible fix (a semver-incompatible bump, or no fix yet) need a person: a code change on a hotfix branch, or an ignore with a reason in `deny.toml`, `.cargo/audit.toml` and `osv-scanner.toml` if the code is unreachable. Run Hotfix by hand with `force` to publish a patch release on fresh base images when nothing is flagged.

### Bot app (one-time setup)

Pull requests, pushes and tags made with a workflow's `GITHUB_TOKEN` do not start other workflows (pull request checks wait for a manual approval), so the release workflows act as a GitHub App:

1. **Settings → Developer settings → GitHub Apps → New GitHub App**, on the account that owns the repository. Any name; homepage: the repository URL; **Webhook: Active** off. Repository permissions: **Contents**, **Pull requests** and **Workflows**: read and write; nothing else. Installable only on this account.
2. **Generate a private key**, then **Install App** on this repository only.
3. Store the key as the secret `BOT_PRIVATE_KEY` of the `bot` environment and the App's client ID as the repository variable `BOT_CLIENT_ID`, then delete the key file:
   `gh secret set BOT_PRIVATE_KEY --env bot < key.pem` and `gh variable set BOT_CLIENT_ID --body <client ID>`.
4. Put the App's numeric ID in `.github/rulesets/version-tags-create.json` (it may create `v*` tags) and apply the rulesets (below).

The environments: `bot` holds the key and may only be used from `develop`, `main` and `v*` tags; `release` (branch `main`, a required reviewer) is the approval Release tag waits for on hotfixes. Workflows mint a token with only the permissions they need, after any build step or in a job that builds nothing.

### Images

| Push to | Image tags (both images) |
| --- | --- |
| `develop` | `devel` |
| tag `vX.Y.Z` | `X.Y.Z`, `latest` |

Every version tag is kept. The Prune images workflow (weekly, and after each Release) deletes any other tag, such as the old `edge`, `main`, `X.Y` and `sha-*` tags. It also deletes `sha256-<digest>` attestation tags once the image they refer to is gone.

## Rules and required checks

The rulesets live in [`.github/rulesets/`](.github/rulesets) and are applied by importing them (below).

- **`main` and `develop`:**
  - Changes go in through a pull request; direct pushes, force-pushes and deletion are blocked.
  - Review threads must be resolved, and no approving review is required.
  - `main` accepts merge commits only and requires the branch to be up to date with `main` before merging. `develop` accepts merge or squash.
- **`release/**`, `hotfix/**`:** no force-pushes.
- **`v*` tags:** only repository admins and the bot app can create them, and nobody (admins included) can move or delete them. These are two rulesets, since a bypass applies to every rule of its ruleset.

These checks must pass, from GitHub Actions, before merging into `main` or `develop`:

| Workflow | Checks |
| --- | --- |
| Branch policy | `branch policy` (into `main`, only same-repo `release/*` and `hotfix/*`) |
| CI | `fmt`, `clippy (native workspace)`, `clippy (wasm client)`, `test`, `design tokens`, `hygiene (typos, unused deps)`, `wasm (trunk build)`, `docker (build + smoke test)`, `browser (wasm smoke test)` |
| Security | `cargo-deny`, `zizmor (workflow audit)`, `cargo-audit`, `cargo-cooldown`, `cargo-vet`, `trivy (image + repo)` |
| CodeQL | `analyze (rust)`, `analyze (actions)` |

If you rename a job, update the rulesets in the same PR and apply them once it is merged, or merges will block on a check that no longer exists.

### Applying the rulesets (repository admin)

1. **Settings → General → Default branch:** set to `develop` (once).
2. `mise run rulesets:check` diffs `.github/rulesets/` against the live rulesets; `mise run rulesets:apply` creates or updates them, and `mise run rulesets:apply --prune` also deletes live rulesets that have no file. (Without mise: `scripts/rulesets.sh`, or import each file under **Settings → Rules → Rulesets → New ruleset ▾ → Import a ruleset**.)
3. Optionally delete any classic branch protection rules so only the rulesets apply.

## Before opening a PR

Run what CI runs ([mise](https://mise.jdx.dev) tasks, which CI also calls):

```sh
mise run check          # fmt, clippy (native + wasm), tests, design tokens, version and ignore-list sync, task definitions
mise run build          # release wasm build, as the CI wasm job does
mise run build:budget   # then its size against .config/wasm-size-budget.toml
mise run test:browser   # headless-browser smoke test against a running server (mise run serve --release)
mise -E ci install      # once (and after tool bumps): the tools CI uses, from mise.ci.toml
mise -E ci run deny     # cargo-deny as Security runs it; likewise audit, cooldown, vet, zizmor, scan:repo, fuzz, typos, shear
```

Without mise, the equivalent commands are in the `[tasks]` of [`mise.toml`](mise.toml) and [`mise.ci.toml`](mise.ci.toml); plain `rustup` is enough for the Rust toolchain because `rust-toolchain.toml` is the only place the version is set (`mise run sync` keeps `Cargo.toml`, `mise.lock` and the Dockerfile equal to it). `cargo nextest` is used for tests when installed, and `mise run coverage` needs `cargo-llvm-cov` (both in `mise.ci.toml`). The Docker job has no mise task; to build the image from an existing `dist/` run `docker build --target runtime-prebuilt --build-context client-dist=dist .`.

### Tools (mise)

Every tool is pinned once, and mise installs it from a release asset that matches the checksum in a lockfile:

| File | Holds | Loaded |
| --- | --- | --- |
| `mise.toml`, `mise.lock` | trunk (Rust comes from `rust-toolchain.toml`), the shared tasks, and task tools (fnox for `up` and `down`, uv for `fonts`) | always |
| `mise.ci.toml`, `mise.ci.lock` | the tools only CI and the image build use, CI's environment, and the tasks behind the security, fuzz and scan steps | with `MISE_ENV=ci` (every workflow sets it) or `mise -E ci` |
| `mise.dev.toml` | the local development stack (`mise run dev`: server, client and a region agent built in Docker) | in the dev environment, which `.miserc.toml` selects unless `MISE_ENV` or `-E` picks another |
| `mise.local.toml` | your own settings (`OBS_CONTACT` and so on; gitignored) | always |

To add or change a tool, edit `mise.toml` or `mise.ci.toml` (for an unusual GitHub release, a `[tool_alias]` entry names its backend), then `mise run lock` to record its checksums in both lockfiles. A workflow job installs only what it needs (the `tools` input of `.github/actions/setup-mise`, the one place the workflows' mise version is pinned; `mise.toml`'s `min_version` warns when your local mise is older, and refuses one older than 2026.7.15, which would run the task scripts under `sh`). The Pins workflow bumps every tool in `[tools]` with `mise upgrade --bump` once a release is a week old (`minimum_release_age` in `mise.toml`); task tools other than fnox stay where they are until someone bumps them. The Dockerfile's build-tools stage installs trunk, cargo-chef and cargo-auditable from the same lockfiles.

Keep colours, spacing and other UI constants in `crates/observation-client/src/theme.rs` (the design-token check fails otherwise). See [docs/design.md](docs/design.md) for architecture and [SECURITY.md](SECURITY.md) for reporting vulnerabilities.
