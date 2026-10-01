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


def canonical_json(value: dict[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


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
    run.add_argument("--source", required=True)
    run.parse_args()
    print("candidate pipeline unavailable: ordered PR API resolution, per-prefix F05 planning, "
          "version-injected builds, suite execution, manifest and artifact upload are not implemented",
          file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
