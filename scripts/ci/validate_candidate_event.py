#!/usr/bin/env python3
"""Authenticate the candidate trigger using documented GitHub event fields."""

import json
import os
import re
import subprocess
import sys
import argparse
from pathlib import Path

SHA = re.compile(r"^[0-9a-f]{40}$")


def reject(message: str) -> None:
    raise ValueError(message)


def validate(event: dict, environ: dict[str, str], checkout_sha: str | None = None) -> None:
    repository = environ.get("GITHUB_REPOSITORY")
    if event.get("repository", {}).get("full_name") != repository:
        reject("event repository mismatch")
    requested = environ.get("REQUESTED_SHA", "")
    if not SHA.fullmatch(requested) or (checkout_sha is not None and checkout_sha != requested):
        reject("requested SHA is invalid or differs from checked-out commit")
    if environ.get("EVENT_NAME") == "pull_request":
        pr = event.get("pull_request")
        if not isinstance(pr, dict):
            reject("missing pull_request payload")
        head = pr.get("head", {})
        if not str(head.get("ref", "")).startswith("mergify/merge-queue/"):
            reject("candidate branch is not under the Mergify queue prefix")
        if head.get("repo", {}).get("full_name") != repository:
            reject("queue PR head repository mismatch")
        if pr.get("user", {}).get("login") != "mergify[bot]":
            reject("queue PR was not opened by the Mergify GitHub App")
        if head.get("sha") != requested:
            reject("requested SHA differs from queue PR head SHA")
        if pr.get("base", {}).get("ref") != "master":
            reject("queue PR target is not master")
        return
    if environ.get("EVENT_NAME") != "workflow_dispatch":
        reject("unsupported candidate event")
    if environ.get("EVENT_REF") != "refs/heads/master":
        reject("trusted dispatch must run from master")
    if event.get("inputs", {}).get("requested_sha") != requested:
        reject("dispatch requested SHA differs from validated SHA")
    inputs = event.get("inputs", {})
    try:
        batch = json.loads(inputs.get("batch_json", ""))
    except (TypeError, json.JSONDecodeError):
        reject("dispatch batch_json must be a JSON object")
    if not isinstance(batch, dict) or set(batch) != {"base_sha", "requested_tree", "pull_requests"}:
        reject("dispatch batch_json must contain exact base_sha, requested_tree and pull_requests fields")
    prs = batch.get("pull_requests")
    if not isinstance(prs, list) or not 1 <= len(prs) <= 3:
        reject("dispatch requires one to three ordered PR records")
    numbers = []
    for pr in prs:
        if (not isinstance(pr, dict) or set(pr) != {"number", "head_sha", "base_sha"}
                or not isinstance(pr["number"], int) or pr["number"] <= 0
                or any(not isinstance(pr.get(key), str) or not SHA.fullmatch(pr[key]) for key in ("head_sha", "base_sha"))):
            reject("dispatch contains an invalid exact PR record")
        numbers.append(pr["number"])
    if len(set(numbers)) != len(numbers) or any(not SHA.fullmatch(batch.get(key, "")) for key in ("base_sha", "requested_tree")):
        reject("dispatch requires unique PR numbers and exact full base/tree SHAs")
    # A repository variable contains exact trusted GitHub usernames; a blank
    # or malformed allow-list intentionally disables manual dispatch.
    trusted = {actor.strip() for actor in environ.get("TRUSTED_DISPATCH_ACTORS", "").split(",") if actor.strip()}
    if not trusted or environ.get("GITHUB_ACTOR") not in trusted:
        reject("dispatch actor is not in LATCHKEY_CI_TRUSTED_DISPATCH_ACTORS")


def main() -> int:
    try:
        parser = argparse.ArgumentParser()
        parser.add_argument("--event-only", action="store_true")
        args = parser.parse_args()
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
        checkout = None if args.event_only else subprocess.run(
            ["git", "rev-parse", "HEAD"], check=True, text=True, capture_output=True
        ).stdout.strip()
        validate(event, os.environ, checkout)
    except (KeyError, OSError, json.JSONDecodeError, subprocess.CalledProcessError, ValueError) as exc:
        print(f"candidate provenance rejected: {exc}", file=sys.stderr)
        return 1
    print("candidate provenance verified; no status or publication token is available")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
