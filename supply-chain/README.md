# Dependency audits (cargo-vet)

[cargo-vet](https://mozilla.github.io/cargo-vet/) checks that every crate version in the
dependency tree has been reviewed by someone we trust, and fails (the `cargo-vet` check in
`.github/workflows/security.yml`; `mise -E ci run vet` locally) when one has not.

| File | Holds | Edited |
| --- | --- | --- |
| `config.toml` | The audit sets we import, our policy, and the **exemptions**: crate versions that were in the tree when vetting started, accepted without a review | by `cargo vet` (it rewrites the file and drops comments, so notes go here) |
| `audits.toml` | Our own audits (`cargo vet certify`) | by `cargo vet certify` |
| `imports.lock` | The imported audits, as fetched | by `cargo vet` (`mise -E ci run vet:imports` refreshes it) |

## Whose audits count

Imported from Mozilla, Google, the Bytecode Alliance (wasmtime), Embark Studios, ISRG
(libprio) and Zcash: they cover much of the server's tree (tokio, hyper, rustls, serde and
so on). Every new crate version that one of them has audited passes without anything here.

## Policy: the server side first

`observation-client` has `criteria = []`: its own tree (Bevy, wgpu, egui and the rest,
hundreds of crates that move together with each Bevy release) is not vetted yet. Crates the
client shares with `observation-core`, the server or the region agent are still vetted through
those crates, which keep the default `safe-to-deploy`. So everything that runs on the server
or handles its input is covered. Tightening the client's policy means exempting its tree too;
do it when the exemptions can be carried.

## When the check fails

A pull request brought in a crate version (a new crate, or a new version of one) that is in the
server side's tree and that nobody we trust has audited. `cargo vet` prints which, and
`cargo vet suggest` lists the smallest diffs to review. Then either:

- **Review it**: `cargo vet diff <crate> <old> <new>` (or `cargo vet inspect <crate> <version>`
  for a new crate), and if it does what it says and nothing else (see the criteria in
  `cargo vet help certify`), `cargo vet certify <crate> <old> <new>`. Commit `audits.toml`.
- **Exempt it**, when a review is not worth it (a large, well-known crate that an import will
  soon cover, say): `cargo vet add-exemption <crate> <version>`, and say why in the pull request.

Dependabot pull requests that fail the check are not merged automatically: whoever pushes the
audit to the Dependabot branch merges it once it is green (the auto-merge sweep only takes pull
requests whose commits are all Dependabot's).

`mise -E ci run vet:imports` fetches the imported sets again and drops exemptions they now
cover; run it now and then, and commit what changes.
