#!/usr/bin/env python3
"""Candidate batch reconstruction and evidence helpers.

The workflow invokes this trusted-base module. Queue ordering is supplied by
Mergify's queue-info metadata (never inferred from a branch name); every PR
also carries its API-reported base/head so reconstruction is reproducible.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import argparse
import sys
import tempfile
import shutil
import tomllib
from pathlib import Path
from typing import Any

SHA = re.compile(r"^[0-9a-f]{40}$")
MAX_BATCH = 3


def digest(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def cache_key(tree: str, lock: str, toolchain: str, features: str, profile: str, version: str) -> str:
    dimensions = (tree, lock, toolchain, features, profile, version)
    if any(not value for value in dimensions):
        raise ValueError("every candidate cache dimension is required")
    return "candidate-" + digest("\0".join(dimensions).encode())


def target_cache_key(lock: str, toolchain: str, features: str, profile: str) -> str:
    """Shared prefix target identity; root version overlays do not split dependencies."""
    dimensions = (lock, toolchain, features, profile)
    if any(not value for value in dimensions):
        raise ValueError("every prefix target cache dimension is required")
    return "prefix-target-" + digest("\0".join(dimensions).encode())


def _feature_fingerprint(source: str) -> str:
    manifest = tomllib.loads((__import__("pathlib").Path(source) / "Cargo.toml").read_text())
    # Include declared dependency feature selections as well as named package
    # features, including target-specific/dev/build dependency declarations.
    inputs = {key: manifest.get(key, {}) for key in
              ("features", "dependencies", "dev-dependencies", "build-dependencies", "target", "workspace")}
    return digest(json.dumps(inputs, sort_keys=True,
                             separators=(",", ":")).encode())


def validate_batch(base: str, requested_tree: str, ordered_heads: list[str], prefixes: list[dict[str, Any]]) -> None:
    if not SHA.fullmatch(base) or not SHA.fullmatch(requested_tree):
        raise ValueError("base and candidate tree must be full SHA-1 identifiers")
    if not 1 <= len(ordered_heads) <= MAX_BATCH or any(not SHA.fullmatch(x) for x in ordered_heads):
        raise ValueError("batch requires one to three ordered full head SHAs")
    if len(set(ordered_heads)) != len(ordered_heads):
        raise ValueError("duplicate PR head in ordered batch")
    if len(prefixes) != len(ordered_heads):
        raise ValueError("missing prefix evidence")
    if [item.get("head_sha") for item in prefixes] != ordered_heads:
        raise ValueError("prefix order/head evidence differs from requested batch")
    if prefixes[-1].get("tree") != requested_tree:
        raise ValueError("rebuilt final prefix tree differs from Mergify candidate tree")
    for item in prefixes:
        if item.get("result") != "passed" or not item.get("version"):
            raise ValueError("every prefix must pass and carry its planned release version")
        if not item.get("suites"):
            raise ValueError("active suites must be explicit and non-empty")


def validate_child_checks(children: list[dict[str, Any]], expected: list[str]) -> None:
    by_name = {row.get("name"): row for row in children}
    if len(by_name) != len(children):
        raise ValueError("duplicate child check")
    for name in expected:
        row = by_name.get(name)
        if row is None:
            raise ValueError(f"missing child check: {name}")
        if row.get("status") in {"cancelled", "failure", "timed_out", "action_required"}:
            raise ValueError(f"child check {name} is {row['status']}")
        if row.get("status") != "completed" or row.get("conclusion") != "success":
            raise ValueError(f"child check {name} is not completed successfully")


def manifest(*, heads: list[str], base: str, tree: str, lock: str, toolchain: str,
             features: str, profile: str, version: str, suites: list[str],
             artifacts: dict[str, str], result: str) -> dict[str, Any]:
    if not heads or len(heads) > MAX_BATCH or not suites or result not in {"passed", "failed"}:
        raise ValueError("invalid candidate manifest inputs")
    if any(not re.fullmatch(r"[0-9a-f]{64}", value) for value in artifacts.values()):
        raise ValueError("artifact checksums must be SHA-256 hex digests")
    return {
        "schema": "latchkey-candidate-evidence/v1",
        "ordered_pr_heads": heads,
        "base_sha": base,
        "content_tree": tree,
        "fingerprints": {"cargo_lock": lock, "toolchain": toolchain, "features": features,
                         "profile": profile, "release_version": version},
        "active_test_suites": suites,
        "artifacts": artifacts,
        "result": result,
        "provenance": {"git_commit_metadata_in_content_derivation": False},
    }


def _run(command: list[str], *, cwd: str, env: dict[str, str] | None = None) -> bytes:
    proc = subprocess.run(command, cwd=cwd, env=env, capture_output=True)
    if proc.returncode:
        raise ValueError(f"candidate command failed ({proc.returncode}): {command[0]}")
    return proc.stdout


def _sha(repo: str, ref: str) -> str:
    return _commit(repo, ref)


def _capability_suites(trusted: str, source: str, commit: str) -> list[str]:
    """Use trusted mandatory foundation policy and reject candidate policy downgrades."""
    trusted_root = __import__("pathlib").Path(trusted)
    baseline = tomllib.loads((trusted_root / "ci/capabilities.toml").read_text())
    candidate_text = git(source, "show", f"{commit}:ci/capabilities.toml").decode()
    candidate = tomllib.loads(candidate_text)
    if baseline.get("schema_version") != candidate.get("schema_version"):
        raise ValueError("candidate capabilities schema differs from trusted baseline")
    if candidate.get("gates_are_cumulative") is not True:
        raise ValueError("candidate capabilities must declare cumulative gates")
    baseline_stages = {stage.get("id"): stage for stage in baseline.get("stages", [])}
    candidate_stages = {stage.get("id"): stage for stage in candidate.get("stages", [])}
    trusted_implemented_gates = {
        (gate.get("id"), gate.get("command"), gate.get("owner"))
        for stage in baseline.get("stages", []) for gate in stage.get("gates", [])
    }
    if len(candidate_stages) != len(candidate.get("stages", [])):
        raise ValueError("candidate capabilities contain duplicate stages")
    for stage_id, stage in baseline_stages.items():
        actual = candidate_stages.get(stage_id)
        if actual is None or actual.get("extends") != stage.get("extends"):
            raise ValueError(f"candidate capabilities removed/altered trusted stage {stage_id}")
        by_id = {gate.get("id"): gate for gate in actual.get("gates", [])}
        for gate in stage.get("gates", []):
            if by_id.get(gate.get("id")) != gate:
                raise ValueError(f"candidate capabilities removed/altered trusted gate {gate.get('id')}")
    all_stages = candidate.get("stages", [])
    known_gates: dict[str, dict[str, Any]] = {}
    for stage in all_stages:
        gates = stage.get("gates")
        if not isinstance(gates, list) or not gates:
            raise ValueError("every candidate capability stage needs gates")
        seen: dict[str, dict[str, Any]] = {}
        for gate in gates:
            if (not isinstance(gate, dict) or set(gate) != {"id", "command", "owner"}
                    or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", str(gate["id"]))
                    or not re.fullmatch(r"just [a-z0-9][a-z0-9-]*(?: [a-z0-9][a-z0-9-]*)*", str(gate["command"]))
                    or not re.fullmatch(r"[A-Z]+-[0-9]+", str(gate["owner"]))):
                raise ValueError("candidate capability gate is malformed or unowned")
            if (gate["id"], gate["command"], gate["owner"]) not in trusted_implemented_gates:
                raise ValueError("candidate capability addition is not owned and implemented by trusted policy")
            if gate["id"] in seen:
                raise ValueError("duplicate candidate capability gate")
            seen[gate["id"]] = gate
        parent = stage.get("extends")
        if parent:
            if parent not in known_gates or not all(seen.get(gid) == gate for gid, gate in known_gates[parent].items()):
                raise ValueError(f"candidate capability transition {stage.get('id')} is not cumulative")
        known_gates[stage.get("id")] = seen
    foundation = candidate_stages.get("foundation")
    if foundation is None:
        raise ValueError("candidate capabilities have no foundation stage")
    base_ids = {gate["id"] for gate in baseline_stages["foundation"]["gates"]}
    # Mandatory baseline commands always come from trusted master. Candidate
    # additions are explicit data and are accepted only when trusted policy
    # recognizes and implements their command semantics.
    return [g["command"] for g in baseline_stages["foundation"]["gates"]] + [
        g["command"] for g in foundation["gates"] if g["id"] not in base_ids
    ]


def _foundation_gates(trusted: str, source: str, commit: str) -> list[dict[str, str]]:
    """Return mandatory gate IDs selected by trusted policy, plus validated additions."""
    # _capability_suites performs the full candidate downgrade/ownership audit.
    required = _capability_suites(trusted, source, commit)
    trusted_policy = tomllib.loads((__import__("pathlib").Path(trusted) / "ci/capabilities.toml").read_text())
    candidate_policy = tomllib.loads(git(source, "show", f"{commit}:ci/capabilities.toml").decode())
    definitions = {gate["command"]: gate for gate in trusted_policy["stages"]
                   if gate["id"] == "foundation" for gate in gate["gates"]}
    definitions.update({gate["command"]: gate for gate in candidate_policy["stages"]
                        if gate["id"] == "foundation" for gate in gate["gates"]})
    gates = []
    for command in required:
        gate = definitions.get(command)
        if gate is None:
            raise ValueError(f"foundation command has no trusted gate mapping: {command}")
        gates.append({"id": gate["id"], "command": command})
    return gates


def _cached_tools(source: str) -> tuple[str, str]:
    """Read cached absolute tools without invoking any candidate dispatcher."""
    script = 'source "$1/.dev/env"; printf "%s\\n%s\\n" "$LATCHKEY_CARGO" "$LATCHKEY_PYTHON3"'
    values = _run(["bash", "-c", script, "candidate-env", source], cwd=source).decode().splitlines()
    if len(values) != 2 or any(not os.path.isabs(value) or not os.access(value, os.X_OK) for value in values):
        raise ValueError("candidate .dev/env does not contain usable cached Cargo/Python tools")
    return values[0], values[1]


def execute(args: argparse.Namespace) -> None:
    implementation_root = Path(__file__).resolve().parents[2]
    source = os.path.realpath(args.source)
    repository = os.path.realpath(args.repository)
    trusted = os.path.realpath(args.trusted)
    batch = json.loads(open(args.batch, encoding="utf-8").read())
    if set(batch) != {"schema", "base_sha", "requested_tree", "pull_requests"} or batch["schema"] != "latchkey-candidate-batch/v1":
        raise ValueError("batch must be canonical latchkey-candidate-batch/v1")
    prs = batch["pull_requests"]
    if not isinstance(prs, list) or not 1 <= len(prs) <= MAX_BATCH:
        raise ValueError("batch requires one to three ordered PR records")
    for pr in prs:
        if (not isinstance(pr, dict) or set(pr) != {"number", "head_sha", "base_sha"}
                or not isinstance(pr["number"], int) or pr["number"] <= 0
                or any(not isinstance(pr[k], str) or not SHA.fullmatch(pr[k]) for k in ("head_sha", "base_sha"))):
            raise ValueError("invalid PR record in canonical batch")
    if len({pr["number"] for pr in prs}) != len(prs) or len({pr["head_sha"] for pr in prs}) != len(prs):
        raise ValueError("duplicate PR number or head SHA in ordered batch")
    base, requested_tree = batch["base_sha"], batch["requested_tree"]
    if not SHA.fullmatch(base) or not SHA.fullmatch(requested_tree):
        raise ValueError("batch base/tree must be full SHA-1 identifiers")
    if _sha(repository, "HEAD") != args.requested_sha:
        raise ValueError("candidate checkout differs from exact requested SHA")
    if git(repository, "rev-parse", "HEAD^{tree}").decode().strip() != requested_tree:
        raise ValueError("requested Mergify candidate tree differs from batch tree")
    # Reconstruct using the candidate repository object database. The workflow
    # fetches exact PR heads into this checkout after trusted metadata resolution.
    prefixes = reconstruct_prefixes(repository, base, prs, requested_tree)
    lock_fingerprint = digest((__import__("pathlib").Path(source) / "Cargo.lock").read_bytes())
    toolchain_file = (__import__("pathlib").Path(source) / "rust-toolchain.toml").read_bytes()
    toolchain = digest(toolchain_file)
    features = _feature_fingerprint(source)
    profile = "ci"
    gates = _foundation_gates(trusted, repository, args.requested_sha)
    suites = [gate["command"] for gate in gates]
    # Only cached executable paths are taken from .dev/env. Foundation gates
    # below always use a prefix worktree as both CWD and source root.
    if not os.path.isfile(os.path.join(source, ".dev", "env")):
        raise ValueError("candidate .dev/env is missing; run trusted setup.sh for the candidate checkout")
    evidence_dir = __import__("pathlib").Path(args.evidence).resolve()
    artifacts_dir = evidence_dir / "artifacts"
    artifacts_dir.mkdir(parents=True, exist_ok=True)
    cargo, python = _cached_tools(source)
    # This explicit CI-owned directory is shared by all disposable prefix roots.
    target = os.path.join(os.path.dirname(source), "candidate-ci-target",
                          target_cache_key(lock_fingerprint, toolchain, features, profile))
    os.makedirs(target, exist_ok=True)
    all_prefixes: list[dict[str, Any]] = []
    all_passed = True
    for index, prefix in enumerate(prefixes, 1):
        worktree = tempfile.mkdtemp(prefix="latchkey-prefix-")
        try:
            git(repository, "worktree", "add", "--detach", worktree, prefix["commit"])
            plan_cmd = [sys.executable, os.path.join(trusted, "scripts/release.py"), "--root", source,
                        "--policy", os.path.join(trusted, "release-policy.toml"), "plan",
                        "--commit", prefix["commit"], "--json"]
            plan_data = json.loads(_run(plan_cmd, cwd=source))
            version = plan_data.get("version")
            if plan_data.get("tree") != prefix["tree"] or not re.fullmatch(r"\d+\.\d+\.\d+", str(version)):
                raise ValueError("release planner returned invalid prefix tree/version")
            _run([sys.executable, str(implementation_root / "scripts/ci/inject_root_version.py"), worktree, version], cwd=trusted)
            env = os.environ.copy()
            env.pop("GITHUB_TOKEN", None)
            env.pop("GH_TOKEN", None)
            env.pop("GITHUB_READ_TOKEN", None)
            # Registry availability is not part of the Crane-seeded target
            # cache. Let Cargo fetch the exact locked dependencies instead.
            env.pop("CARGO_NET_OFFLINE", None)
            env["CARGO_TARGET_DIR"] = target
            env["CARGO_PROFILE"] = profile
            env["LATCHKEY_PROFILE"] = profile
            prefix_logs = artifacts_dir / f"prefix-{index}-{version}-{prefix['tree'][:12]}-logs"
            gates_file = evidence_dir / f".prefix-{index}-gates.json"
            gates_file.write_text(json.dumps(gates), encoding="utf-8")
            runner = str(implementation_root / "scripts/ci/prefix_runner.py")
            run_cmd = [python, runner, "--root", worktree, "--target", target,
                       "--cargo", cargo, "--python", python, "--gates", str(gates_file),
                       "--logs", str(prefix_logs)]
            gate_proc = subprocess.run(run_cmd, cwd=worktree, env=env, capture_output=True)
            suite_results = json.loads((prefix_logs / "results.json").read_text()) if (prefix_logs / "results.json").is_file() else []
            if gate_proc.returncode:
                all_passed = False
                raise ValueError(f"foundation gates failed for prefix {index}; results={suite_results}; "
                                 f"runner={gate_proc.stdout!r}/{gate_proc.stderr!r}; logs: {prefix_logs}")
            binary = __import__("pathlib").Path(target) / profile / "latchkey"
            if not binary.is_file():
                raise ValueError("cargo build did not produce expected root binary")
            actual_version = _run([str(binary), "--version"], cwd=worktree).decode().strip()
            expected_version = f"latchkey {version}"
            if actual_version != expected_version:
                raise ValueError(f"versioned binary --version mismatch ({actual_version!r}, expected {expected_version!r})")
            _run([str(binary), "--help"], cwd=worktree)
            name = f"prefix-{index}-{version}-{prefix['tree'][:12]}"
            artifact_path = artifacts_dir / name
            shutil.copy2(binary, artifact_path)
            artifact_hash = digest(artifact_path.read_bytes())
            all_prefixes.append({"number": prs[index - 1]["number"], "head_sha": prefix["head_sha"],
                                 "synthetic_sha": prefix["commit"], "tree": prefix["tree"],
                                  "release_version": version,
                                  "suite_results": [{"command": row["command"], "result": row["result"]}
                                                    for row in suite_results],
                                 "artifact": {"filename": f"artifacts/{name}", "sha256": artifact_hash},
                                  "result": "passed" if all(r["result"] == "passed" for r in suite_results) else "failed"})
        except Exception:
            all_passed = False
            raise
        finally:
            subprocess.run(["git", "worktree", "remove", "--force", worktree], cwd=repository, capture_output=True)
    # One full final candidate gate, never repeated for each prefix.
    if all_passed:
        final_env = os.environ.copy()
        for key in ("GITHUB_TOKEN", "GH_TOKEN", "GITHUB_READ_TOKEN"):
            final_env.pop(key, None)
        final_env["LK_ROOT"] = source
        _run(["just", "--justfile", os.path.join(trusted, "justfile"),
              "--working-directory", source, "candidate-check"], cwd=source, env=final_env)
    content_material = "\0".join([requested_tree, lock_fingerprint, toolchain, features, profile,
                                    ",".join(p["release_version"] for p in all_prefixes)])
    artifact_name = "latchkey-candidate-" + digest(content_material.encode())[:32]
    result = "passed" if all_passed else "failed"
    document = {"schema": "latchkey-candidate-evidence/v1", "ordered_prs": all_prefixes,
                "base_sha": base, "candidate_sha": args.requested_sha, "candidate_tree": requested_tree,
                "fingerprints": {"cargo_lock_sha256": lock_fingerprint, "toolchain_sha256": toolchain,
                                 "profile": profile, "features_sha256": features},
                "artifact_name": artifact_name, "final_result": result,
                "provenance": {"workflow_run_id": os.environ.get("GITHUB_RUN_ID", "local"),
                               "git_commit_metadata_in_content_derivation": False}}
    if result == "passed":
        if len(all_prefixes) != len(prs) or any(p["result"] != "passed" for p in all_prefixes):
            raise ValueError("manifest prefix validation failed")
        manifest_path = evidence_dir / "manifest.json"
        validate_evidence(document, evidence_dir, prs, suites)
        if all_prefixes[-1]["tree"] != requested_tree:
            raise ValueError("manifest final prefix tree differs from requested candidate tree")
        manifest_path.write_bytes(canonical_json(document))
        sums = []
        for path in sorted(artifacts_dir.iterdir()):
            if path.is_file():
                sums.append(f"{digest(path.read_bytes())}  artifacts/{path.name}")
        (evidence_dir / "SHA256SUMS").write_text("\n".join(sums) + "\n", encoding="utf-8")
    else:
        raise ValueError("a candidate prefix failed; no successful evidence produced")


def print_cache_key(args: argparse.Namespace) -> None:
    source = os.path.realpath(args.source)
    trusted = os.path.realpath(args.trusted)
    batch = json.loads(open(args.batch, encoding="utf-8").read())
    if set(batch) != {"schema", "base_sha", "requested_tree", "pull_requests"} or batch["schema"] != "latchkey-candidate-batch/v1":
        raise ValueError("batch must be canonical latchkey-candidate-batch/v1")
    prefixes = reconstruct_prefixes(args.repository, batch["base_sha"], batch["pull_requests"], batch["requested_tree"])
    versions = []
    for prefix in prefixes:
        plan = json.loads(_run([sys.executable, os.path.join(trusted, "scripts/release.py"), "--root", source,
                               "--policy", os.path.join(trusted, "release-policy.toml"), "plan",
                               "--commit", prefix["commit"], "--json"], cwd=trusted))
        if plan.get("tree") != prefix["tree"] or not re.fullmatch(r"\d+\.\d+\.\d+", str(plan.get("version"))):
            raise ValueError("release planner returned invalid prefix tree/version")
        versions.append(plan["version"])
    lock = digest((__import__("pathlib").Path(source) / "Cargo.lock").read_bytes())
    toolchain = digest((__import__("pathlib").Path(source) / "rust-toolchain.toml").read_bytes())
    features = _feature_fingerprint(source)
    print(target_cache_key(lock, toolchain, features, "ci"))


def canonical_json(value: dict[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def validate_evidence(document: dict[str, Any], evidence_dir: Any,
                      pull_requests: list[dict[str, Any]], suites: list[str]) -> None:
    if document.get("schema") != "latchkey-candidate-evidence/v1" or document.get("final_result") != "passed":
        raise ValueError("evidence schema/final result is invalid")
    for key in ("base_sha", "candidate_sha", "candidate_tree"):
        if not SHA.fullmatch(document.get(key, "")):
            raise ValueError(f"evidence {key} is invalid")
    prefixes = document.get("ordered_prs")
    if not isinstance(prefixes, list) or len(prefixes) != len(pull_requests):
        raise ValueError("evidence prefix count differs from batch")
    for record, requested in zip(prefixes, pull_requests, strict=True):
        artifact = record.get("artifact", {})
        path = (evidence_dir / artifact.get("filename", "")).resolve()
        if (record.get("number") != requested["number"] or record.get("head_sha") != requested["head_sha"]
                or not SHA.fullmatch(record.get("synthetic_sha", ""))
                or not SHA.fullmatch(record.get("tree", ""))
                or not re.fullmatch(r"\d+\.\d+\.\d+", record.get("release_version", ""))
                or record.get("result") != "passed" or not record.get("suite_results")
                or [row.get("command") for row in record["suite_results"]] != suites
                or any(row.get("result") != "passed" for row in record["suite_results"])
                or not path.is_file() or path.parent != (evidence_dir / "artifacts").resolve()
                or not re.fullmatch(r"[0-9a-f]{64}", artifact.get("sha256", ""))
                or digest(path.read_bytes()) != artifact["sha256"]):
            raise ValueError("candidate evidence prefix/artifact validation failed")
    fingerprints = document.get("fingerprints", {})
    for key in ("cargo_lock_sha256", "toolchain_sha256", "features_sha256"):
        if not re.fullmatch(r"[0-9a-f]{64}", fingerprints.get(key, "")):
            raise ValueError("candidate evidence fingerprint is invalid")
    if fingerprints.get("profile") != "ci" or not document.get("artifact_name"):
        raise ValueError("candidate evidence profile/artifact name is missing")


def git(repo: str, *args: str, input: bytes | None = None) -> bytes:
    proc = subprocess.run(["git", *args], cwd=repo, input=input, capture_output=True)
    if proc.returncode:
        raise ValueError(f"git {args[0]} failed ({proc.returncode})")
    return proc.stdout


def _commit(repo: str, ref: str) -> str:
    value = git(repo, "rev-parse", "--verify", "--quiet", "--end-of-options", f"{ref}^{{commit}}").decode().strip()
    if not SHA.fullmatch(value):
        raise ValueError(f"invalid or unavailable commit: {ref}")
    return value


def _parents(repo: str, commit: str) -> list[str]:
    return git(repo, "rev-list", "--parents", "-n", "1", commit).decode().split()[1:]


def _fragment_delta(repo: str, parent: str, treeish: str) -> None:
    statuses = git(repo, "diff", "--name-status", "--no-renames", parent, treeish, "--", ".changes").decode().splitlines()
    fragments = [line for line in statuses if re.fullmatch(r"[A-Z]\t\.changes/[^/]+\.toml", line)]
    if len(fragments) != 1 or not fragments[0].startswith("A\t"):
        raise ValueError("each PR prefix must add exactly one new .changes/*.toml fragment")


def reconstruct_prefixes(repo: str, base: str, pull_requests: list[dict[str, Any]],
                         requested_tree: str | None = None) -> list[dict[str, str]]:
    """Apply each PR's exact base..head patch in authoritative queue order.

    Each returned SHA is a synthetic one-parent commit over the preceding
    prefix, suitable for F05 first-parent planning. Worktrees are temporary;
    tracked source is never modified.
    """
    base_commit = _commit(repo, base)
    if not 1 <= len(pull_requests) <= MAX_BATCH:
        raise ValueError("batch requires one to three ordered pull requests")
    prefixes: list[dict[str, str]] = []
    with tempfile.TemporaryDirectory(prefix="latchkey-candidate-") as tmp:
        work = os.path.join(tmp, "tree")
        git(repo, "worktree", "add", "--detach", work, base_commit)
        try:
            previous = base_commit
            for number, pr in enumerate(pull_requests, 1):
                pr_base = _commit(repo, str(pr.get("base_sha", "")))
                head = _commit(repo, str(pr.get("head_sha", "")))
                if len(_parents(repo, head)) > 1:
                    raise ValueError(f"PR {number} head is a merge commit")
                # The patch is explicitly bounded by the PR API base/head. A
                # missing/unknown base cannot be approximated safely.
                patch = git(repo, "diff", "--binary", "--no-ext-diff", pr_base, head, "--")
                if not patch:
                    raise ValueError(f"PR {number} has no reconstructable patch")
                _fragment_delta(repo, pr_base, head)
                applied = subprocess.run(["git", "apply", "--index", "--3way", "-"], cwd=work,
                                         input=patch, capture_output=True)
                if applied.returncode:
                    raise ValueError(f"PR {number} patch does not apply cleanly to its ordered prefix")
                tree = git(work, "write-tree").decode().strip()
                if not SHA.fullmatch(tree):
                    raise ValueError("git returned malformed prefix tree")
                # A PR's own patch is validated against its API base; compare
                # its fragment delta against the preceding reconstructed tree.
                _fragment_delta(repo, previous, tree)
                synthetic = git(repo, "-c", "user.name=Latchkey CI", "-c",
                                "user.email=ci@users.noreply.github.com", "commit-tree", tree,
                                "-p", previous, input=f"candidate prefix {number}\n".encode()).decode().strip()
                prefixes.append({"head_sha": head, "commit": synthetic, "tree": tree})
                previous = synthetic
                if number < len(pull_requests):
                    # Reset the temporary index/worktree to the synthetic tree
                    # without creating or moving any branch ref.
                    git(work, "read-tree", "--reset", "-u", tree)
            if requested_tree is not None and prefixes[-1]["tree"] != requested_tree:
                raise ValueError("final reconstructed tree differs from requested candidate tree")
        finally:
            subprocess.run(["git", "worktree", "remove", "--force", work], cwd=repo,
                           capture_output=True)
    return prefixes


def main() -> int:
    parser = argparse.ArgumentParser(description="reconstruct and validate merge-candidate evidence")
    subparsers = parser.add_subparsers(dest="command", required=True)
    run = subparsers.add_parser("run", help="run the candidate evidence pipeline")
    run.add_argument("--repository", required=True)
    run.add_argument("--trusted", required=True, help="trusted base checkout containing CI policy and scripts")
    run.add_argument("--source", required=True)
    run.add_argument("--batch", required=True)
    run.add_argument("--requested-sha", required=True)
    run.add_argument("--evidence", default="candidate-evidence")
    key = subparsers.add_parser("cache-key", help="derive the exact immutable candidate target cache key")
    key.add_argument("--repository", required=True)
    key.add_argument("--trusted", required=True, help="trusted base checkout containing CI policy and scripts")
    key.add_argument("--source", required=True)
    key.add_argument("--batch", required=True)
    args = parser.parse_args()
    try:
        if args.command == "run":
            execute(args)
        else:
            print_cache_key(args)
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError, subprocess.CalledProcessError) as exc:
        print(f"candidate execution failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
