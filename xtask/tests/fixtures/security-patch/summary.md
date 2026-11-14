Latest release: **v1.2.3**

Crate advisories that a patch release fixes:

- [RUSTSEC-2022-0013](https://rustsec.org/advisories/RUSTSEC-2022-0013): `regex` 1.5.4 -> 1.5.5 (Regexes with large repetitions)

Crate advisories with no semver-compatible fix (they need a code change or an upstream release; add an ignore with a reason only if the code is unreachable):

- [RUSTSEC-2020-0071](https://example.com/a): `time` 0.1.43, patched in nothing yet (Potential segfault)

`ghcr.io/panevain/observation:1.2.3`: fixable OS vulnerabilities:

| Severity | ID | Package | Installed | Fixed | Rebuild fixes it |
| --- | --- | --- | --- | --- | --- |
| CRITICAL | CVE-1 | libssl3 | 1 | 2 | no, the base image has not been rebuilt yet |
| MEDIUM | CVE-2 | libc6 | 1 | 2 | yes |
| LOW | CVE-0 | zlib | 1 | 2 | yes |

`ghcr.io/panevain/observation-regions:1.2.3`: not published.

**Action:** a patch release (hotfix.yml) fixes the items marked above.
