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

## The `just` workflow

| Recipe           | What it does                                                        |
| ---------------- | ------------------------------------------------------------------- |
| `just setup`     | Warms the toolchain and records a gitignored `.setup-marker` with a fingerprint of `rust-toolchain.toml` + `flake.lock` + `Cargo.lock` |
| `just doctor`    | Verifies environment health; fails with guidance if setup is stale or missing |
| `just fmt-check` | `cargo fmt --all -- --check` through `nix develop`                  |
| `just lint`      | `cargo clippy --all-targets --locked -- -D warnings`                |
| `just test-unit` | `cargo test --all-targets --locked` (dev/test profile, unwinding)   |
| `just build`     | `cargo build --locked` (dev profile)                                |
| `just image`     | `nix build .#oci`                                                   |
| `just pr-check`  | Cheap gate: `fmt-check` + `lint` + `test-unit`                       |
| `just candidate-check` | Full gate: everything `pr-check` runs, plus `nix flake check`, `nix build .#package .#oci`, native `--help`/`--version`/serve-refusal smoke tests, and the same smoke tests against the OCI image (layers extracted, static binary run directly — no container daemon needed) |

Cadence:

- **Run `just setup` once per toolchain or lock change** (i.e. whenever
  `rust-toolchain.toml`, `flake.lock`, or `Cargo.lock` changes). It is cheap
  after the first run; `just doctor` tells you when it is stale.
- **Run `just pr-check` on every PR update** — it is the cheap gate and is
  what the `pr-fast` CI job enforces.
- **Run `just candidate-check` on merge candidates** — it is the full gate
  and is what the `candidate-ready` CI job enforces (Mergify requires both
  checks for the bootstrap queue).

Unknown recipes fail closed: `just` errors if a recipe is not defined here,
and no recipe silently pretends to do work it does not do.

## Toolchain pins

- `rust-toolchain.toml` pins Rust **1.96.0** (minimal profile + `rustfmt`,
  `clippy`, and the `x86_64-unknown-linux-musl` std). The flake reads this
  file through `rust-bin.fromRustupToolchainFile`; it is the only place a
  Rust version is decided.
- `flake.nix` pins `nixpkgs`, `rust-overlay` (oxalica), and `crane`
  (ipetkov) by exact revisions; `flake.lock` records their hashes. All
  dependencies — toolchain components, `just`, the OCI tooling — come from
  that locked graph.

## Cargo profiles

Defined in `Cargo.toml`, selected per derivation via `CARGO_PROFILE` (crane
turns it into `--profile <name>`):

| Profile   | Purpose                                   | Key settings                                            |
| --------- | ----------------------------------------- | ------------------------------------------------------- |
| `dev`     | local iteration, `just build`             | `opt-level=0`, `debug=1`, `incremental=true`, `codegen-units=256`, `lto=false` |
| `test`    | `cargo test`                              | same as `dev` (tests always **unwind**)                 |
| `ci`      | CI-oriented builds (`deps-ci`)            | inherits `dev`, `opt-level=1`, `incremental=false`, `debug=1` |
| `release` | shippable artifacts (`package`, `oci`)    | `opt-level="z"`, `lto="thin"`, `codegen-units=1`, `strip="symbols"`, `panic="abort"`, `incremental=false` |

No `RUSTC_BOOTSTRAP`, no nightly: everything runs on the pinned stable
toolchain. `panic = "abort"` appears only in `release`, so dev/test/ci tests
keep unwinding.

## Flake outputs (x86_64-linux)

| Output                         | Meaning                                                     |
| ------------------------------ | ----------------------------------------------------------- |
| `devTools`                     | toolchain + locked `just` (warming this builds the toolchain) |
| `deps-dev` / `deps-ci` / `deps-release` | crane `buildDepsOnly` caches for the native target, one per profile |
| `package`                      | native release binary (built from `deps-release`)           |
| `oci`                          | static musl image: **only** the latchkey binary + CA roots, running as `65532:65532` |
| `bootstrap`                    | `nix run .#bootstrap --` entry (locked `just` → `just setup`) |
| `devShells.default`            | `nix develop -c ...` shell (exposes `devTools`)             |
| `checks`                       | `fmt`, `clippy`, `unit` — built by `nix flake check`        |

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
- **Local state is never committed.** `target/`, `result*`, `*.log`, and the
  `.setup-marker` are gitignored.
- **CI substitution.** The GitHub workflow adds the nix-community binary
  cache via the nix installer's `extra_nix_config` so the pinned toolchain
  is substituted instead of built on runners. Nothing outside the flake
  pins tool versions.
