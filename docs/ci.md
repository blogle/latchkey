# Continuous integration

## Ordinary pull requests

`.github/workflows/pr.yml` accepts only same-repository pull requests to
`master`. It checks out the PR base first and uses a small inline event
validator to check the same-repository source, `master` target, and exact
requested head SHA before checking out that head into a separate directory
(with persisted checkout credentials disabled). It rejects merge commits since merge
base, and runs the trusted F05 fragment validator with `--root` pointing at the
candidate checkout. After setting up the candidate's cached environment, the
trusted justfile and cached just tool run `pr-check` and the integration
`contracts` suite against the candidate checkout via `--working-directory` and
`LK_ROOT`. `pr-fast` is the stable queue-admission check and runs for ordinary
same-repository PR heads and Mergify queue heads. Queue heads skip only the
merge-base/one-new-fragment validation because a batch contains multiple valid
fragments; they still run `pr-check` and `contracts` against the combined
candidate tree. The candidate workflow independently validates each batch
prefix and remains the final `candidate-ready` gate.

## Candidate metadata and trust boundary

`.github/workflows/candidate.yml` exposes `candidate-ready`. It accepts a
Mergify queue PR only after inline event validation confirms the bot, queue
ref, master target, same repository and exact event SHA; manual dispatch is
restricted to the configured trusted actor on `master` and validates the exact
input batch before candidate source is checked out. This inline validator is
self-contained because F04 validator scripts do not exist in the base on the
first workflow run.

For queue runs, after exact SHA checkout and verification, the workflow runs
the official `mergify ci queue-info` from inside the candidate checkout. Its
ordered `pull_requests` and `checking_base_sha` determine queue
order/base; a separate read-only GitHub PR API call resolves each exact PR
number to its API `head.sha` and `base.sha`. The requested candidate tree is
resolved from the exact event commit. Dispatch supplies one explicit JSON
`batch_json` containing `base_sha`, `requested_tree`, and ordered
`pull_requests` records `{number, head_sha, base_sha}`; it does not infer order
or make unused inputs. Both paths validate and serialize the identical
`latchkey-candidate-batch/v1` schema. `Mergifyio/setup-cli@v1.3.0` is a
published official action ref. API credentials are present only for queue-info
and metadata resolution; those steps receive the read-only `GH_TOKEN` only as
needed. The resolver alone makes authenticated API lookups. Candidate
orchestration, build, and test steps receive no token.

## Prefix execution and evidence

The base checkout supplies trusted tools when present, including the F05
planner, root-version injector, and capability policy. On the first F04 queue
run, the base lacks the F04 resolver and candidate orchestrator, so after
inline validation and exact SHA verification those scripts may bootstrap from
the candidate tree. They run without write/publication credentials; the
resolver alone may receive the read-only API token for its API lookups. The
candidate checkout has
`persist-credentials: false`. Candidate build/test subprocesses explicitly drop
`GITHUB_TOKEN`, `GH_TOKEN`, and `GITHUB_READ_TOKEN`.

`candidate.py run` independently reconstructs each ordered base-to-head patch
with Git plumbing, rejects merge commits and anything other than one newly
added fragment, and writes synthetic one-parent commits. The trusted F05
`scripts/release.py --root <candidate> --policy <trusted release-policy.toml>
plan --commit ... --json` reads candidate commit objects/fragments while
preserving the trusted release policy. It overlays the planned version
with the trusted `inject_root_version.py` in a disposable worktree. The trusted
`prefix_runner.py` maps trusted capability gate IDs to their foundation
semantics and runs source-sensitive commands with that worktree as CWD: Cargo
formatting, clippy, unit/integration tests, and build use the absolute cached
Cargo executable; script-test discovery uses the prefix's `scripts/dev/tests`,
`scripts/ci/tests`, and discovered `tests/<suite>` roots. Test gates fail when
their suite reports zero tests. `pr-check` verifies its constituent fast gates
succeeded for that same prefix. Candidate policy cannot remove or redefine
trusted gates, and unsupported additions fail closed. Each versioned binary
must report the planned `--version` and support `--help`.
Trusted master `ci/capabilities.toml` gates are mandatory. Candidate capability
data is checked for preservation of every trusted stage/gate and cumulative
transitions; valid owned additions are dispatched only if the trusted prefix
runner implements them. No environment variable can replace suite policy. The
complete final `candidate-check` remains a single trusted just invocation
against the final candidate tree; prefix gates do not execute candidate
`scripts/dev` or `candidate.py` code. A prefix failure stops the batch and its
uniquely named gate logs remain attributed to that prefix.

The per-prefix build uses one explicit CI-owned `candidate-ci-target` shared
target, keyed by the candidate-level lockfile, toolchain, profile, and feature
identity (not the per-prefix planned root version). The root manifest and lock
entry receive the release-version overlay in each worktree. Cargo reuses
compatible dependencies and recompiles the root crate as its version changes;
this target is not an F03 `.dev/target`. Cargo target reuse is limited to
matching toolchain, target, profile, lock and feature settings. This does **not** mean Nix dependency derivations are reused:
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
