# Changelog fragments

Every pull request — implementation, docs-only, config-only, everything —
adds exactly one changelog fragment here. There is **no release bypass or
skip** for a PR that "only touches docs": the fragment is how the release
history learns about the PR at all.

Each fragment is a TOML file named after the issue that owns the PR:

```toml
# .changes/LATCH-4.toml
category = "Added"
summary = "Add deterministic Dojo-style changelog and release planning"
bump = "patch"
```

## Contract

| Field      | Required | Type                  | Rules                                                        |
| ---------- | -------- | --------------------- | ------------------------------------------------------------ |
| (filename) | yes      | `<ISSUE-ID>.toml`     | Letters, digits and dashes ending `-<number>` (e.g. `LATCH-4.toml`). The **filename is authoritative**: it is the issue id. |
| `category` | yes      | string                | One of `Added`, `Changed`, `Fixed`, `Breaking`.              |
| `summary`  | yes      | string, or array of strings | Non-empty entry/entries. An array renders one bullet per entry for the same issue. |
| `bump`     | no       | string                | `patch` (the default when omitted), `minor`, or `major`. Only ever **explicit** `minor`/`major` move the release off the patch train; normal implementation agents do not choose semantic bumps independently — they use `patch` unless a ticket explicitly directs otherwise. There is no `release:none` for normal merges. |
| `issue`    | no       | string                | Must equal the filename stem when present; the filename wins on any conflict. Already-merged fragments without this field stay valid. |

Unknown fields, malformed TOML, out-of-range categories/bumps, empty
summaries, and filename/`issue` mismatches are all rejected.

## Validation (`scripts/release.py validate-fragment`)

```console
$ python3 scripts/release.py validate-fragment --base origin/master
```

Checked, in order, against the base commit:

1. **Exactly one new fragment** per PR (a docs-only PR included).
2. **Previously merged fragments are immutable**: any modification or
   deletion of a fragment introduced by earlier commits is rejected.
3. **Schema**: filename pattern, TOML shape, required/allowed fields, value
   ranges, `issue`/filename agreement.
4. **No secrets** (documented simple heuristic): the fragment text is scanned
   for well-known credential shapes — PEM private-key headers, AWS access key
   ids (`AKIA…`), GitHub tokens (`ghp_…`, `github_pat_…`), Slack tokens
   (`xox…`), and any credential-looking assignment such as
   `password: <value>` / `api_key = <value>`. Matches are reported by label
   only, never by content. This is a tripwire, not a substitute for review.
5. **No duplicate issue ids**: issue ids are compared case-insensitively, so
   `latch-1.toml` may not coexist with (or re-introduce) `LATCH-1`.

The same invariants are re-enforced at plan time, per traversed commit, by
`scripts/release.py plan` — see docs/releases.md.

## Workflow

1. On every PR, add `.changes/<ISSUE-ID>.toml` (one file, one PR).
2. Run `python3 scripts/release.py validate-fragment --base <pr-base>`.
3. Never edit a fragment that is already on the target branch — history is
   immutable; fix forward with a new fragment instead.

Fragments are consumed by `scripts/release.py changelog`/`plan` to render the
generated, never-committed cumulative changelog (root `CHANGELOG.md` is only a
pointer document). Full release rules: `docs/releases.md`.
