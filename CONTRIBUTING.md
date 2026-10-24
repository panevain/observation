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

Dependabot and the Pins workflow target `develop` (see `.github/dependabot.yml` and `.github/workflows/pins.yml`).

### Releasing

1. Run the **Prepare release** workflow (Actions → Prepare release → version `0.2.0`). It creates `release/0.2.0` from `develop`, bumps `version` in the root `Cargo.toml`, updates `Cargo.lock`, dates the `CHANGELOG.md` entry and opens the PR into `main`. By hand: `git switch -c release/0.2.0 origin/develop`, make the same edits (`scripts/set-version.sh 0.2.0`), and open the PR.
2. Review the PR `release/0.2.0` → `main` (stabilising fixes go on the branch). When it is green, merge it with a merge commit.
3. The rest is automatic. **Release tag** tags the merge commit `v0.2.0`; **Release** publishes `ghcr.io/panevain/observation` and `ghcr.io/panevain/observation-regions` as `0.2.0` and `latest`, each with a cosign signature, provenance and an SBOM, and a GitHub release with the changelog section; **Treemap** opens a PR to `develop` to refresh the bundle-size chart; and Release tag opens `backmerge/v0.2.0` → `develop`, which merges itself once its checks pass. If the back-merge conflicts (usually `CHANGELOG.md` or `Cargo.lock`), merge `develop` into `backmerge/v0.2.0`, resolve, and push.

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
| CI | `fmt`, `clippy (native workspace)`, `clippy (wasm client)`, `test`, `design tokens`, `wasm (trunk build)`, `docker (build + smoke test)` |
| Security | `cargo-deny`, `zizmor (workflow audit)`, `cargo-audit`, `trivy (image + repo)` |
| CodeQL | `analyze (rust)`, `analyze (actions)` |

If you rename a job, update the rulesets in the same PR and apply them once it is merged, or merges will block on a check that no longer exists.

### Applying the rulesets (repository admin)

1. **Settings → General → Default branch:** set to `develop` (once).
2. `mise run rulesets:check` diffs `.github/rulesets/` against the live rulesets; `mise run rulesets:apply` creates or updates them, and `mise run rulesets:apply --prune` also deletes live rulesets that have no file. (Without mise: `scripts/rulesets.sh`, or import each file under **Settings → Rules → Rulesets → New ruleset ▾ → Import a ruleset**.)
3. Optionally delete any classic branch protection rules so only the rulesets apply.

## Before opening a PR

Run what CI runs ([mise](https://mise.jdx.dev) tasks, which CI also calls):

```sh
mise run check     # fmt, clippy (native + wasm), tests, design tokens, version and ignore-list sync
mise run build     # release wasm build, as the CI wasm job does
cargo deny check   # if installed
```

Without mise, the equivalent commands are in the `[tasks]` of [`mise.toml`](mise.toml); plain `rustup` is enough for the Rust toolchain because `rust-toolchain.toml` is the only place the version is set (`scripts/check-toolchain-sync.sh` keeps `Cargo.toml` and the Dockerfile equal to it). `cargo nextest` is used for tests when installed, and `mise run coverage` needs `cargo-llvm-cov`. The Docker job has no mise task; to build the image from an existing `dist/` run `docker build --target runtime-prebuilt --build-context client-dist=dist .`.

Keep colours, spacing and other UI constants in `crates/observation-client/src/theme.rs` (the design-token check fails otherwise). See [docs/design.md](docs/design.md) for architecture and [SECURITY.md](SECURITY.md) for reporting vulnerabilities.
