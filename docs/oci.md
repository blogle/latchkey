# Production OCI Image

The production image is the `x86_64-linux` Nix output `.#oci`:

```console
nix build .#oci
```

It is a reproducible `linux/amd64` image containing the optimized static musl
`latchkey` binary and the Nix `cacert` root bundle. It has no shell, compiler,
package manager, source, test fixture, deployment configuration, or secret.
The image configuration runs as UID/GID `65532:65532`; the application does
not require a writable root filesystem.

The image has no baked configuration. Its entrypoint and default command are:

```text
/bin/latchkey serve --mode standalone --config /etc/latchkey/config.toml --listen 0.0.0.0:8080
```

Mount `/etc/latchkey/config.toml` from the deployment. Credential references in
that file must resolve to mounted files or environment variables; credential
values are not image inputs.

## Contract and footprint

After building, run the archive contract and measurements without a container
runtime:

```console
scripts/checks/image-contract.sh result
scripts/checks/image-footprint.sh result
```

The contract checks metadata, nonroot execution, CA roots, absence of forbidden
content, `--help`/`--version`, and standalone startup with an external empty
configuration. The real standalone search-to-exec process smoke remains in
PR #47's `tests/runtime_smoke.rs`; it should be run against the rebased image
once that runtime lands rather than copied into packaging tests.

## Publication target

The release convention is `ghcr.io/blogle/latchkey:vX.Y.Z`, where `X.Y.Z` is
the Cargo package/release version. The release publisher owns pushing the
archive and resolving its immutable digest. The deployment reference for a
published artifact is `ghcr.io/blogle/latchkey@sha256:<published-digest>`;
the version tag is only the release label. LATCH-27 does not invent a second
registry path or publish credentials.
