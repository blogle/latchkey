#!/usr/bin/env python3
"""Fail-closed validation of ordinary GitHub pull_request provenance."""

import json
import os
import subprocess
import sys
import argparse
from pathlib import Path


def reject(message: str) -> None:
    raise ValueError(message)


def validate(event: dict, repository: str, requested_sha: str, checkout_sha: str | None = None) -> None:
    if event.get("action") not in {"opened", "synchronize", "reopened", "ready_for_review", "edited"}:
        reject("unsupported pull_request action")
    if event.get("repository", {}).get("full_name") != repository:
        reject("event repository does not match workflow repository")
    pr = event.get("pull_request")
    if not isinstance(pr, dict):
        reject("missing pull_request metadata")
    head = pr.get("head")
    if not isinstance(head, dict) or not isinstance(head.get("repo"), dict):
        reject("missing PR head repository metadata")
    if head["repo"].get("full_name") != repository:
        reject("PR head must be from this repository")
    if not requested_sha or requested_sha != head.get("sha"):
        reject("requested SHA does not match PR head SHA")
    if checkout_sha is not None and checkout_sha != requested_sha:
        reject("checked-out SHA does not match validated PR head SHA")
    if pr.get("base", {}).get("ref") != "master":
        reject("PR target is not master")


def main() -> int:
    try:
        parser = argparse.ArgumentParser()
        parser.add_argument("--event-only", action="store_true")
        args = parser.parse_args()
        event_path = Path(os.environ["GITHUB_EVENT_PATH"])
        event = json.loads(event_path.read_text(encoding="utf-8"))
        checkout = None if args.event_only else subprocess.run(
            ["git", "rev-parse", "HEAD"], check=True, text=True, capture_output=True
        ).stdout.strip()
        validate(event, os.environ["GITHUB_REPOSITORY"], os.environ["REQUESTED_SHA"], checkout)
    except (KeyError, OSError, json.JSONDecodeError, subprocess.CalledProcessError, ValueError) as exc:
        print(f"pr provenance rejected: {exc}", file=sys.stderr)
        return 1
    print("PR provenance verified: same-repository PR and exact head SHA")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
