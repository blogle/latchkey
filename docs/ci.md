# Continuous integration

CI follows Mergify's documented two-step model. `pr-fast` is the queue-entry
gate; `candidate-ready` is only the final-merge gate on temporary queue PRs.
The latter is deliberately not a prerequisite for queue admission.

## Ordinary pull requests

`.github/workflows/pr.yml` runs on PRs to `master`, cancels superseded runs,
checks out and verifies the exact same-repository head, and runs cached
`just pr-check`. It rejects merge commits since the merge base and delegates
the exactly-one-new-fragment/immutable-history checks to
`scripts/release.py validate-fragment`. It does not run E2E, OCI or repeated
Nix dependency builds.

## Merge candidates and evidence

`.github/workflows/candidate.yml` has the stable `candidate-ready` check and
runs for Mergify queue PRs (`mergify/merge-queue/`) or an explicitly
allow-listed `workflow_dispatch` from `master`. Set the repository variable
`LATCHKEY_CI_TRUSTED_DISPATCH_ACTORS` to exact comma-separated GitHub
usernames to enable manual dispatch. Every candidate checkout is validated
against the event's requested SHA and Mergify App author. Workflows have only
`contents: read`; there are no status-writing, publication or release tokens
available to candidate code.

Candidate batch data must contain ordered PR heads, exact base and Mergify
tree, and the independently reconstructed prefix trees. The helpers in
`scripts/ci/candidate.py` enforce a maximum of three prefixes, order and final
tree agreement, required per-prefix result/version/suites, child-check
completion, and SHA-256 artifact hashes. Cache identities include content
tree, Cargo.lock, toolchain, features, profile and F05 release version; Git
commit metadata is provenance only. Candidate manifest JSON is canonical and
contains no mutable branch-name cache fallback. Missing batch metadata or
evidence is failure; do not infer ordering from a queue branch name.

The suite list is stage-selected from `ci/capabilities.toml`; `ci/gates.toml`
records F04's additional cumulative prefix and artifact policies. Foundation
currently names only bootstrap binary validation.

## Known gate blocker

F05 `scripts/release.py plan` walks every first-parent commit from the release
anchor and requires exactly one fragment for every commit. The current real
history includes fragmentless `a014e4e` before this branch, so candidate
prefix version planning fails under the authoritative policy. F04 must not
invent a version or weaken that policy. Until an F05-supported interface or
history correction exists, release-correct prefix binaries and successful
candidate evidence cannot be claimed.
