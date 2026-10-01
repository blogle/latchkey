#!/usr/bin/env python3
"""Deterministic Dojo-style changelog and patch-release planning (LATCH-4, F05).

Release metadata is a *pure function of committed history*: every version is
derived by folding the first-parent chain from `--commit` back to the anchor
recorded in `release-policy.toml` (patch by default; minor/major only when a
fragment says so explicitly). Mutable tag state never influences version
derivation, and no subcommand here writes to the worktree — `changelog` writes
only the `--output PATH` the caller names, and refuses tracked files, because
generated release artifacts are never committed to master.

Concepts follow the pinned Dojo2 release helper (patch default, explicit
minor/major directive, deterministic generated sections, exact tag-target
checks, idempotent publication planning) without its /merge bot, per-command
`nix develop`, or shared pre-merge version edits. Publication itself is owned
by LATCH-7 and is deliberately absent here.

Canonical invocations (Python stdlib only; run with the cached python3):

    scripts/release.py validate-fragment [--base REF]
    scripts/release.py plan --commit SHA [--json]
    scripts/release.py changelog --commit SHA --output PATH
    scripts/release.py verify-tag --tag TAG --commit SHA

Global options (before the subcommand):

    --root DIR      repository root (default: this checkout)
    --policy PATH   release policy file (default: <root>/release-policy.toml)

Exit status: 0 on success, 1 on any policy/validation rejection (message on
stderr, prefixed `latchkey: error:`), 2 for CLI usage errors (argparse).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tomllib
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_POLICY_NAME = "release-policy.toml"
PLAN_SCHEMA = "latchkey-release-plan/v1"

# Category rank (also the deterministic bullet order inside a rendered
# section); the set of accepted fragment categories.
CATEGORIES: tuple[str, ...] = ("Breaking", "Added", "Changed", "Fixed")
BUMPS: tuple[str, ...] = ("patch", "minor", "major")
FRAGMENT_KEYS = frozenset({"category", "summary", "bump", "issue"})

# Issue-id filenames: project key, dash, number (LATCH-4, TCK-1, ...).
FRAGMENT_FILENAME_RE = re.compile(r"^[A-Za-z][A-Za-z0-9]*-[0-9]+\.toml$")
# How fragments appear in `git diff --name-status` output (top level only;
# every other file under .changes/ — e.g. this README — is out of contract).
FRAGMENT_DIFF_RE = re.compile(r"^\.changes/[^/]+\.toml$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
BOOTSTRAP_COMMITS: tuple[str, ...] = (
    "a014e4e338842fac9927ce1a007c5581b0605ae2",
    "8c997bc05ad1caffdd2e6a6688826649d230432d",
    "846e01f0f90962bd67c5a4fe8c4935f9a8b9f173",
)
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")
TAG_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")

# Documented simple secret heuristic (.changes/README.md): a deterministic
# scan of the whole fragment file for well-known credential shapes. This is a
# tripwire, not a substitute for review; matches are reported by label only,
# never by content.
SECRET_PATTERNS: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"), "private key block"),
    (re.compile(r"\bAKIA[0-9A-Z]{16}\b"), "AWS access key id"),
    (re.compile(r"\bghp_[A-Za-z0-9]{36}\b"), "GitHub token"),
    (re.compile(r"\bgithub_pat_[A-Za-z0-9_]{8,}\b"), "GitHub fine-grained token"),
    (re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"), "Slack token"),
    (
        re.compile(
            r"(?i)\b(?:password|passwd|secret|api[_-]?key|token|credential)s?"
            r"\b\s*[:=]\s*\S"
        ),
        "credential-looking assignment",
    ),
)


class ReleaseError(Exception):
    """A deterministic, user-facing release policy violation."""


# ---- policy -----------------------------------------------------------------


@dataclass(frozen=True)
class Policy:
    anchor: str
    starting_version: str
    non_releasable_bootstrap: tuple[str, ...] = ()


def load_policy(path: Path) -> Policy:
    if not path.is_file():
        raise ReleaseError(f"release policy not found: {path}")
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        raise ReleaseError(f"cannot read release policy {path}: {exc}") from None
    if not isinstance(data, dict) or set(data) != {"history"}:
        raise ReleaseError(
            "release policy: expected exactly one top-level table [history]"
        )
    history = data["history"]
    if not isinstance(history, dict) or set(history) not in (
        {"anchor", "starting_version"},
        {"anchor", "starting_version", "non_releasable_bootstrap"},
    ):
        raise ReleaseError(
            "release policy: [history] must define exactly 'anchor', "
            "'starting_version', and optional 'non_releasable_bootstrap'"
        )
    anchor = history["anchor"]
    starting = history["starting_version"]
    if not isinstance(anchor, str) or not SHA_RE.fullmatch(anchor):
        raise ReleaseError(
            "release policy: 'anchor' must be a full 40-hex commit id"
        )
    if not isinstance(starting, str) or not VERSION_RE.fullmatch(starting):
        raise ReleaseError(
            "release policy: 'starting_version' must be MAJOR.MINOR.PATCH"
        )
    bootstrap = history.get("non_releasable_bootstrap", [])
    if (
        not isinstance(bootstrap, list)
        or not all(isinstance(sha, str) and SHA_RE.fullmatch(sha) for sha in bootstrap)
        or (bootstrap and tuple(bootstrap) != BOOTSTRAP_COMMITS)
    ):
        raise ReleaseError(
            "release policy: 'non_releasable_bootstrap' must contain exactly the "
            "three configured full immutable SHAs in first-parent order"
        )
    return Policy(
        anchor=anchor,
        starting_version=starting,
        non_releasable_bootstrap=tuple(bootstrap),
    )


# ---- versions ---------------------------------------------------------------


def parse_version(text: str) -> tuple[int, int, int]:
    if not VERSION_RE.fullmatch(text):
        raise ReleaseError(f"invalid version {text!r}: expected MAJOR.MINOR.PATCH")
    major, minor, patch = (int(part) for part in text.split("."))
    return major, minor, patch


def bump_version(version: tuple[int, int, int], bump: str) -> tuple[int, int, int]:
    if bump not in BUMPS:
        raise ReleaseError(f"invalid bump {bump!r}: expected one of {', '.join(BUMPS)}")
    major, minor, patch = version
    if bump == "major":
        return major + 1, 0, 0
    if bump == "minor":
        return major, minor + 1, 0
    return major, minor, patch + 1


def format_version(version: tuple[int, int, int]) -> str:
    return ".".join(str(part) for part in version)


def artifact_names(version: str) -> dict[str, str]:
    """Deterministic expected artifact names for a release version.

    Recorded in docs/releases.md; LATCH-7 publishes exactly these names.
    Immutability comes from the exact tag-target checks (verify-tag) plus the
    published checksums, not from embedding mutable state in the names.
    """
    tag = f"v{version}"
    return {
        "source_archive": f"latchkey-{tag}-src.tar.gz",
        "binary_archive": f"latchkey-{tag}-linux-x86_64-musl.tar.gz",
        "oci_ref": f"ghcr.io/blogle/latchkey:{tag}",
        "checksums": f"latchkey-{tag}-SHA256SUMS.txt",
        "notes": f"latchkey-{tag}-notes.md",
        "changelog": "CHANGELOG.md",
    }


def sha256_hex(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


# ---- fragments --------------------------------------------------------------


@dataclass(frozen=True)
class Fragment:
    path: str
    issue: str
    category: str
    summaries: tuple[str, ...]
    bump: str

    def to_json(self) -> dict:
        return {
            "path": self.path,
            "issue": self.issue,
            "category": self.category,
            "summary": list(self.summaries),
            "bump": self.bump,
        }


def scan_for_secrets(path: str, text: str) -> None:
    for pattern, label in SECRET_PATTERNS:
        if pattern.search(text):
            raise ReleaseError(
                f"fragment {path}: possible secret ({label}); never commit "
                "credentials (see .changes/README.md)"
            )


def parse_fragment(path: str, text: str) -> Fragment:
    name = path.rsplit("/", 1)[-1]
    if not FRAGMENT_FILENAME_RE.fullmatch(name):
        raise ReleaseError(
            f"fragment {path}: filename must be <ISSUE-ID>.toml "
            "(letters, digits and dashes, ending -<number>, e.g. LATCH-4.toml)"
        )
    stem = name[: -len(".toml")]
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        raise ReleaseError(f"fragment {path}: invalid TOML: {exc}") from None
    if not isinstance(data, dict):
        raise ReleaseError(f"fragment {path}: top level must be a TOML table")

    unknown = sorted(set(data) - FRAGMENT_KEYS)
    if unknown:
        raise ReleaseError(
            f"fragment {path}: unknown field(s): {', '.join(unknown)} "
            "(allowed: category, summary, bump, issue)"
        )
    if "category" not in data:
        raise ReleaseError(f"fragment {path}: missing required field 'category'")
    category = data["category"]
    if not isinstance(category, str) or category not in CATEGORIES:
        raise ReleaseError(
            f"fragment {path}: 'category' must be one of {', '.join(CATEGORIES)}"
        )
    if "summary" not in data:
        raise ReleaseError(f"fragment {path}: missing required field 'summary'")
    summary = data["summary"]
    if isinstance(summary, str):
        summaries: tuple[str, ...] = (summary,)
    elif isinstance(summary, list):
        if not summary:
            raise ReleaseError(f"fragment {path}: 'summary' must not be an empty array")
        if not all(isinstance(entry, str) for entry in summary):
            raise ReleaseError(
                f"fragment {path}: 'summary' array entries must all be strings"
            )
        summaries = tuple(summary)
    else:
        raise ReleaseError(
            f"fragment {path}: 'summary' must be a string or an array of strings"
        )
    if any(not entry.strip() for entry in summaries):
        raise ReleaseError(f"fragment {path}: 'summary' entries must be non-empty")

    bump = data.get("bump", "patch")
    if not isinstance(bump, str) or bump not in BUMPS:
        raise ReleaseError(
            f"fragment {path}: 'bump' must be one of {', '.join(BUMPS)} "
            "(patch is the default; minor/major must be explicit)"
        )
    issue = data.get("issue")
    if issue is not None and (not isinstance(issue, str) or issue != stem):
        raise ReleaseError(
            f"fragment {path}: 'issue' ({issue!r}) must match the filename stem "
            f"({stem!r}); the filename is authoritative"
        )

    scan_for_secrets(path, text)
    return Fragment(
        path=path,
        issue=stem,
        category=category,
        summaries=summaries,
        bump=bump,
    )


# ---- git plumbing -----------------------------------------------------------


def _git(root: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-c", "core.quotepath=false", *args],
        cwd=root,
        capture_output=True,
        text=True,
        errors="replace",
    )
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout).strip()
        raise ReleaseError(f"git {' '.join(args)} failed: {detail}")
    return proc.stdout


def _git_ok(root: Path, *args: str) -> str | None:
    """Run git, returning stripped stdout on success and None on failure."""
    proc = subprocess.run(
        ["git", "-c", "core.quotepath=false", *args],
        cwd=root,
        capture_output=True,
        text=True,
        errors="replace",
    )
    if proc.returncode != 0:
        return None
    return proc.stdout.strip()


def resolve_commit(root: Path, spec: str) -> str:
    out = _git_ok(
        root, "rev-parse", "--verify", "--quiet", "--end-of-options", f"{spec}^{{commit}}"
    )
    if out is None or not SHA_RE.fullmatch(out):
        raise ReleaseError(f"commit '{spec}' does not resolve to a commit in {root}")
    return out


def commit_parents(root: Path, commit: str) -> list[str]:
    out = _git(root, "rev-list", "--parents", "-n", "1", commit)
    parts = out.split()
    return parts[1:]


def commit_tree(root: Path, commit: str) -> str:
    out = _git(root, "rev-parse", "--verify", "--end-of-options", f"{commit}^{{tree}}")
    return out.strip()


def commit_subject(root: Path, commit: str) -> str:
    return _git(root, "log", "-1", "--format=%s", commit).strip()


def describe(root: Path, commit: str) -> str:
    """`<short> (<subject>)` for error messages; subject omitted when empty."""
    subject = commit_subject(root, commit)
    if subject:
        return f"{commit[:12]} ({subject})"
    return commit[:12]


def tree_fragments(root: Path, commit: str) -> list[str]:
    """Top-level `.changes/*.toml` paths present in a commit's tree."""
    out = _git(root, "ls-tree", "-r", "--name-only", commit, "--", ".changes")
    return [
        line for line in out.splitlines() if FRAGMENT_DIFF_RE.fullmatch(line)
    ]


def fragment_diff(root: Path, parent: str, commit: str) -> tuple[list[str], list[str]]:
    """`(added, changed)` fragment paths between two trees (no worktree use).

    `changed` covers every non-add status (modify/delete/type-change); the
    contract has no rename detection on purpose, so a rename reads as
    delete+add and is rejected as a deletion.
    """
    out = _git(
        root, "diff", "--name-status", "--no-renames", parent, commit, "--", ".changes"
    )
    added: list[str] = []
    changed: list[str] = []
    for line in out.splitlines():
        if not line.strip():
            continue
        status, _, path = line.partition("\t")
        if not FRAGMENT_DIFF_RE.fullmatch(path):
            continue
        status = status.strip()
        if status == "A":
            added.append(path)
        else:
            changed.append(f"{status} {path}")
    return added, changed


def load_commit_fragment(root: Path, commit: str, path: str) -> Fragment:
    """Read and validate a fragment *from the commit tree* (never the worktree)."""
    text = _git(root, "show", f"{commit}:{path}")
    return parse_fragment(path, text)


# ---- history traversal (structure phase) ------------------------------------


def first_parent_chain(root: Path, anchor: str, target: str) -> list[str]:
    """First-parent chain from `target` down to (excluding) `anchor`.

    Returns the chain oldest-first. Raises on a rewritten/missing anchor, an
    anchor that is not on the target's first-parent history, or any merge
    commit on the chain — release history is linear squash merges only.
    """
    if _git_ok(root, "cat-file", "-e", f"{anchor}^{{commit}}") is None:
        raise ReleaseError(
            f"release anchor {anchor} does not exist in this repository "
            "(history rewritten?)"
        )
    chain: list[str] = []
    current = target
    while current != anchor:
        if len(chain) > 1_000_000:
            raise ReleaseError(
                "release history walk exceeded 1000000 commits; refusing to plan"
            )
        parents = commit_parents(root, current)
        if len(parents) > 1:
            raise ReleaseError(
                f"commit {describe(root, current)} is a merge commit; release "
                "history must be linear first-parent squash merges"
            )
        if not parents:
            raise ReleaseError(
                f"release anchor {anchor} is not on the first-parent history of "
                f"{target[:12]} (anchor rewritten or commit from another line)"
            )
        chain.append(current)
        current = parents[0]
    chain.reverse()
    return chain


# ---- history folding (content phase) ----------------------------------------


@dataclass(frozen=True)
class CommitPlan:
    commit: str
    tree: str
    parent: str | None
    version: str
    tag: str
    fragment: Fragment | None
    notes_sha256: str

    def to_entry(self) -> dict:
        return {
            "commit": self.commit,
            "tree": self.tree,
            "parent": self.parent,
            "version": self.version,
            "tag": self.tag,
            "fragment": self.fragment.to_json() if self.fragment else None,
            "artifacts": artifact_names(self.version),
            "notes_sha256": self.notes_sha256,
        }


def render_notes(version: str, fragments: Sequence[Fragment]) -> str:
    """Dojo-style per-version notes: `## vX.Y.Z` heading plus bullets.

    Bullets are `- <summary> (<issue>)`, ordered by category rank
    (Breaking, Added, Changed, Fixed) then input order — a pure function of
    the fragments, so historical notes re-render byte-identically.
    """
    if not fragments:
        return ""
    lines = [f"## v{version}", ""]
    for category in CATEGORIES:
        for fragment in fragments:
            if fragment.category != category:
                continue
            for summary in fragment.summaries:
                lines.append(f"- {summary} ({fragment.issue})")
    return "\n".join(lines) + "\n"


def fold_chain(
    root: Path, policy: Policy, chain: Sequence[str]
) -> list[CommitPlan]:
    """Fold every chained commit into its plan (oldest-first).

    Per-commit check order (documented in docs/releases.md):
    1. previously merged fragments immutable (no modify/delete),
    2. exactly one new fragment (missing/ambiguous rejected),
    3. fragment schema, filename and secret heuristic,
    4. issue id not already used (case-insensitive reuse rejected).
    """
    seen_ids: set[str] = set()
    bootstrap_set = set(BOOTSTRAP_COMMITS)
    configured = set()
    # The optional empty list preserves isolated synthetic-history policies.
    # A real configured history may only omit future baseline commits when the
    # requested target itself is still before them. Once any releaseable
    # fragment is present, the complete fixed baseline must be in the chain.
    if any(fragment_diff(root, policy.anchor, commit)[0] for commit in chain):
        # Check membership/order against the actual first-parent chain before
        # folding. This fails closed if an expected pre-policy commit vanished.
        positions = [chain.index(sha) for sha in policy.non_releasable_bootstrap if sha in chain]
        if policy.non_releasable_bootstrap and (
            len(positions) != len(policy.non_releasable_bootstrap)
            or positions != sorted(positions)
        ):
            missing = [sha for sha in policy.non_releasable_bootstrap if sha not in chain]
            raise ReleaseError(
                "configured non-releasable bootstrap commit(s) missing from "
                "anchored first-parent history: " + ", ".join(missing)
            )
    configured.update(policy.non_releasable_bootstrap)
    if configured and configured != bootstrap_set:
        raise ReleaseError("release policy bootstrap commit list does not match fixed policy")
    for path in tree_fragments(root, policy.anchor):
        seen_ids.add(load_commit_fragment(root, policy.anchor, path).issue.casefold())

    version = parse_version(policy.starting_version)
    plans: list[CommitPlan] = []
    for index, commit in enumerate(chain):
        parent = chain[index - 1] if index > 0 else policy.anchor
        if commit in configured:
            continue
        added, changed = fragment_diff(root, parent, commit)
        if changed:
            raise ReleaseError(
                f"commit {describe(root, commit)} modifies previously merged "
                f"changelog fragment(s): {', '.join(changed)} "
                "(fragments are immutable once merged)"
            )
        if not added:
            raise ReleaseError(
                f"commit {describe(root, commit)} introduces no changelog "
                "fragment; every merged PR must add exactly one "
                ".changes/<issue-id>.toml (see .changes/README.md)"
            )
        if len(added) > 1:
            raise ReleaseError(
                f"commit {describe(root, commit)} introduces "
                f"{len(added)} changelog fragments ({', '.join(added)}); "
                "exactly one .changes/<issue-id>.toml per PR"
            )
        path = added[0]
        fragment = load_commit_fragment(root, commit, path)
        if fragment.issue.casefold() in seen_ids:
            raise ReleaseError(
                f"commit {describe(root, commit)} reuses changelog issue id "
                f"'{fragment.issue}' ({path}); each issue id may introduce "
                "exactly one fragment"
            )
        seen_ids.add(fragment.issue.casefold())

        version = bump_version(version, fragment.bump)
        formatted = format_version(version)
        notes = render_notes(formatted, (fragment,))
        plans.append(
            CommitPlan(
                commit=commit,
                tree=commit_tree(root, commit),
                parent=parent,
                version=formatted,
                tag=f"v{formatted}",
                fragment=fragment,
                notes_sha256=sha256_hex(notes),
            )
        )
    return plans


def build_plan(root: Path, policy: Policy, target_spec: str) -> dict:
    target = resolve_commit(root, target_spec)
    chain = first_parent_chain(root, policy.anchor, target)
    plans = fold_chain(root, policy, chain)

    if plans:
        target_plan = plans[-1]
        ancestors = plans[:-1]
    else:
        # Planning the anchor itself is valid: the starting version with
        # empty notes and no ancestors.
        parents = commit_parents(root, target)
        target_plan = CommitPlan(
            commit=target,
            tree=commit_tree(root, target),
            parent=parents[0] if parents else None,
            version=policy.starting_version,
            tag=f"v{policy.starting_version}",
            fragment=None,
            notes_sha256=sha256_hex(""),
        )
        ancestors = []

    document: dict = {
        "schema": PLAN_SCHEMA,
        "policy": {
            "anchor": policy.anchor,
            "starting_version": policy.starting_version,
            "non_releasable_bootstrap": list(policy.non_releasable_bootstrap),
        },
        "commit": target_plan.commit,
        "tree": target_plan.tree,
        "parent": target_plan.parent,
        "version": target_plan.version,
        "tag": target_plan.tag,
        "fragment": (
            target_plan.fragment.to_json() if target_plan.fragment else None
        ),
        "artifacts": artifact_names(target_plan.version),
        "notes_sha256": target_plan.notes_sha256,
        "unreleased_ancestors": [plan.to_entry() for plan in ancestors],
    }
    return document


def render_changelog(target: str, plans: Sequence[CommitPlan]) -> str:
    header = (
        "# Latchkey changelog\n"
        "\n"
        f"<!-- GENERATED by scripts/release.py changelog --commit {target}.\n"
        "     Release output artifact: this file is NEVER committed to master.\n"
        "     Root CHANGELOG.md is a pointer document; see docs/releases.md. -->\n"
    )
    if not plans:
        return header + "\nNo releases yet: the anchored history has no commits.\n"
    sections = []
    for plan in reversed(plans):
        fragments = (plan.fragment,) if plan.fragment is not None else ()
        sections.append(render_notes(plan.version, fragments))
    return header + "\n" + "\n".join(sections)


# ---- subcommands -----------------------------------------------------------


def cmd_validate_fragment(root: Path, base_spec: str) -> int:
    """PR gate: exactly one new valid fragment; merged fragments immutable."""
    base = resolve_commit(root, base_spec)
    base_paths = set(tree_fragments(root, base))
    workdir_changes = root / ".changes"
    worktree_paths = {
        f".changes/{entry.name}"
        for entry in sorted(workdir_changes.glob("*.toml"))
        if entry.is_file()
    }

    deleted = sorted(base_paths - worktree_paths)
    if deleted:
        raise ReleaseError(
            "previously merged changelog fragment(s) deleted: "
            f"{', '.join(deleted)} (fragments are immutable once merged)"
        )
    added = sorted(worktree_paths - base_paths)
    modified = []
    for path in sorted(base_paths & worktree_paths):
        base_text = _git(root, "show", f"{base}:{path}")
        worktree_text = (root / path).read_text(encoding="utf-8", errors="replace")
        if worktree_text != base_text:
            modified.append(path)
    if modified:
        raise ReleaseError(
            "previously merged changelog fragment(s) modified: "
            f"{', '.join(modified)} (fragments are immutable once merged)"
        )
    if not added:
        raise ReleaseError(
            f"no new changelog fragment vs {base_spec}; every PR adds exactly "
            "one .changes/<issue-id>.toml (docs-only PRs included — there is "
            "no release bypass; see .changes/README.md)"
        )
    if len(added) > 1:
        raise ReleaseError(
            f"{len(added)} new changelog fragments vs {base_spec} "
            f"({', '.join(added)}); exactly one .changes/<issue-id>.toml per PR"
        )

    fragments: dict[str, Fragment] = {}
    for path in sorted(worktree_paths):
        fragments[path] = parse_fragment(
            path, (root / path).read_text(encoding="utf-8", errors="replace")
        )
    seen: dict[str, str] = {}
    for path, fragment in fragments.items():
        folded = fragment.issue.casefold()
        if folded in seen:
            raise ReleaseError(
                f"duplicate issue id '{fragment.issue}': {seen[folded]} and "
                f"{path} (issue ids are compared case-insensitively)"
            )
        seen[folded] = path

    print(
        f"validate-fragment: ok: exactly 1 new fragment vs {base_spec} "
        f"({added[0]}); 0 modified, 0 deleted; "
        f"{len(fragments)} fragment(s) valid"
    )
    return 0


def cmd_plan(root: Path, policy: Policy, commit: str, as_json: bool) -> int:
    document = build_plan(root, policy, commit)
    if as_json:
        print(json.dumps(document, indent=2))
        return 0
    lines = [
        f"commit               {document['commit']}",
        f"tree                 {document['tree']}",
        f"parent               {document['parent'] or '-'}",
        f"version              {document['tag']}",
        f"fragment             {document['fragment']['path'] if document['fragment'] else '-'}",
        f"notes sha256         {document['notes_sha256']}",
    ]
    for name, value in document["artifacts"].items():
        lines.append(f"artifact {name:<14} {value}")
    ancestors = document["unreleased_ancestors"]
    if ancestors:
        listing = ", ".join(f"{a['tag']} {a['commit'][:12]}" for a in ancestors)
        lines.append(f"unreleased ancestors {len(ancestors)}: {listing}")
    else:
        lines.append("unreleased ancestors 0")
    print("\n".join(lines))
    return 0


def cmd_changelog(root: Path, policy: Policy, commit: str, output: Path) -> int:
    target = resolve_commit(root, commit)
    chain = first_parent_chain(root, policy.anchor, target)
    plans = fold_chain(root, policy, chain)
    text = render_changelog(target, plans)

    resolved = output.resolve()
    try:
        relative = resolved.relative_to(root.resolve())
    except ValueError:
        relative = None
    if relative is not None and _git_ok(
        root, "ls-files", "--error-unmatch", "--", str(relative)
    ) is not None:
        raise ReleaseError(
            f"refusing to write {relative}: it is a tracked file; generated "
            "release artifacts are never committed to master "
            "(see docs/releases.md)"
        )
    if resolved.parent.exists() and not resolved.parent.is_dir():
        raise ReleaseError(f"output parent is not a directory: {resolved.parent}")
    resolved.parent.mkdir(parents=True, exist_ok=True)
    resolved.write_text(text, encoding="utf-8")
    print(
        f"changelog: wrote {len(plans)} release section(s) for {target} "
        f"to {resolved}"
    )
    return 0


def cmd_verify_tag(root: Path, policy: Policy, tag: str, commit: str) -> int:
    """Exact tag-target check: the tag must point at exactly `commit`, and its
    name must equal that commit's planned version (`vMAJOR.MINOR.PATCH`)."""
    if not TAG_RE.fullmatch(tag):
        raise ReleaseError(
            f"invalid release tag {tag!r}: expected vX.Y.Z (MAJOR.MINOR.PATCH)"
        )
    target = resolve_commit(root, commit)
    tagged = _git_ok(
        root,
        "rev-parse",
        "--verify",
        "--quiet",
        "--end-of-options",
        f"refs/tags/{tag}^{{commit}}",
    )
    if tagged is None or not SHA_RE.fullmatch(tagged):
        raise ReleaseError(f"tag {tag} does not exist in this repository")
    if tagged != target:
        raise ReleaseError(
            f"tag {tag} points at {tagged}, expected exactly {target}"
        )
    document = build_plan(root, policy, target)
    if tag != document["tag"]:
        raise ReleaseError(
            f"tag {tag} does not match the planned version for "
            f"{target[:12]}: expected {document['tag']}"
        )
    print(f"verify-tag: ok: {tag} -> {target} (planned version {document['version']})")
    return 0


# ---- CLI --------------------------------------------------------------------


def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="scripts/release.py",
        description=(
            "Deterministic Dojo-style changelog and patch-release planning; "
            "release metadata is a pure function of linear merged history."
        ),
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=REPO_ROOT,
        help="repository root (default: this checkout)",
    )
    parser.add_argument(
        "--policy",
        type=Path,
        default=None,
        help=f"release policy file (default: <root>/{DEFAULT_POLICY_NAME})",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    fragment_parser = subparsers.add_parser(
        "validate-fragment",
        help="validate the PR's exactly-one-new changelog fragment against a base",
    )
    fragment_parser.add_argument(
        "--base",
        default="HEAD",
        help="base commit/branch to compare the working tree against (default: HEAD)",
    )

    plan_parser = subparsers.add_parser(
        "plan", help="plan the release for a commit from committed history"
    )
    plan_parser.add_argument("--commit", required=True, help="commit to plan")
    plan_parser.add_argument(
        "--json",
        action="store_true",
        help="emit the machine-readable plan JSON (canonical form)",
    )

    changelog_parser = subparsers.add_parser(
        "changelog", help="render cumulative Dojo-style release notes"
    )
    changelog_parser.add_argument("--commit", required=True, help="commit to render")
    changelog_parser.add_argument(
        "--output",
        required=True,
        type=Path,
        help="output PATH for the generated changelog artifact",
    )

    tag_parser = subparsers.add_parser(
        "verify-tag", help="check a release tag's exact target and planned version"
    )
    tag_parser.add_argument("--tag", required=True, help="tag name, e.g. v0.1.0")
    tag_parser.add_argument("--commit", required=True, help="commit the tag must point at")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_arg_parser().parse_args(argv)
    root = args.root.resolve()
    if not root.is_dir():
        print(f"latchkey: error: repository root does not exist: {root}", file=sys.stderr)
        return 1
    try:
        if args.command == "validate-fragment":
            return cmd_validate_fragment(root, args.base)
        policy_path = (
            args.policy.resolve() if args.policy else root / DEFAULT_POLICY_NAME
        )
        policy = load_policy(policy_path)
        if args.command == "plan":
            return cmd_plan(root, policy, args.commit, args.json)
        if args.command == "changelog":
            return cmd_changelog(root, policy, args.commit, args.output)
        if args.command == "verify-tag":
            return cmd_verify_tag(root, policy, args.tag, args.commit)
        raise AssertionError(f"unhandled command {args.command!r}")
    except ReleaseError as exc:
        print(f"latchkey: error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
