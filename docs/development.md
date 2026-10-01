# Development

How to build, check, and release the Latchkey bootstrap package. Everything
below runs from the repository root and uses the pinned toolchain only —
never a system `cargo`/`rustc`.

## Bootstrap

The one-shot entry point is:

```console
$ nix run .#bootstrap --
```

The `bootstrap` flake entry (see `flake.nix`, logic in `scripts/bootstrap`)
prepends a `just` binary from the **locked** nixpkgs graph to `PATH` — never
the system one — and defaults to running `just setup`. Any other recipe can
be passed through:

```console
$ nix run .#bootstrap -- doctor
$ nix run .#bootstrap -- pr-check
```

## The cached dev environment (LATCH-3)

`just setup` materializes a **GC-rooted, fingerprint-keyed** dev environment
into the gitignored `.dev/` directory. Ordinary recipes never evaluate nix:
they source the cached `.dev/env` and invoke the absolute tool paths recorded
in it. Edit/test/lint/fmt/build recipes keep working with failing
`nix`/`nix-store` sentinels first in `PATH`.

### Cold setup

```console
$ just setup
```

Runs only when the fingerprint changes. The fingerprint hashes:

- `flake.lock`, `rust-toolchain.toml`, `Cargo.lock`, every `Cargo.toml`
- a setup version string (`LK_SETUP_VERSION` in `scripts/dev/lib.sh` — bump it
  when the devShell composition or the fingerprint scheme changes)
- the absolute checkout path (a moved/re-created checkout re-materializes)

The cold path performs exactly two nix interactions:

1. `nix print-dev-env --profile .dev/gcroot` — one evaluation that writes the
   sourceable `.dev/env` (atomically) **and** the GC root that keeps the
   devShell alive across `nix-collect-garbage`.
2. `nix build .#deps-dev .#deps-ci` — the immutable crane dependency archives,
   copied once into profile-keyed writable target dirs when their profile tree
   and fingerprints match (never mutated in place; incompatible fingerprints
   are skipped, never promised as reusable).

### Warm setup

```console
$ just setup          # no nix calls at all when the fingerprint matches
```

The warm path is a hash comparison only and reports
`zero nix calls performed`. Force a rebuild with:

```console
$ just setup true
```

`just setup true` is the **single forced-refresh command**. `just doctor`
only diagnoses — it never rebuilds and never silently refreshes.

### Edit–test–inspect loop

```console
$ source .dev/env     # optional: direct cargo use in your shell
$ just build          # dev profile, profile-keyed persistent target dir
$ just test-unit cli  # lane-scoped: cargo test --locked --lib cli::
$ just fmt            # format in place;  just fmt-check  to verify
$ just lint           # clippy, all targets, -D warnings
```

After `source .dev/env`, the pinned toolchain is on `PATH` and
`CARGO_TARGET_DIR` points at the dev-profile keyed directory, so a bare
`cargo build` reuses the same cache as `just build`. Note `just test-unit`
and `just candidate-check` select their own profile keys (see
"Profile-keyed target dirs" below), so their artifacts live in their own
directories.

### Stale-lock / toolchain repair

Any fingerprint input change (or a hand-edited `.dev/fingerprint`) makes
`just doctor` and every environment-dependent recipe fail **early**, before
any tool runs:

```console
$ just doctor
doctor: recorded fingerprint: ...
doctor: current fingerprint:  ...
doctor: error: stale setup: ...
doctor: fix: run 'just setup true' (the single forced-refresh command; doctor never rebuilds)

$ just setup true     # forced re-materialization (nix runs here)
$ just doctor         # healthy again
```

`touch Cargo.lock` alone does **not** invalidate the cache: the fingerprint
is content-based, not mtime-based. A checkout move does invalidate it (path
is an input).

## The frozen `just` surface

Root command dispatch is frozen: the recipes below are the entire surface and
live only in the justfile; logic lives in `scripts/dev/*` and
`scripts/checks/*`. Lane/suite selection is parameterized (arguments and file
names), never by editing the justfile in parallel. Optional arguments are
passed as safely quoted argv arrays (`set positional-arguments` + `"$@"`).
Unknown recipes fail closed with just's own "recipe not found" error.

| Recipe | What it does |
| ---------------- | ------------------------------------------------------------------- |
| `just` / `just --list` | List the recipes (default) |
| `just setup [refresh]` | Materialize `.dev/` when the fingerprint changed; `just setup true` forces |
| `just doctor` | Diagnose by hashes; reports `just setup true`; never rebuilds |
| `just fmt` | `cargo fmt --all` in place |
| `just fmt-check` | `cargo fmt --all -- --check` |
| `just lint` | `cargo clippy --all-targets --locked -- -D warnings` |
| `just build` | `cargo build --locked` (dev profile) |
| `just test-unit [lane]` | `cargo test --locked --lib [lane::]`; **fails if zero tests ran** |
| `just test-integration <suite> [-- args…]` | `cargo test --locked --test <suite>`; fails on a missing suite file citing its owning ticket, and on zero tests |
| `just script-test <suite>` | Python unittest discovery over `scripts/dev/tests/test_<suite>.py`; fails on zero tests |
| `just pr-check` | Fast gate: fmt-check + lint + script-test + test-unit (no OCI, no `nix flake check`, no E2E) |
| `just candidate-check` | Full gate: pr-check semantics with the **`ci` cargo profile** for cargo steps, plus `nix flake check`, `nix build .#package .#oci`, native + OCI `--help`/`--version`/serve-refusal smoke tests |
| `just image` | `nix build .#oci --out-link result-oci` |
| `just clean` | Remove selected local outputs only (see below) |

Future commands exist on the surface but **fail closed naming the owning
ticket** (they can never succeed before their ticket lands):

| Recipe(s) | Owning ticket |
| --- | --- |
| `test-integration contracts` (until `tests/contracts.rs` exists) | LATCH-2 (F02) |
| `release-plan` | LATCH-4 |
| `release-publish`, `release-verify` | LATCH-7 |
| `bench`, `foundation-audit` | LATCH-8 |
| `generate-crd` | LATCH-16 |
| `dev`, `dev-reload`, `dev-stop` | LATCH-20 |
| `e2e-standalone` | LATCH-21 |
| `k8s-render`, `k8s-test-up`, `k8s-test-run`, `k8s-test-down` | LATCH-24 |
| `mvp-acceptance` | LATCH-28 |
| `migration-preflight`, `migration-rehearse` | LATCH-29 |
| `migration-cutover`, `migration-verify`, `migration-rollback` | LATCH-30 |

The ticket→command map for future commands lives in the justfile call sites
(`scripts/dev/future.sh <ticket> <command>`); the suite→ticket map for
missing integration suites lives in `lk_suite_owner` in `scripts/dev/lib.sh`.

Cadence:

- **Run `just setup` once per toolchain/lock change** — `just doctor` tells
  you when it is stale; `just setup true` is the repair.
- **Run `just pr-check` on every PR update** — the cheap gate the `pr-fast`
  CI job enforces (`nix develop -c just pr-check`).
- **Run `just candidate-check` on merge candidates** — the full gate the
  `candidate-ready` CI job enforces (`nix develop -c just candidate-check`).
- **Real-cluster tests are opt-in** via the documented environment variable
  `LATCHKEY_TEST_KUBECONFIG` (never a Cargo flag); nothing here implies a
  cluster is needed for the local loop.

## Profile-keyed target dirs and artifact reuse

`just setup` exports `CARGO_TARGET_DIR` for the dev profile, and every
recipe overrides it for the profile it selects. The directory key
(`.dev/target/<key>`, see `lk_target_key` in `scripts/dev/lib.sh`) hashes:

- **compiler identity** — `.dev/toolchain-id`, the pinned `rustc -vV` output
  (release + commit hash + host)
- **profile** — `dev` (default recipes; build/lint/cargo-test share one dev
  tree, exactly like vanilla `target/debug`), `ci` (`LATCHKEY_PROFILE=ci`,
  used by `candidate-check`), `release` (direct release builds)
- **features** — via the `Cargo.toml` content hash
- **lockfile fingerprint** — `Cargo.lock` content hash
- **cargo config/rustflags** — `.cargo/config.toml` content hash

Separation guarantees:

- **Per worktree.** Keys live under each checkout's own `.dev/`, so parallel
  worktrees never share a target dir, never block each other on cargo's
  target-dir lock, and cannot corrupt each other's artifacts.
- **Crane deps seeds.** A cold setup copies (reflinks-by-extraction, never
  mutates) the crane `deps-*` archives into the matching profile key once,
  and only when the archive actually contains that profile's artifact tree.
  Today `deps-ci` and the release tree match; `deps-dev` ships no `debug`
  tree, so the dev key is **not** seeded from it — no reuse is promised for
  incompatible fingerprints.
- **Honest limits.** With zero external crates there are no dependency units
  to reuse: no-op builds finish instantly (`Fresh` in `cargo build -v`) and a
  one-file edit recompiles exactly the local crate. Vendor-source reuse does
  not apply until the repository actually vendors sources.

## Toolchain pins

- `rust-toolchain.toml` pins Rust **1.96.0** (minimal profile + `rustfmt`,
  `clippy`, and the `x86_64-unknown-linux-musl` std). The flake reads this
  file through `rust-bin.fromRustupToolchainFile`; it is the only place a
  Rust version is decided.
- `flake.nix` pins `nixpkgs`, `rust-overlay` (oxalica), and `crane`
  (ipetkov) by exact revisions; `flake.lock` records their hashes. All
  dependencies — toolchain components, `just`, `shellcheck`, `actionlint`,
  `python3`, `zstd`, the OCI tooling — come from that locked graph.
- No `RUSTC_BOOTSTRAP`, no nightly: everything runs on the pinned stable
  toolchain, via the cached `.dev/env`.

## Cargo profiles

Defined in `Cargo.toml`, selected per derivation via `CARGO_PROFILE` (crane
turns it into `--profile <name>`):

| Profile   | Purpose                                   | Key settings                                            |
| --------- | ----------------------------------------- | ------------------------------------------------------- |
| `dev`     | local iteration, `just build`             | `opt-level=0`, `debug=1`, `incremental=true`, `codegen-units=256`, `lto=false` |
| `test`    | `cargo test`                              | same as `dev` (tests always **unwind**)                 |
| `ci`      | candidate gate (`--profile ci`) and CI-oriented builds (`deps-ci`) | inherits `dev`, `opt-level=1`, `incremental=false`, `debug=1` |
| `release` | shippable artifacts (`package`, `oci`)    | `opt-level="z"`, `lto="thin"`, `codegen-units=1`, `strip="symbols"`, `panic="abort"`, `incremental=false` |

`panic = "abort"` appears only in `release`, so dev/test/ci tests keep
unwinding. Release/image builds are always explicit `nix build` steps — a
cargo default never silently produces them.

## Flake outputs (x86_64-linux)

| Output                         | Meaning                                                     |
| ------------------------------ | ----------------------------------------------------------- |
| `devTools`                     | toolchain + locked `just` (warming this builds the toolchain) |
| `deps-dev` / `deps-ci` / `deps-release` | crane `buildDepsOnly` caches for the native target, one per profile |
| `package`                      | native release binary (built from `deps-release`)           |
| `oci`                          | static musl image: **only** the latchkey binary + CA roots, running as `65532:65532` |
| `bootstrap`                    | `nix run .#bootstrap --` entry (locked `just` → `just setup`) |
| `devShells.default`            | `nix develop -c ...` shell: `devTools` + shellcheck + actionlint + python3 + zstd (the cached-environment tools `just setup` records into `.dev/env`) |
| `checks`                       | `fmt`, `clippy`, `unit` — built by `nix flake check`        |

## `clean`

```console
$ just clean
```

Removes **selected local outputs only**: `.dev/` (including the profile-keyed
target dirs), `./target/`, `result*` out-links, stray `*.log` files, and the
legacy `.setup-marker`. It never evicts shared caches by default: no nix
store paths, no substituter caches, and no other worktree's state are
touched. Run `just setup` afterwards to materialize the environment again.

## Caching notes

- **Dependency invalidation.** crane's `buildDepsOnly` launders its source
  down to `Cargo.toml`/`Cargo.lock` (plus `.cargo/config`): editing Rust
  function bodies does **not** change the `deps-*` derivation paths, while
  any `Cargo.lock` change does. This is asserted by the deps-identity gate
  (`nix eval --raw .#deps-ci.drvPath` before/after each edit).
- **Native vs musl are separate graphs.** `package` builds its dependencies
  for `x86_64-unknown-linux-gnu` (`deps-release`); `oci` compiles its own
  musl dependency tree with `CARGO_BUILD_TARGET=x86_64-unknown-linux-musl`.
  Artifacts are never shared across targets.
- **No Git metadata in builds.** Sources are file-tree based
  (`cleanCargoSource`); no `self.rev`/`.git` input feeds any derivation, so
  dirty working trees and commit hashes do not affect image contents.
- **Local state is never committed.** `target/`, `.dev/`, `result*`,
  `*.log`, and `.direnv/` are gitignored.
- **CI substitution.** The GitHub workflow adds the nix-community binary
  cache via the nix installer's `extra_nix_config` so the pinned toolchain
  is substituted instead of built on runners. Nothing outside the flake
  pins tool versions. CI keeps calling `nix develop -c just pr-check` /
  `candidate-check`; the recipes themselves make no nix calls except where
  the gate explicitly requires them (`nix flake check`, `nix build`).
