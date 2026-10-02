#!/usr/bin/env python3
"""Snapshot, plan, and explicitly apply the GitHub repository merge policy."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

RULESET_NAME = "Latchkey master merge policy"
MERGIFY_APP_ID = 10562
MERGIFY_CHECK = "Mergify Merge Queue"
DESIRED_REPO = {
    "allow_squash_merge": True,
    "allow_merge_commit": False,
    "allow_rebase_merge": False,
    "allow_auto_merge": False,
    "delete_branch_on_merge": False,
}
DESIRED_RULESET = {
    "name": RULESET_NAME,
    "target": "branch",
    "enforcement": "active",
    "bypass_actors": [],
    "conditions": {"ref_name": {"include": ["refs/heads/master"], "exclude": []}},
    "rules": [
        {"type": "pull_request", "parameters": {
            "allowed_merge_methods": ["squash"],
            "dismiss_stale_reviews_on_push": False,
            "require_code_owner_review": False,
            "require_last_push_approval": False,
            "required_approving_review_count": 0,
            "required_review_thread_resolution": False,
        }},
        {"type": "required_linear_history"},
        {"type": "non_fast_forward"},
        {"type": "deletion"},
        {"type": "required_status_checks", "parameters": {
            "do_not_enforce_on_create": False,
            "required_status_checks": [{"context": MERGIFY_CHECK, "integration_id": MERGIFY_APP_ID}],
            "strict_required_status_checks_policy": False,
        }},
    ],
}


class Gh:
    """Small gh CLI adapter; gh owns authentication and never exposes headers here."""

    def request(self, method: str, endpoint: str, data: dict | None = None) -> Any:
        command = ["gh", "api", "--method", method, endpoint]
        if data is not None:
            command += ["--input", "-"]
        result = subprocess.run(
            command, input=json.dumps(data) if data is not None else None,
            text=True, capture_output=True,
        )
        if result.returncode:
            # gh's diagnostic can contain transport details. Do not echo it.
            if method == "GET" and endpoint.endswith("/protection") and "HTTP 404" in result.stderr:
                return None
            raise RuntimeError(f"GitHub API request failed ({method} {endpoint}, exit {result.returncode})")
        return json.loads(result.stdout) if result.stdout.strip() else None


def endpoint(repo: str, suffix: str) -> str:
    return f"repos/{repo}" + (f"/{suffix}" if suffix else "")


def take_snapshot(api: Gh, repo: str) -> dict:
    info = api.request("GET", endpoint(repo, ""))
    protection = api.request("GET", endpoint(repo, "branches/master/protection"))
    rulesets = api.request("GET", endpoint(repo, "rulesets?includes_parents=false&per_page=100"))
    rulesets = [
        api.request("GET", endpoint(repo, f"rulesets/{row['id']}"))
        if row.get("name") == RULESET_NAME and row.get("id") is not None else row
        for row in (rulesets or [])
    ]
    return {
        "schema": "latchkey-repository-policy-snapshot/v1",
        "repo": repo,
        "default_branch": info["default_branch"],
        "settings": {key: info[key] for key in DESIRED_REPO},
        "branch_protection": protection,
        "rulesets": sorted(rulesets or [], key=lambda row: (row.get("name", ""), row.get("id", 0))),
    }


def canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def diff(snapshot: dict, current: dict) -> dict:
    return {
        "repo": current["repo"],
        "settings": {key: {"before": current["settings"][key], "after": value}
                     for key, value in DESIRED_REPO.items() if current["settings"][key] != value},
        "branch_protection": {"before": current["branch_protection"], "after": None},
        "ruleset": {
            "name": RULESET_NAME,
            "operation": "unchanged" if any(r.get("name") == RULESET_NAME and
                canonical({k: r.get(k) for k in DESIRED_RULESET}) == canonical(DESIRED_RULESET)
                for r in current["rulesets"]) else "create-or-update",
            "desired": DESIRED_RULESET,
        },
        "baseline_snapshot": snapshot.get("repo"),
    }


def read_snapshot(path: str) -> dict:
    value = json.loads(Path(path).read_text(encoding="utf-8"))
    if value.get("schema") != "latchkey-repository-policy-snapshot/v1":
        raise ValueError("unsupported snapshot schema")
    return value


def normalized_ruleset(row: dict) -> dict:
    normalized = {key: row.get(key) for key in DESIRED_RULESET if key != "rules"}
    expected_rules = {rule["type"]: rule for rule in DESIRED_RULESET["rules"]}
    observed_rules = {rule["type"]: rule for rule in row.get("rules", [])}
    normalized["rules"] = []
    for rule_type, expected in expected_rules.items():
        observed = observed_rules.get(rule_type, {})
        item = {"type": rule_type}
        if "parameters" in expected:
            parameters = observed.get("parameters", {})
            item["parameters"] = {
                key: parameters.get(key) for key in expected["parameters"]
            }
        normalized["rules"].append(item)
    if set(observed_rules) != set(expected_rules):
        normalized["rules"].append({"unexpected_types": sorted(set(observed_rules) - set(expected_rules))})
    return normalized


def ensure_snapshot_current(snapshot: dict, current: dict) -> None:
    """Allow only changes already made by this policy; reject all other drift."""
    expected = json.loads(canonical(snapshot))
    if expected.get("repo") != current["repo"]:
        raise ValueError("snapshot repository does not match requested repository")
    for key, desired in DESIRED_REPO.items():
        observed = current["settings"].get(key)
        if observed != expected["settings"].get(key):
            if observed != desired:
                raise ValueError("repository changed since snapshot; take a fresh snapshot before applying")
            expected["settings"][key] = observed
    if expected["default_branch"] != current["default_branch"]:
        raise ValueError("repository changed since snapshot; take a fresh snapshot before applying")
    if canonical(expected["branch_protection"]) != canonical(current["branch_protection"]):
        raise ValueError("repository changed since snapshot; take a fresh snapshot before applying")

    old_owned = [row for row in expected["rulesets"] if row.get("name") == RULESET_NAME]
    new_owned = [row for row in current["rulesets"] if row.get("name") == RULESET_NAME]
    old_other = [row for row in expected["rulesets"] if row.get("name") != RULESET_NAME]
    new_other = [row for row in current["rulesets"] if row.get("name") != RULESET_NAME]
    if canonical(old_other) != canonical(new_other):
        raise ValueError("repository changed since snapshot; take a fresh snapshot before applying")
    if old_owned != new_owned:
        if len(new_owned) != 1 or canonical(normalized_ruleset(new_owned[0])) != canonical(DESIRED_RULESET):
            raise ValueError("repository changed since snapshot; take a fresh snapshot before applying")


def apply_policy(api: Gh, repo: str, snapshot: dict) -> None:
    current = take_snapshot(api, repo)
    ensure_snapshot_current(snapshot, current)
    permission = api.request("GET", endpoint(repo, "")).get("permissions", {})
    if not permission.get("admin"):
        raise ValueError("authenticated GitHub account does not have repository admin permission")
    changed_settings = {key: value for key, value in DESIRED_REPO.items()
                        if current["settings"].get(key) != value}
    if changed_settings:
        api.request("PATCH", endpoint(repo, ""), changed_settings)
    matches = [r for r in current["rulesets"] if r.get("name") == RULESET_NAME]
    if len(matches) > 1:
        raise ValueError("multiple policy rulesets found; refusing ambiguous update")
    if matches:
        if canonical(normalized_ruleset(matches[0])) != canonical(DESIRED_RULESET):
            api.request("PUT", endpoint(repo, f"rulesets/{matches[0]['id']}"), DESIRED_RULESET)
    else:
        api.request("POST", endpoint(repo, "rulesets"), DESIRED_RULESET)


def verify(api: Gh, repo: str) -> None:
    state = take_snapshot(api, repo)
    if state["default_branch"] != "master":
        raise ValueError("default branch is not master")
    if state["settings"] != DESIRED_REPO:
        raise ValueError("repository merge settings do not match squash-only policy")
    if state["branch_protection"] is not None:
        raise ValueError("legacy branch protection is active; expected ruleset-only master protection")
    matches = [r for r in state["rulesets"] if r.get("name") == RULESET_NAME]
    if len(matches) != 1 or canonical(normalized_ruleset(matches[0])) != canonical(DESIRED_RULESET):
        raise ValueError("active master ruleset does not match the required policy")
    print("repository policy verified")


def verify_ancestry(before: str, after: str, pull: int, repo: str, api: Gh) -> None:
    pr = api.request("GET", endpoint(repo, f"pulls/{pull}"))
    title = pr.get("title", "")
    issue_id = re.search(r"\bLATCH-\d+\b", title)
    if not pr.get("merged") or pr.get("merge_commit_sha") != after or not issue_id:
        raise ValueError("merged PR metadata does not match requested squash commit or issue title")
    try:
        parents = subprocess.run(["git", "show", "-s", "--format=%P", after], check=True,
                                 text=True, capture_output=True).stdout.strip().split()
        message = subprocess.run(["git", "show", "-s", "--format=%B", after], check=True,
                                 text=True, capture_output=True).stdout
        first_parent = subprocess.run(["git", "rev-parse", f"{after}^1"], check=True,
                                      text=True, capture_output=True).stdout.strip()
    except subprocess.CalledProcessError as exc:
        raise ValueError("could not inspect local merge commit ancestry") from exc
    if len(parents) != 1 or first_parent != before:
        raise ValueError("merged commit is not a one-parent child of expected master")
    if title not in message or f"#{pull}" not in message or issue_id.group(0) not in message:
        raise ValueError("squash commit message is missing PR title, number, or LATCH issue ID")
    print("merged PR ancestry and squash message verified")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    snap = commands.add_parser("snapshot")
    snap.add_argument("--repo", required=True)
    snap.add_argument("--output", required=True)
    plan = commands.add_parser("plan")
    plan.add_argument("--snapshot", required=True)
    plan.add_argument("--repo")
    apply = commands.add_parser("apply")
    apply.add_argument("--snapshot", required=True)
    apply.add_argument("--repo", required=True)
    apply.add_argument("--confirm", action="store_true")
    check = commands.add_parser("verify")
    check.add_argument("--repo", required=True)
    ancestry = commands.add_parser("verify-ancestry")
    ancestry.add_argument("--before", required=True)
    ancestry.add_argument("--after", required=True)
    ancestry.add_argument("--pull", required=True, type=int)
    ancestry.add_argument("--repo", required=True)
    args = parser.parse_args(argv)
    api = Gh()
    try:
        if args.command == "snapshot":
            data = take_snapshot(api, args.repo)
            Path(args.output).write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
            print(f"repository settings snapshot written: {args.output}")
        elif args.command == "plan":
            baseline = read_snapshot(args.snapshot)
            repo = args.repo or baseline["repo"]
            current = take_snapshot(api, repo)
            print(json.dumps(diff(baseline, current), indent=2, sort_keys=True))
        elif args.command == "apply":
            if not args.confirm:
                raise ValueError("apply requires --confirm")
            apply_policy(api, args.repo, read_snapshot(args.snapshot))
            print("repository policy applied")
        elif args.command == "verify":
            verify(api, args.repo)
        else:
            verify_ancestry(args.before, args.after, args.pull, args.repo, api)
    except (OSError, json.JSONDecodeError, KeyError, ValueError, RuntimeError) as exc:
        print(f"repository policy error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
