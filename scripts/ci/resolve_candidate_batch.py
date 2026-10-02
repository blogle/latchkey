#!/usr/bin/env python3
"""Resolve trusted queue/dispatch metadata into the canonical batch schema."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

SHA = re.compile(r"^[0-9a-f]{40}$")
MAX_BASE_STACK = 100


def resolve(metadata: dict, repo: str, requested_sha: str, dispatch: bool = False) -> dict:
    if not SHA.fullmatch(requested_sha):
        raise ValueError("requested candidate SHA must be a full commit SHA")
    if dispatch:
        prs = metadata["pull_requests"]
        base_sha, tree = metadata["base_sha"], metadata["requested_tree"]
        base_stack = metadata.get("base_stack", [])
        if set(metadata) not in ({"base_sha", "requested_tree", "pull_requests"},
                                 {"base_sha", "requested_tree", "pull_requests", "base_stack"}):
            raise ValueError("dispatch batch has unexpected fields")
        if not isinstance(prs, list):
            raise ValueError("dispatch pull_requests must be an ordered array")
    else:
        base_sha = metadata.get("checking_base_sha")
        advertised_base = base_sha
        if not isinstance(base_sha, str) or not SHA.fullmatch(base_sha):
            raise ValueError("queue metadata checking_base_sha must be a full commit SHA")
        queued = metadata.get("pull_requests")
        tree = metadata.get("requested_tree")
        if not tree:
            response = subprocess.run(["gh", "api", f"repos/{repo}/git/commits/{requested_sha}"],
                                      check=True, text=True, capture_output=True,
                                      env=os.environ.copy())
            tree = json.loads(response.stdout).get("tree", {}).get("sha")
        if not isinstance(queued, list) or not queued:
            raise ValueError("queue metadata has no ordered pull_requests")
        prs = []
        for row in queued:
            number = row if isinstance(row, int) else row.get("number") if isinstance(row, dict) else None
            if not isinstance(number, int) or number <= 0:
                raise ValueError("queue pull_requests must contain ordered PR numbers")
            response = subprocess.run(["gh", "api", f"repos/{repo}/pulls/{number}"],
                                      check=True, text=True, capture_output=True,
                                      env=os.environ.copy())
            pr = json.loads(response.stdout)
            prs.append({"number": number, "head_sha": pr.get("head", {}).get("sha"),
                        "base_sha": pr.get("base", {}).get("sha")})
        # Mergify's checking base can itself be a stack of synthetic merge
        # commits. Resolve each merge through GitHub's commit-to-PR association
        # API; commit subjects/branch names are not provenance.
        base_stack = []
        cursor = base_sha
        base_tree = None
        while True:
            response = subprocess.run(["gh", "api", f"repos/{repo}/git/commits/{cursor}"],
                                      check=True, text=True, capture_output=True,
                                      env=os.environ.copy())
            commit = json.loads(response.stdout)
            if cursor == advertised_base:
                base_tree = commit.get("tree", {}).get("sha")
            parents = [row.get("sha") for row in commit.get("parents", [])]
            if len(parents) <= 1:
                base_sha = cursor
                break
            if len(parents) != 2 or not all(SHA.fullmatch(str(parent)) for parent in parents):
                raise ValueError("queue checking base has an unverifiable merge ancestry")
            response = subprocess.run(["gh", "api", f"repos/{repo}/commits/{cursor}/pulls"],
                                      check=True, text=True, capture_output=True,
                                      env=os.environ.copy())
            associated = json.loads(response.stdout)
            matches = [pr for pr in associated if pr.get("head", {}).get("sha") == parents[1]]
            if len(matches) != 1:
                raise ValueError("queue base merge is not uniquely associated with its second-parent PR head")
            pr = matches[0]
            number = pr.get("number")
            api_base, head = pr.get("base", {}).get("sha"), pr.get("head", {}).get("sha")
            if not isinstance(number, int) or not SHA.fullmatch(str(api_base)) or head != parents[1]:
                raise ValueError("queue base PR association lacks authoritative base/head data")
            base_stack.append({"number": number, "head_sha": head, "base_sha": api_base})
            if len(base_stack) > MAX_BASE_STACK:
                raise ValueError("queue checking-base stack exceeds the 100-PR safety limit")
            cursor = parents[0]
        base_stack.reverse()
    if not SHA.fullmatch(str(base_sha)) or not SHA.fullmatch(str(tree)):
        raise ValueError("batch exact base/tree must be full SHA-1 identifiers")
    if not 1 <= len(prs) <= 3:
        raise ValueError("batch requires one to three ordered PRs")
    seen: set[int] = set()
    clean = []
    for pr in prs:
        if (not isinstance(pr, dict) or set(pr) != {"number", "head_sha", "base_sha"}
                or not isinstance(pr["number"], int) or pr["number"] <= 0
                or pr["number"] in seen
                or any(not isinstance(pr.get(k), str) or not SHA.fullmatch(pr[k]) for k in ("head_sha", "base_sha"))):
            raise ValueError("invalid ordered PR record")
        seen.add(pr["number"])
        clean.append(pr)
    clean_stack = []
    stack_seen: set[int] = set()
    for pr in base_stack:
        if (not isinstance(pr, dict) or set(pr) != {"number", "head_sha", "base_sha"}
                or not isinstance(pr["number"], int) or pr["number"] <= 0
                or pr["number"] in seen or pr["number"] in stack_seen
                or any(not isinstance(pr.get(k), str) or not SHA.fullmatch(pr[k]) for k in ("head_sha", "base_sha"))):
            raise ValueError("invalid or duplicate authoritative preceding queue PR")
        stack_seen.add(pr["number"])
        clean_stack.append(pr)
    result = {"schema": "latchkey-candidate-batch/v1", "base_sha": base_sha,
            "base_stack": clean_stack,
            "requested_tree": tree, "pull_requests": clean}
    if not dispatch:
        if not SHA.fullmatch(str(base_tree)):
            raise ValueError("queue checking base has no authoritative tree")
        result["base_tree"] = base_tree
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--metadata", required=True, help="queue-info JSON or exact dispatch batch JSON")
    parser.add_argument("--output", required=True)
    parser.add_argument("--requested-sha", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--dispatch", action="store_true")
    args = parser.parse_args()
    try:
        result = resolve(json.loads(Path(args.metadata).read_text()), args.repository,
                         args.requested_sha, args.dispatch)
        Path(args.output).write_text(json.dumps(result, sort_keys=True, separators=(",", ":")) + "\n")
    except (OSError, ValueError, KeyError, json.JSONDecodeError, subprocess.CalledProcessError) as exc:
        print(f"candidate batch resolution rejected: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
