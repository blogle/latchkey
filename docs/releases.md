# Releases (LATCH-4, F05)

Deterministic, Dojo-style changelog and patch-release **planning**. Release
metadata is a pure function of the linear merged history: `scripts/release.py`
derives it from committed git history alone. Publication, verification of
published artifacts, and the release CLI wiring are owned by **LATCH-7**;
this ticket builds no publication path and edits no build metadata.

Tool: `scripts/release.py` — Python standard library only (`tomllib`), run
with the cached python3 (`LATCHKEY_PYTHON3` from `.dev/env`). The tool never
writes to the worktree: `plan` and `verify-tag` are read-only, and
`changelog` writes only the `--output PATH` you name (and refuses tracked
files).

## Concepts (adapted from the pinned Dojo2 helper)

From `dojo2/scripts/release.py` (pinned commit `5483eebc`):

- **Patch is the default bump**; `minor`/`major` only ever come from an
  explicit directive (here: the fragment's `bump` field).
- **Deterministic generated sections**: release notes are re-rendered from
  fragments every time — already-rendered historical notes are preserved
  byte-for-byte because rendering is a pure function of the fragments.
- **Exact tag-target checks**: a release tag must point at exactly the
  planned commit and be named for that commit's planned version.
- **Idempotent publication planning**: planning the same commit twice (or a
  publisher starting out of order) yields the same mapping.

Deliberately **not** adopted: the `/merge` bot, per-command `nix develop`,
shared pre-merge version edits, and any mutation of `CHANGELOG.md` or
`Cargo.toml` on `master`.

## Changelog fragments

The full fragment contract lives in [.changes/README.md](../.changes/README.md):
one new `.changes/<ISSUE-ID>.toml` per PR (`category`, `summary`, optional
`bump` defaulting to `patch`, optional `issue` that must match the filename),
valid schema, no secrets (documented simple heuristic), and previously merged
fragments immutable. The filename is authoritative, which keeps the
already-merged `LATCH-1/2/3` fragments (no `issue` field) valid without
modification.

`scripts/release.py validate-fragment [--base REF]` is the PR gate; `plan`
re-enforces the same invariants per traversed commit.

## History anchor and traversal (`release-policy.toml`)

```toml
[history]
anchor = "52623fd4c3622288ee0b939e547c6f07e03423e5"
starting_version = "0.0.0"
```

- The version is derived from **committed history, never from the mutable
  latest tag**: walk the **first-parent** chain from `--commit` back to the
  anchor (oldest first), starting at `starting_version`, and fold each squash
  commit.
- Every chained commit must introduce **exactly one new** `.changes/*.toml`
  (relative to its first parent) and may not modify or delete any fragment
  that already exists. Each commit bumps patch unless its fragment explicitly
  requests `minor` or `major`. N squash commits therefore yield N versions,
  each prefix folded independently.
- The anchor itself may be planned: it yields `starting_version` with empty
  notes and no ancestors.

Rejections (in two phases — all structural checks first, then fragment checks
in oldest-first order):

1. Missing anchor object, or an anchor not on the target's first-parent
   chain (**rewritten/moved anchor**).
2. **Merge commits** anywhere on the chain (release history is linear
   squash merges; side-branch commits are never traversed unless you plan a
   commit whose own first-parent chain contains them).
3. Per commit: modified/deleted fragment (**immutability**), then zero
   fragments (**missing**) or 2+ (**ambiguous**), then schema/filename/secret
   violations, then an issue id already used earlier in the traversal
   (**reuse**, case-insensitive).

The policy file itself is configuration read from the checkout
(`<root>/release-policy.toml`, override with `--policy PATH`); everything
else — commits, trees, fragment contents, notes — is read from commit trees,
never from the worktree.

## CLI (canonical forms)

```console
$ python3 scripts/release.py validate-fragment [--base REF]
$ python3 scripts/release.py plan --commit SHA --json
$ python3 scripts/release.py changelog --commit SHA --output PATH
$ python3 scripts/release.py verify-tag --tag TAG --commit SHA
```

Global options precede the subcommand: `--root DIR` (repository root,
default this checkout) and `--policy PATH` (default `<root>/release-policy.toml`).
Exit 0 on success, 1 on any policy rejection (`latchkey: error: …` on stderr),
2 on CLI usage errors.

### `plan --commit SHA --json`

Emits one JSON document (`schema: latchkey-release-plan/v1`):

| Field                  | Meaning                                                        |
| ---------------------- | -------------------------------------------------------------- |
| `policy`               | The anchor and starting version used (provenance).             |
| `commit`, `tree`       | Full source SHA and its tree hash.                             |
| `parent`               | First parent (null only when planning the anchor).             |
| `version`, `tag`       | The folded version and its canonical tag name (`vX.Y.Z`).      |
| `fragment`             | The fragment introduced by this commit (`null` at the anchor). |
| `artifacts`            | Expected immutable artifact names (table below).               |
| `notes_sha256`         | SHA-256 of the rendered per-version notes for this commit.     |
| `unreleased_ancestors` | A flat plan entry (same fields as above, minus `schema`, `policy` and the nested list) for **every** anchored ancestor, oldest first. |

Batch folding: `plan --commit C3` contains entries equal to
`plan --commit C1` and `plan --commit C2` (core fields deep-equal). Because
the version derives only from the prefix, an **out-of-order publisher** that
starts at an older commit computes the identical mapping — a publisher picks
its commit's entry from any larger plan. `unreleased_ancestors` lists all
anchored ancestors unfiltered: tags are never consulted, which is what keeps
the mapping pure.

The plan JSON is how the release version reaches build metadata: LATCH-7
consumes `version` from it. This ticket performs **no build wiring** —
`Cargo.toml`, dependency artifacts, and the flake are not edited.

### Expected artifact names (for LATCH-7)

Derived from the version alone; immutability is enforced by `verify-tag` and
the published checksums, not by embedding mutable state:

| Artifact        | Name                                        |
| --------------- | ------------------------------------------- |
| Source archive  | `latchkey-vX.Y.Z-src.tar.gz`                |
| Binary archive  | `latchkey-vX.Y.Z-linux-x86_64-musl.tar.gz`  |
| OCI reference   | `ghcr.io/blogle/latchkey:vX.Y.Z`            |
| Checksums       | `latchkey-vX.Y.Z-SHA256SUMS.txt`            |
| Per-release notes | `latchkey-vX.Y.Z-notes.md`                |
| Cumulative changelog | `CHANGELOG.md`                         |

### `changelog --commit SHA --output PATH`

Renders the cumulative Dojo-style document: a header comment (naming the
generating command and commit) followed by one `## vX.Y.Z` section per
version, newest first. Each section is exactly the notes for that version:

```markdown
## v0.1.0

- Drop the legacy config format (TCK-2)
- Require static credentials (TCK-2)
```

Bullets are `- <summary> (<issue>)`, ordered by category rank
(`Breaking`, `Added`, `Changed`, `Fixed`) then history order. With an empty
anchored history the document contains a single deterministic
"No releases yet" line.

**Deliberate generated-artifact changelog adaptation (recorded here per the
ticket):** Dojo2 edits a committed `CHANGELOG.md` in a release PR. Latchkey
instead treats the rendered changelog as a *release output artifact* —
`changelog` writes only `--output PATH`, refuses to write any git-tracked
file, and the root `CHANGELOG.md` is a short pointer document. Nothing
generated is ever committed back to `master`, so release automation cannot
create a commit loop or a shared-file conflict, and historical sections stay
deterministic because they are always re-rendered from the immutable
fragments.

### `verify-tag --tag TAG --commit SHA`

Exact tag-target checks, in order:

1. `TAG` must be a well-formed `vMAJOR.MINOR.PATCH` name.
2. The tag must exist and **point at exactly** the given commit (annotated
   tags are dereferenced).
3. The commit must plan successfully under the current policy.
4. The tag name must equal that commit's planned version (`v` + `version`).

### `validate-fragment [--base REF]`

PR gate; see [.changes/README.md](../.changes/README.md) for the contract and
the full check order. `REF` defaults to `HEAD`; PR CI passes the PR base
(e.g. `--base origin/master`).

## Tests

```console
$ just script-test release_policy
$ just script-test dispatch
$ just pr-check
```

`tests/release_policy/` builds synthetic git repositories in temp directories
and exercises the real CLI end to end. The suite discovery rule is generic
(code comment in `scripts/dev/lib.sh`): a suite `X` resolves to
`scripts/dev/tests/test_X.py`, else a directory `tests/X/` containing
`test_*.py`, else `scripts/ci/tests/test_X.py`.

## Known state of this repository's history

The three commits after the anchor that predate the fragment contract —
`a014e4e` (owner queue activation), `8c997bc` (config syntax fix) and
`846e01f` (Mergify bot config upgrade) — carry no fragment. Planning any
commit after `a014e4e` with the default policy therefore **rejects at
`a014e4e`**, the first fragmentless commit, by design: there is no exemption
or bypass. The fragment-bearing suffix (everything after `846e01f`) plans
normally when the policy anchors there; see `.changes/README.md` and the
LATCH-4 evidence for recorded outputs.
