{
  description = "Latchkey bootstrap: pinned Rust 1.96.0 toolchain, Crane builds, and OCI image";

  inputs = {
    # Pinned once for LATCH-1 (chosen 2026-09-30); revisions are recorded
    # here and locked (with hashes) in flake.lock. Do not bump casually:
    # every pin change invalidates the toolchain and all Cargo caches.
    nixpkgs.url = "github:NixOS/nixpkgs/b4fd65b198c599cbe814fcb9f42d25d021595ec9";

    # Provides rust-bin.fromRustupToolchainFile (reads rust-toolchain.toml,
    # which pins Rust 1.96.0 plus rustfmt, clippy, and the musl std).
    rust-overlay = {
      url = "github:oxalica/rust-overlay/ed3a19fd0439ed618ec5fe1e12f0ba69a8be38b5";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # crane declares no inputs of its own, so there is nothing to follow.
    crane.url = "github:ipetkov/crane/73b980519cefc727a5f6cc8e5c0947a2f9be6edd";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      crane,
      ...
    }:
    let
      # The MVP targets exactly one platform; every output below exists for
      # x86_64-linux only.
      system = "x86_64-linux";

      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };

      # The single toolchain definition: rust-toolchain.toml decides the
      # channel, components, and targets for native and musl builds alike.
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

      # Every cargo invocation in this flake goes through this crane instance.
      craneLib = (crane.mkLib pkgs).overrideToolchain (_: rustToolchain);

      # Source for every cargo derivation. cleanCargoSource keeps only
      # Cargo/Rust inputs; no Git metadata (self.rev, .git) participates
      # anywhere in the graph.
      src = craneLib.cleanCargoSource ./.;

      # Package version, read from the manifest (no Git metadata involved).
      packageMeta = builtins.fromTOML (builtins.readFile ./Cargo.toml);

      # Cargo profiles live in Cargo.toml; each derivation selects one via
      # CARGO_PROFILE, which crane's cargoWithProfile helper turns into
      # `--profile <name>` (or `--release`).
      nativeArgs = {
        inherit src;
        strictDeps = true;
      };

      # ---- dependency-only derivations (cache-warming exports) --------------
      # buildDepsOnly launders its source down to Cargo.toml/Cargo.lock (and
      # .cargo/config), so editing Rust function bodies cannot invalidate
      # these derivations, while any Cargo.lock change does. That property is
      # what the deps-identity gate checks on .#deps-ci.

      deps-dev = craneLib.buildDepsOnly nativeArgs; # profile: dev

      deps-ci = craneLib.buildDepsOnly (
        nativeArgs // {
          env.CARGO_PROFILE = "ci";
        }
      ); # profile: ci

      deps-release = craneLib.buildDepsOnly (
        nativeArgs // {
          env.CARGO_PROFILE = "release";
          # Tests always run under the dev/test profiles (just test-unit,
          # nix flake check); the release tree never compiles test binaries.
          doCheck = false;
        }
      ); # profile: release

      # ---- packages ---------------------------------------------------------

      # Releasable native binary (release profile: opt-z, thin LTO, single
      # codegen unit, stripped, panic=abort), built from its own native
      # dependency tree.
      package = craneLib.buildPackage (
        nativeArgs // {
          cargoArtifacts = deps-release;
          env.CARGO_PROFILE = "release";
          doCheck = false;
        }
      );

      # Fully static musl binary for the OCI image. This build compiles its
      # own dependency tree for x86_64-unknown-linux-musl; native artifacts
      # are never reused here (and musl artifacts never feed native builds).
      muslTarget = "x86_64-unknown-linux-musl";

      muslArgs = nativeArgs // {
        CARGO_BUILD_TARGET = muslTarget;
      };

      musl-deps = craneLib.buildDepsOnly (
        muslArgs // {
          env.CARGO_PROFILE = "release";
          doCheck = false;
        }
      );

      musl-package = craneLib.buildPackage (
        muslArgs // {
          cargoArtifacts = musl-deps;
          env.CARGO_PROFILE = "release";
          doCheck = false;
        }
      );

      # The image root contains exactly the static latchkey binary and the CA
      # root bundle: no test binaries, no shell, no source, no Git metadata.
      # uid/gid 65532 runs the process (no passwd entry is required).
      oci = pkgs.dockerTools.buildLayeredImage {
        name = "latchkey";
        tag = packageMeta.package.version;
        contents = [
          musl-package
          pkgs.cacert
        ];
        config = {
          User = "65532:65532";
          Entrypoint = [ "/bin/latchkey" ];
        };
      };

      # ---- developer tooling ------------------------------------------------

      # Cached developer tools from the locked graph: the pinned Rust
      # toolchain plus `just` (recipes never reach for the system PATH).
      devTools = pkgs.symlinkJoin {
        name = "latchkey-dev-tools";
        paths = [
          rustToolchain
          pkgs.just
        ];
      };

      # `nix run .#bootstrap -- [recipe]`: a `just` from the locked graph
      # (see scripts/bootstrap), defaulting to `just setup`.
      bootstrap = pkgs.writeShellApplication {
        name = "bootstrap";
        runtimeInputs = [
          pkgs.just
          pkgs.coreutils
        ];
        meta.mainProgram = "bootstrap";
        text = builtins.readFile ./scripts/bootstrap;
      };

      # devShell used by `nix develop -c ...` and by `just setup`
      # (`nix print-dev-env`) so local shells, the cached `.dev/env`, and CI
      # see the same tools. shellcheck/actionlint/python3/zstd are the
      # cached-environment additions of LATCH-3: shellcheck lints the
      # dispatcher scripts, actionlint is available for workflow linting,
      # python3 runs `just script-test`, and zstd unpacks the crane deps
      # seed archives during a cold setup.
      devShell = pkgs.mkShell {
        packages = [
          devTools
          pkgs.shellcheck
          pkgs.actionlint
          pkgs.python3
          pkgs.zstd
        ];
      };
    in
    {
      packages.${system} = {
        inherit
          package
          oci
          devTools
          bootstrap
          ;
        "deps-dev" = deps-dev;
        "deps-ci" = deps-ci;
        "deps-release" = deps-release;
      };

      # `nix flake check` builds these: formatting, lints (dev profile), and
      # unit tests (dev profile) over the native dependency tree.
      checks.${system} = {
        fmt = craneLib.cargoFmt { inherit src; };

        clippy = craneLib.cargoClippy (
          nativeArgs // {
            cargoArtifacts = deps-dev;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          }
        );

        unit = craneLib.cargoTest (
          nativeArgs // {
            cargoArtifacts = deps-dev;
            cargoTestExtraArgs = "--all-targets";
          }
        );
      };

      devShells.${system}.default = devShell;
    };
}
