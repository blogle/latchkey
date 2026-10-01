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


def resolve(metadata: dict, repo: str, requested_sha: str, dispatch: bool = False) -> dict:
    if not SHA.fullmatch(requested_sha):
        raise ValueError("requested candidate SHA must be a full commit SHA")
    if dispatch:
        if set(metadata) != {"base_sha", "requested_tree", "pull_requests"}:
            raise ValueError("dispatch batch must contain base_sha, requested_tree and pull_requests")
        prs = metadata["pull_requests"]
        base_sha, tree = metadata["base_sha"], metadata["requested_tree"]
        if not isinstance(prs, list):
            raise ValueError("dispatch pull_requests must be an ordered array")
    else:
        base_sha = metadata.get("checking_base_sha")
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
    return {"schema": "latchkey-candidate-batch/v1", "base_sha": base_sha,
            "requested_tree": tree, "pull_requests": clean}


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
