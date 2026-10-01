# Continuous integration

## Ordinary pull requests

`.github/workflows/pr.yml` accepts only same-repository pull requests to
`master`. It checks out the PR base first and runs `validate_pr.py --event-only`
from that trusted revision before checking out the requested head into a
separate directory. The candidate checkout is compared to the event SHA. Its
own build/test files are then executed with the normal read-only workflow
token. `pr-fast` is the stable queue-admission check; Mergify queue PRs do not
run this workflow.

## Candidate trigger and trust boundary

`.github/workflows/candidate.yml` exposes the stable `candidate-ready` check.
It is configured only for a Mergify queue PR (prefix **and** `mergify[bot]`
author, base `master`, exact event SHA) or a workflow dispatch on `master` by
an exact username in `LATCHKEY_CI_TRUSTED_DISPATCH_ACTORS`. Dispatch input
`base_sha` and `ordered_heads` must be valid, non-empty, unique full SHAs (one
to three heads). The validator is loaded from the event base/master checkout
before candidate checkout. No checks-write or publication token is granted.

The queue job installs the official Mergify CLI and calls `mergify ci
queue-info`; it does not infer membership/order from the temporary branch
name. Its documented JSON includes ordered `pull_requests` IDs and
`checking_base_sha`, but does not include each PR's head/base SHAs. Those must
be resolved from GitHub PR metadata before reconstruction. Missing or
malformed metadata must fail closed.

## Current implementation boundary

`candidate.py` now has isolated Git-worktree reconstruction primitives and
checks the one-new-fragment invariant, one-parent synthetic prefix commits,
batch ordering and final tree match. Its unit suite includes real temporary
Git repositories. These primitives are not yet an end-to-end candidate
executor: workflow artifact production/upload, F05 plan invocation for every
prefix, version-injected binary builds, suite execution, and manifest
validation are not implemented. The workflow's `candidate.py run` command
currently fails closed with an explicit blocker rather than claiming success.
The candidate job therefore does not yet satisfy the final-merge contract and
this work must not be represented as complete.

## Suite and future artifact contract

`ci/capabilities.toml` remains authoritative for cumulative gate selection.
The intended evidence schema is canonical JSON
`latchkey-candidate-evidence/v1`, with ordered PR heads, exact base, each
prefix/tree, Cargo.lock/toolchain/features/profile/version fingerprints,
active suites, SHA-256 artifact checksums and result. Artifacts must be
downloaded by F07 using the exact workflow run/artifact identity, then matched
to source commit, content tree and version from the manifest; broad cache
restore keys are forbidden. `ci/gates.toml` records this policy, not proof
that the future executor/artifact contract has been delivered.

### Remaining acceptance blockers

1. Wire `mergify ci queue-info` and GitHub PR API heads/bases into reconstruction
   and run F05 `plan --commit` on each synthetic prefix.
2. Execute all selected `ci/capabilities.toml` suites independently for every
   prefix, with runtime `--version` and candidate-check evidence.
3. Produce correctly versioned root-crate binaries without tracked source or
   dependency changes; bind exact tree/lock/toolchain/features/profile/version
   to cache and SHA-256 artifacts, and upload canonical evidence for F07.
4. Complete actual dispatch and workflow integration fixtures proving these
   paths, including failed/missing child evidence and cache invalidation.

Until these items are implemented, `candidate-ready` must not be configured as
a required final merge condition.
