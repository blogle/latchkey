#!/usr/bin/env python3
"""Pure candidate evidence, cache identity, and child-check validation helpers.

The runner consumes explicit ordered heads; it deliberately does not guess
batch membership/order from Mergify branch names or other undocumented data.
"""

from __future__ import annotations

import hashlib
import json
import re
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
