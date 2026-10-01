# Continuous integration

## Ordinary pull requests

`.github/workflows/pr.yml` accepts only same-repository pull requests to
`master`. It checks out the PR base first and runs `validate_pr.py --event-only`
from that trusted revision before checking out the requested head into a
separate directory. It validates exact same-repository head SHA, rejects merge
commits since merge base, validates the release fragment, then runs `just
pr-check`. `pr-fast` is the stable queue-admission check; queue PRs do not run
this workflow.

## Candidate metadata and trust boundary

`.github/workflows/candidate.yml` exposes `candidate-ready`. It accepts a
Mergify queue PR only after trusted-base event validation confirms the bot,
queue ref, master target and exact event SHA; manual dispatch is restricted to
the configured trusted actor on `master`. The trusted-base copy executes all
event and metadata validation before candidate source is checked out.

For queue runs, the workflow consumes the official `mergify ci queue-info`
output. Its ordered `pull_requests` and `checking_base_sha` determine queue
order/base; a separate read-only GitHub PR API call resolves each exact PR
number to its API `head.sha` and `base.sha`. The requested candidate tree is
resolved from the exact event commit. Dispatch supplies one explicit JSON
`batch_json` containing `base_sha`, `requested_tree`, and ordered
`pull_requests` records `{number, head_sha, base_sha}`; it does not infer order
or make unused inputs. Both paths validate and serialize the identical
`latchkey-candidate-batch/v1` schema. API credentials are present only during
trusted metadata resolution; candidate Python and Cargo receive none.

## Prefix execution and evidence

`candidate.py run` independently reconstructs each ordered base-to-head patch
with Git plumbing, rejects merge commits and anything other than one newly
added fragment, and writes synthetic one-parent commits. It runs the F05
`scripts/release.py plan --commit ... --json`, overlays the planned version
with `inject_root_version.py` in a disposable worktree, then builds the root
binary with pinned Cargo 1.96.0 and the cached development environment. Each
versioned binary must report the planned `--version` and support `--help`.
The active foundation gates are selected from `ci/capabilities.toml` and run
for each prefix; the complete `just candidate-check` runs once for the final
candidate after prefix validation. Earlier prefix failure is never repaired
by a later pass.

The per-prefix build uses the shared Cargo target because the experiment in
`edb7609` demonstrated that changing only the root package version and its
root lock entry recompiles only Latchkey with `--locked --offline`. Cargo
target reuse is limited to matching toolchain, target, profile, lock and
feature settings. This does **not** mean Nix dependency derivations are reused:
the `.#package` and `.#oci` derivations change under the root-lock-version
overlay. Nix builds are deliberately performed once by final `candidate-check`,
not once per versioned prefix.

Successful runs write canonical `candidate-evidence/manifest.json`, versioned
root binaries under `candidate-evidence/artifacts/`, and deterministic
`SHA256SUMS`. Manifest validation binds ordered PR heads, exact base, candidate
SHA/tree, each synthetic SHA/tree/version/suite result and artifact digest,
Cargo.lock/toolchain/profile/features fingerprints, final result, and
separate provenance. Artifact names are deterministic from the content tree,
lock, toolchain, features, profile and ordered versions; run ID exists only in
provenance. The workflow uploads the exact manifest/checksum/artifact paths
only after success. A failed run may leave runner diagnostics but does not
upload successful candidate evidence.

### F07 lookup/download contract

F07 must locate the successful `candidate-ready` check for the exact candidate
SHA in repository `blogle/latchkey`, identify its successful workflow run,
download the artifact by the manifest's deterministic `artifact_name`, verify
`SHA256SUMS`, then validate the manifest schema, candidate SHA/tree, ordered
heads, version and binary digest before consuming any artifact. Do not select
by latest run, branch name, or a broad cache restore key.

`ci/gates.toml` records policy; it is not evidence that a successful workflow
run exists. Candidate readiness must remain a required final merge condition
only after its workflow is enabled and observed green.
