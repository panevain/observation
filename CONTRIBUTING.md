# Contributing

## Branching model (git-flow)

| Branch | Purpose | Created from | Merges into | How |
| --- | --- | --- | --- | --- |
| `main` | Released code only. Every commit is a release or hotfix merge. | – | – | Protected; PRs from `release/*` and `hotfix/*` only |
| `develop` | Integration branch and the GitHub default branch | `main` (once) | – | Protected; all other work lands here by PR |
| `feature/<topic>`, `bugfix/<topic>` | Day-to-day work | `develop` | `develop` | PR, squash or merge commit |
| `release/<x.y.z>` | Stabilising a release (version bump, docs, fixes only) | `develop` | `main`, then `develop` | Two PRs, merge commits |
| `hotfix/<x.y.z>` | Urgent fix to a release | `main` | `main`, then `develop` | Two PRs, merge commits |

Dependabot targets `develop` (see `.github/dependabot.yml`).

### Releasing

1. `git switch -c release/0.2.0 origin/develop`, then bump `version` in the root `Cargo.toml` (`[workspace.package]`) and run `cargo check` so `Cargo.lock` updates.
2. Open a PR `release/0.2.0` → `main`. When it is green, merge it with a merge commit.
3. Tag the merge commit on `main` (admins only, see the tag ruleset): `git tag -a v0.2.0 -m "v0.2.0" origin/main && git push origin v0.2.0`. The Release workflow publishes `ghcr.io/panevain/observation:0.2.0`, `:0.2` and `:latest` with provenance and an SBOM.
4. Open a PR `release/0.2.0` → `develop` to bring the version bump back, then delete the release branch.

Hotfixes work the same way, starting from `main` with `hotfix/<x.y.z>`.

### Images

| Push to | Image tags |
| --- | --- |
| `develop` | `edge`, `sha-<commit>` |
| `main` | `main`, `sha-<commit>` |
| tag `vX.Y.Z` | `X.Y.Z`, `X.Y`, `latest`, `sha-<commit>` |

## Rules and required checks

The rulesets live in [`.github/rulesets/`](.github/rulesets) and are applied by importing them (below).

- **`main` and `develop`:**
  - Changes go in through a pull request; direct pushes, force-pushes and deletion are blocked.
  - Review threads must be resolved, and no approving review is required.
  - `main` accepts merge commits only and requires the branch to be up to date with `main` before merging. `develop` accepts merge or squash.
- **`release/**`, `hotfix/**`:** no force-pushes.
- **`v*` tags:** only repository admins can create them, and nobody can move or delete them.

These checks must pass, from GitHub Actions, before merging into `main` or `develop`:

| Workflow | Checks |
| --- | --- |
| Branch policy | `branch policy` (into `main`, only same-repo `release/*` and `hotfix/*`) |
| CI | `fmt`, `clippy (native workspace)`, `clippy (wasm client)`, `test`, `design tokens`, `wasm (trunk build)`, `docker (build + smoke test)` |
| Security | `cargo-deny`, `cargo-audit`, `trivy (image + repo)` |
| CodeQL | `analyze (rust)`, `analyze (actions)` |

If you rename a job, update the rulesets in the same PR and re-import them, or merges will block on a check that no longer exists.

### Applying the rulesets (one-time, repository admin)

1. **Settings → General → Default branch:** set to `develop`.
2. **Settings → Rules → Rulesets → New ruleset ▾ → Import a ruleset.** Import each file in `.github/rulesets/`, check it, and save.
3. Optionally delete any classic branch protection rules so only the rulesets apply.

## Before opening a PR

Run what CI runs ([mise](https://mise.jdx.dev) tasks, which CI also calls):

```sh
mise run check     # fmt, clippy (native + wasm), tests, design tokens
mise run build     # release wasm build, as the CI wasm job does
cargo deny check   # if installed
```

Without mise, the equivalent commands are in the `[tasks]` of [`mise.toml`](mise.toml). The Docker job has no mise task.

Keep colours, spacing and other UI constants in `crates/observation-client/src/theme.rs` (the design-token check fails otherwise). See [docs/design.md](docs/design.md) for architecture and [SECURITY.md](SECURITY.md) for reporting vulnerabilities.
