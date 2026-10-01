"""Fixture tests for scripts/release.py — deterministic release planning (LATCH-4).

Runs via `just script-test release_policy`, the suite resolved through the
generic multi-root discovery rule (tests/release_policy/). Every fixture
builds a synthetic git repository in a temp directory and drives the real CLI
subprocesses; the pure helpers are additionally exercised directly. Coverage
maps to the ticket matrix: empty history, 1/3 patch commits, minor/major
directives, malformed/missing/ambiguous fragment, duplicate issue id,
historical rewrite, wrong tag target, rerun idempotence, delayed publication,
and branch commits outside the planned first-parent line.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RELEASE_PY = REPO / "scripts" / "release.py"
LIB = REPO / "scripts" / "dev" / "lib.sh"
SCRIPT_TEST = REPO / "scripts" / "dev" / "script-test.sh"

# Isolated git for fixtures: ignore user/system config, never sign, never prompt.
GIT_ENV = {
    **os.environ,
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_SYSTEM": os.devnull,
    "GIT_TERMINAL_PROMPT": "0",
}


def _load_release_module():
    spec = importlib.util.spec_from_file_location("latchkey_release", RELEASE_PY)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    # Register before exec: dataclasses resolves annotations via sys.modules.
    sys.modules["latchkey_release"] = module
    spec.loader.exec_module(module)
    return module


release = _load_release_module()


def fragment_text(
    category: str = "Added",
    summary="Add a feature",
    bump: str | None = None,
    issue_field: str | None = None,
    extra: tuple[str, ...] = (),
) -> str:
    lines: list[str] = []
    if issue_field is not None:
        lines.append(f'issue = "{issue_field}"')
    lines.append(f'category = "{category}"')
    if isinstance(summary, (list, tuple)):
        lines.append(
            "summary = [" + ", ".join(f'"{entry}"' for entry in summary) + "]"
        )
    else:
        lines.append(f'summary = "{summary}"')
    if bump is not None:
        lines.append(f'bump = "{bump}"')
    lines.extend(extra)
    return "\n".join(lines) + "\n"


def policy_text(anchor: str, starting: str = "0.0.0") -> str:
    return (
        "[history]\n"
        f'anchor = "{anchor}"\n'
        f'starting_version = "{starting}"\n'
    )


def policy_with_bootstrap(anchor: str, bootstrap=None) -> str:
    values = release.BOOTSTRAP_COMMITS if bootstrap is None else bootstrap
    return (
        policy_text(anchor).rstrip()
        + "\nnon_releasable_bootstrap = [\n"
        + "".join(f'  "{sha}",\n' for sha in values)
        + "]\n"
    )


class FixtureRepo:
    """A synthetic git repository in a temp directory."""

    def __init__(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="latchkey-release-")
        self.root = Path(self._tmp.name)
        self.git("init", "-q")
        self.branch = self.git("symbolic-ref", "--short", "HEAD").stdout.strip()
        self.git("config", "user.email", "fixture@latchkey.invalid")
        self.git("config", "user.name", "Latchkey Fixture")
        self.git("config", "commit.gpgsign", "false")

    def cleanup(self) -> None:
        self._tmp.cleanup()

    def git(self, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["git", *args],
            cwd=self.root,
            env=GIT_ENV,
            capture_output=True,
            text=True,
            check=check,
        )

    def write(self, path: str, text: str) -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self, message: str, files: dict[str, str], remove: tuple[str, ...] = ()) -> str:
        for path, text in files.items():
            self.write(path, text)
        for path in remove:
            (self.root / path).unlink()
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD").stdout.strip()

    def tree_of(self, commit: str) -> str:
        return self.git("rev-parse", f"{commit}^{{tree}}").stdout.strip()

    def status(self) -> str:
        return self.git("status", "--porcelain").stdout

    def run(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(RELEASE_PY), "--root", str(self.root), *args],
            cwd=self.root,
            env=GIT_ENV,
            capture_output=True,
            text=True,
        )


def make_repo(anchor_files: dict[str, str] | None = None) -> FixtureRepo:
    """Anchor commit + release-policy.toml (untracked until the first chain commit)."""
    repo = FixtureRepo()
    repo.anchor = repo.commit(
        "anchor: requirements baseline",
        anchor_files if anchor_files is not None else {"README.md": "fixture\n"},
    )
    repo.policy = policy_text(repo.anchor)
    repo.write("release-policy.toml", repo.policy)
    return repo


def plan_json(repo: FixtureRepo, commit: str) -> dict:
    proc = repo.run("plan", "--commit", commit, "--json")
    assert proc.returncode == 0, f"plan failed: {proc.stderr}"
    return json.loads(proc.stdout)


class FixtureTestCase(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = make_repo()
        self.addCleanup(self.repo.cleanup)


# ---- direct helper tests ----------------------------------------------------


class VersionTests(unittest.TestCase):
    def test_bump_rules(self):
        self.assertEqual(release.bump_version((0, 0, 0), "patch"), (0, 0, 1))
        self.assertEqual(release.bump_version((0, 0, 3), "minor"), (0, 1, 0))
        self.assertEqual(release.bump_version((0, 9, 9), "major"), (1, 0, 0))
        self.assertEqual(release.format_version((1, 2, 3)), "1.2.3")

    def test_invalid_bump_rejected(self):
        with self.assertRaises(release.ReleaseError):
            release.bump_version((0, 0, 1), "none")

    def test_artifact_names_are_deterministic(self):
        self.assertEqual(
            release.artifact_names("0.1.0"),
            {
                "source_archive": "latchkey-v0.1.0-src.tar.gz",
                "binary_archive": "latchkey-v0.1.0-linux-x86_64-musl.tar.gz",
                "oci_ref": "ghcr.io/blogle/latchkey:v0.1.0",
                "checksums": "latchkey-v0.1.0-SHA256SUMS.txt",
                "notes": "latchkey-v0.1.0-notes.md",
                "changelog": "CHANGELOG.md",
            },
        )


class FragmentParseTests(unittest.TestCase):
    PATH = ".changes/LATCH-4.toml"

    def parse(self, text: str, path: str = PATH):
        return release.parse_fragment(path, text)

    def test_valid_fragment_with_optional_issue(self):
        fragment = self.parse(
            fragment_text(category="Fixed", summary="Fix the router", bump="patch",
                          issue_field="LATCH-4")
        )
        self.assertEqual(fragment.issue, "LATCH-4")
        self.assertEqual(fragment.category, "Fixed")
        self.assertEqual(fragment.summaries, ("Fix the router",))
        self.assertEqual(fragment.bump, "patch")

    def test_default_bump_is_patch(self):
        self.assertEqual(self.parse(fragment_text()).bump, "patch")

    def test_summary_array_becomes_multiple_summaries(self):
        fragment = self.parse(fragment_text(summary=["First change", "Second change"]))
        self.assertEqual(fragment.summaries, ("First change", "Second change"))

    def test_issue_must_match_filename(self):
        with self.assertRaises(release.ReleaseError) as ctx:
            self.parse(fragment_text(issue_field="LATCH-9"))
        self.assertIn("must match the filename stem", str(ctx.exception))

    def test_unknown_field_rejected(self):
        with self.assertRaises(release.ReleaseError) as ctx:
            self.parse(fragment_text(extra=('release = "none"',)))
        self.assertIn("unknown field", str(ctx.exception))

    def test_invalid_category_rejected(self):
        with self.assertRaises(release.ReleaseError) as ctx:
            self.parse(fragment_text(category="Docs"))
        self.assertIn("'category' must be one of", str(ctx.exception))

    def test_missing_category_rejected(self):
        with self.assertRaises(release.ReleaseError) as ctx:
            self.parse('summary = "x"\n')
        self.assertIn("missing required field 'category'", str(ctx.exception))

    def test_empty_summary_rejected(self):
        with self.assertRaises(release.ReleaseError):
            self.parse(fragment_text(summary="   "))
        with self.assertRaises(release.ReleaseError):
            self.parse('category = "Added"\nsummary = []\n')

    def test_invalid_bump_rejected(self):
        with self.assertRaises(release.ReleaseError) as ctx:
            self.parse(fragment_text(bump="none"))
        self.assertIn("'bump' must be one of", str(ctx.exception))

    def test_malformed_toml_rejected(self):
        with self.assertRaises(release.ReleaseError) as ctx:
            self.parse("category = \n")
        self.assertIn("invalid TOML", str(ctx.exception))

    def test_bad_filename_rejected(self):
        with self.assertRaises(release.ReleaseError) as ctx:
            self.parse(fragment_text(), path=".changes/notes.toml")
        self.assertIn("<ISSUE-ID>.toml", str(ctx.exception))

    def test_secret_heuristic_labels_without_echoing_value(self):
        cases = (
            ('category = "Fixed"\nsummary = "Rotate password: hunter2-rotated"\n',
             "credential-looking assignment"),
            ('category = "Fixed"\nsummary = "Use key AKIAABCDEFGH12345678"\n',
             "AWS access key id"),
            ('category = "Fixed"\nsummary = "x"\n', None),  # control
        )
        for text, label in cases:
            if label is None:
                self.parse(text)
                continue
            with self.assertRaises(release.ReleaseError) as ctx:
                self.parse(text)
            message = str(ctx.exception)
            self.assertIn(label, message)
            for secret in ("hunter2-rotated", "AKIAABCDEFGH12345678"):
                self.assertNotIn(secret, message)


class NotesRenderTests(unittest.TestCase):
    def test_render_notes_dojo_style(self):
        fragment = release.parse_fragment(
            ".changes/TCK-2.toml",
            fragment_text(
                category="Breaking",
                summary=["Drop the legacy config format", "Require static credentials"],
                bump="minor",
            ),
        )
        notes = release.render_notes("0.1.0", (fragment,))
        self.assertEqual(
            notes,
            "## v0.1.0\n"
            "\n"
            "- Drop the legacy config format (TCK-2)\n"
            "- Require static credentials (TCK-2)\n",
        )

    def test_render_notes_empty_is_empty(self):
        self.assertEqual(release.render_notes("0.0.0", ()), "")

    def test_sha256_hex(self):
        self.assertEqual(
            release.sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )


# ---- plan: history matrix ---------------------------------------------------


class EmptyHistoryTests(FixtureTestCase):
    def test_plan_at_anchor_yields_starting_version(self):
        document = plan_json(self.repo, self.repo.anchor)
        self.assertEqual(document["schema"], "latchkey-release-plan/v1")
        self.assertEqual(document["policy"]["anchor"], self.repo.anchor)
        self.assertEqual(document["version"], "0.0.0")
        self.assertEqual(document["tag"], "v0.0.0")
        self.assertIsNone(document["fragment"])
        self.assertIsNone(document["parent"])  # the anchor is a root commit
        self.assertEqual(document["unreleased_ancestors"], [])
        self.assertEqual(
            document["notes_sha256"],
            hashlib.sha256(b"").hexdigest(),
        )
        self.assertEqual(document["tree"], self.repo.tree_of(self.repo.anchor))
        self.assertEqual(document["artifacts"], release.artifact_names("0.0.0"))

    def test_changelog_of_empty_history(self):
        out = self.repo.root / "empty-changelog.md"
        proc = self.repo.run(
            "changelog", "--commit", self.repo.anchor, "--output", str(out)
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        text = out.read_text(encoding="utf-8")
        self.assertTrue(text.startswith("# Latchkey changelog\n"))
        self.assertIn(self.repo.anchor, text)
        self.assertTrue(text.endswith("No releases yet: the anchored history has no commits.\n"))


class PatchHistoryTests(FixtureTestCase):
    def test_single_patch_commit(self):
        c1 = self.repo.commit(
            "feat: one", {".changes/TCK-1.toml": fragment_text(summary="Add the one")}
        )
        document = plan_json(self.repo, c1)
        self.assertEqual(document["version"], "0.0.1")
        self.assertEqual(document["tag"], "v0.0.1")
        self.assertEqual(document["parent"], self.repo.anchor)
        self.assertEqual(document["tree"], self.repo.tree_of(c1))
        self.assertEqual(document["unreleased_ancestors"], [])
        fragment = document["fragment"]
        self.assertEqual(fragment["path"], ".changes/TCK-1.toml")
        self.assertEqual(fragment["issue"], "TCK-1")
        self.assertEqual(fragment["summary"], ["Add the one"])
        self.assertEqual(fragment["bump"], "patch")
        expected_notes = "## v0.0.1\n\n- Add the one (TCK-1)\n"
        self.assertEqual(
            document["notes_sha256"], hashlib.sha256(expected_notes.encode()).hexdigest()
        )
        self.assertEqual(
            document["artifacts"]["source_archive"], "latchkey-v0.0.1-src.tar.gz"
        )
        self.assertEqual(document["artifacts"]["oci_ref"], "ghcr.io/blogle/latchkey:v0.0.1")

    def test_three_patch_commits_yield_three_versions(self):
        c1 = self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        c2 = self.repo.commit("feat: two", {".changes/TCK-2.toml": fragment_text()})
        c3 = self.repo.commit("feat: three", {".changes/TCK-3.toml": fragment_text()})
        self.assertEqual(plan_json(self.repo, c1)["version"], "0.0.1")
        self.assertEqual(plan_json(self.repo, c2)["version"], "0.0.2")
        document = plan_json(self.repo, c3)
        self.assertEqual(document["version"], "0.0.3")
        ancestors = document["unreleased_ancestors"]
        self.assertEqual([entry["version"] for entry in ancestors], ["0.0.1", "0.0.2"])
        self.assertEqual([entry["commit"] for entry in ancestors], [c1, c2])
        self.assertEqual(document["parent"], c2)


class BumpDirectiveTests(FixtureTestCase):
    def test_minor_and_major_sequence(self):
        c1 = self.repo.commit("feat: p", {".changes/TCK-1.toml": fragment_text()})
        c2 = self.repo.commit(
            "feat: minor", {".changes/TCK-2.toml": fragment_text(bump="minor")}
        )
        c3 = self.repo.commit("feat: p2", {".changes/TCK-3.toml": fragment_text()})
        c4 = self.repo.commit(
            "feat: major", {".changes/TCK-4.toml": fragment_text(bump="major")}
        )
        self.assertEqual(plan_json(self.repo, c1)["version"], "0.0.1")  # patch default
        self.assertEqual(plan_json(self.repo, c2)["version"], "0.1.0")
        self.assertEqual(plan_json(self.repo, c3)["version"], "0.1.1")
        self.assertEqual(plan_json(self.repo, c4)["version"], "1.0.0")
        self.assertEqual(plan_json(self.repo, c4)["tag"], "v1.0.0")

    def test_major_from_starting_version(self):
        c1 = self.repo.commit(
            "feat!: break", {".changes/TCK-1.toml": fragment_text(bump="major")}
        )
        self.assertEqual(plan_json(self.repo, c1)["version"], "1.0.0")


class MalformedFragmentTests(FixtureTestCase):
    def _expect_fragment_error(self, files: dict[str, str], needle: str):
        commit = self.repo.commit("feat: broken", files)
        proc = self.repo.run("plan", "--commit", commit, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn(needle, proc.stderr)
        self.assertIn("latchkey: error:", proc.stderr)
        return proc

    def test_invalid_category_in_history(self):
        self._expect_fragment_error(
            {".changes/TCK-1.toml": fragment_text(category="Docs")},
            "'category' must be one of",
        )

    def test_malformed_toml_in_history(self):
        self._expect_fragment_error(
            {".changes/TCK-1.toml": "category = \n"}, "invalid TOML"
        )

    def test_unknown_field_in_history(self):
        self._expect_fragment_error(
            {".changes/TCK-1.toml": fragment_text(extra=('release = "none"',))},
            "unknown field",
        )

    def test_secret_in_history(self):
        proc = self._expect_fragment_error(
            {".changes/TCK-1.toml": fragment_text(summary="Rotate password: hunter2-rotated")},
            "possible secret",
        )
        self.assertNotIn("hunter2-rotated", proc.stderr)


class MissingAndAmbiguousFragmentTests(FixtureTestCase):
    def test_fragmentless_commit_rejected(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        c2 = self.repo.commit("chore: touch readme", {"README.md": "changed\n"})
        proc = self.repo.run("plan", "--commit", c2, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("introduces no changelog fragment", proc.stderr)
        self.assertIn(c2[:12], proc.stderr)
        self.assertIn("chore: touch readme", proc.stderr)

    def test_two_fragments_in_one_commit_rejected(self):
        commit = self.repo.commit(
            "feat: two fragments",
            {
                ".changes/TCK-1.toml": fragment_text(),
                ".changes/TCK-2.toml": fragment_text(),
            },
        )
        proc = self.repo.run("plan", "--commit", commit, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("introduces 2 changelog fragments", proc.stderr)
        self.assertIn("exactly one", proc.stderr)


class DuplicateIssueTests(FixtureTestCase):
    def test_plan_rejects_case_variant_issue_reuse(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        commit = self.repo.commit(
            "feat: reuse", {".changes/tck-1.toml": fragment_text()}
        )
        proc = self.repo.run("plan", "--commit", commit, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("reuses changelog issue id", proc.stderr)
        self.assertIn("tck-1", proc.stderr)

    def test_validate_fragment_rejects_duplicate_issue_id(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        self.repo.write(".changes/tck-1.toml", fragment_text())
        proc = self.repo.run("validate-fragment", "--base", "HEAD")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("duplicate issue id", proc.stderr)


class ImmutabilityTests(FixtureTestCase):
    def test_plan_rejects_modified_merged_fragment(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        commit = self.repo.commit(
            "feat: tamper",
            {
                ".changes/TCK-1.toml": fragment_text(summary="Rewritten history"),
                ".changes/TCK-2.toml": fragment_text(),
            },
        )
        proc = self.repo.run("plan", "--commit", commit, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("modifies previously merged", proc.stderr)
        self.assertIn("TCK-1.toml", proc.stderr)

    def test_plan_rejects_deleted_merged_fragment(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        commit = self.repo.commit(
            "feat: delete", {".changes/TCK-2.toml": fragment_text()},
            remove=(".changes/TCK-1.toml",),
        )
        proc = self.repo.run("plan", "--commit", commit, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("modifies previously merged", proc.stderr)

    def test_validate_fragment_rejects_worktree_edits(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        # Modified.
        self.repo.write(".changes/TCK-1.toml", fragment_text(summary="Tampered"))
        proc = self.repo.run("validate-fragment", "--base", "HEAD")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("previously merged", proc.stderr)
        # Deleted.
        (self.repo.root / ".changes" / "TCK-1.toml").unlink()
        proc = self.repo.run("validate-fragment", "--base", "HEAD")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("previously merged", proc.stderr)


# ---- validate-fragment ------------------------------------------------------


class ValidateFragmentTests(FixtureTestCase):
    def test_happy_path_exactly_one_new_fragment(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        self.repo.write(".changes/TCK-2.toml", fragment_text(summary="Add the two"))
        proc = self.repo.run("validate-fragment", "--base", "HEAD")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("exactly 1 new fragment", proc.stdout)
        self.assertIn(".changes/TCK-2.toml", proc.stdout)

    def test_first_fragment_ever_is_accepted(self):
        self.repo.write(".changes/TCK-1.toml", fragment_text())
        proc = self.repo.run("validate-fragment", "--base", "HEAD")
        self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_no_new_fragment_rejected(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        proc = self.repo.run("validate-fragment", "--base", "HEAD")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("no new changelog fragment", proc.stderr)

    def test_two_new_fragments_rejected(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        self.repo.write(".changes/TCK-2.toml", fragment_text())
        self.repo.write(".changes/TCK-3.toml", fragment_text())
        proc = self.repo.run("validate-fragment", "--base", "HEAD")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("2 new changelog fragments", proc.stderr)

    def test_malformed_new_fragment_rejected(self):
        self.repo.write(".changes/TCK-1.toml", fragment_text(category="Docs"))
        proc = self.repo.run("validate-fragment", "--base", "HEAD")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("'category' must be one of", proc.stderr)

    def test_secret_in_new_fragment_rejected(self):
        self.repo.write(
            ".changes/TCK-1.toml", fragment_text(summary="Rotate password: hunter2-rotated")
        )
        proc = self.repo.run("validate-fragment", "--base", "HEAD")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("possible secret", proc.stderr)
        self.assertNotIn("hunter2-rotated", proc.stderr)

    def test_issue_field_mismatch_rejected(self):
        self.repo.write(
            ".changes/TCK-1.toml", fragment_text(issue_field="TCK-2")
        )
        proc = self.repo.run("validate-fragment", "--base", "HEAD")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("must match the filename stem", proc.stderr)


# ---- structural rejections --------------------------------------------------


class HistoricalRewriteTests(FixtureTestCase):
    def test_missing_anchor_object_rejected(self):
        fabricated = "a" * 40
        self.repo.write("release-policy.toml", policy_text(fabricated))
        commit = self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        proc = self.repo.run("plan", "--commit", commit, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("does not exist in this repository", proc.stderr)
        self.assertIn(fabricated, proc.stderr)

    def test_rewritten_history_rejects_stale_anchor(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        # Rewrite history by amending the anchor itself; the configured anchor
        # object still exists but is no longer on the first-parent chain.
        self.repo.git("reset", "--hard", self.repo.anchor)
        self.repo.git("commit", "--amend", "-q", "-m", "anchor: rewritten baseline")
        self.repo.write("release-policy.toml", self.repo.policy)
        commit = self.repo.commit("feat: after rewrite", {".changes/TCK-3.toml": fragment_text()})
        proc = self.repo.run("plan", "--commit", commit, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("is not on the first-parent history", proc.stderr)
        self.assertIn("anchor rewritten", proc.stderr)


class BootstrapPolicyTests(FixtureTestCase):
    def test_policy_requires_the_exact_ordered_full_sha_list(self):
        with tempfile.TemporaryDirectory(prefix="latchkey-policy-") as tmp:
            path = Path(tmp) / "policy.toml"
            path.write_text(
                policy_with_bootstrap(self.repo.anchor, reversed(release.BOOTSTRAP_COMMITS)),
                encoding="utf-8",
            )
            with self.assertRaises(release.ReleaseError) as ctx:
                release.load_policy(path)
            self.assertIn("exactly the three configured full immutable SHAs", str(ctx.exception))

            path.write_text(
                policy_with_bootstrap(self.repo.anchor, ["a014e4e"]),
                encoding="utf-8",
            )
            with self.assertRaises(release.ReleaseError):
                release.load_policy(path)

    def test_missing_configured_bootstrap_history_fails_closed(self):
        self.repo.write(
            "release-policy.toml", policy_with_bootstrap(self.repo.anchor)
        )
        commit = self.repo.commit(
            "feat: after configured baseline",
            {".changes/TCK-1.toml": fragment_text()},
        )
        proc = self.repo.run("plan", "--commit", commit, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("configured non-releasable bootstrap commit(s) missing", proc.stderr)


class ActualRepositoryHistoryTests(unittest.TestCase):
    def test_real_repository_plan_maps_release_fragments_and_skips_baseline(self):
        before = subprocess.run(
            ["git", "status", "--porcelain"], cwd=REPO,
            capture_output=True, text=True, check=True,
        ).stdout
        master = subprocess.run(
            ["git", "rev-parse", "--verify", "origin/master^{commit}"], cwd=REPO,
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        latch_31_in_master = subprocess.run(
            ["git", "cat-file", "-e", f"{master}:.changes/LATCH-31.toml"],
            cwd=REPO, capture_output=True,
        ).returncode == 0
        proc = subprocess.run(
            [sys.executable, str(RELEASE_PY), "plan", "--commit", master, "--json"],
            cwd=REPO, capture_output=True, text=True,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        document = json.loads(proc.stdout)
        entries = document["unreleased_ancestors"] + [document]
        release_entries = [entry for entry in entries if entry["fragment"] is not None]
        expected_releases = [
            ("LATCH-1", "0.0.1"),
            ("LATCH-3", "0.0.2"),
            ("LATCH-2", "0.0.3"),
            ("LATCH-4", "0.0.4"),
        ]
        if latch_31_in_master:
            expected_releases.append(("LATCH-31", "0.0.5"))
        self.assertEqual(
            [(entry["fragment"]["issue"], entry["version"]) for entry in release_entries],
            expected_releases,
        )
        self.assertFalse(
            set(release.BOOTSTRAP_COMMITS).intersection(entry["commit"] for entry in entries)
        )
        after = subprocess.run(
            ["git", "status", "--porcelain"], cwd=REPO,
            capture_output=True, text=True, check=True,
        ).stdout
        self.assertEqual(after, before, "plan must not mutate tracked or untracked state")

    def test_grandfathered_commits_are_exact_and_other_fragmentless_commits_fail(self):
        self.assertEqual(len(set(release.BOOTSTRAP_COMMITS)), 3)
        self.assertTrue(all(release.SHA_RE.fullmatch(sha) for sha in release.BOOTSTRAP_COMMITS))
        # The actual plan proves these exact baseline commits are skipped; this
        # fixture proves that merely being fragmentless never grants an exemption.
        repo = make_repo()
        self.addCleanup(repo.cleanup)
        repo.write("release-policy.toml", policy_with_bootstrap(repo.anchor))
        commit = repo.commit("chore: unrelated fragmentless", {"README.md": "changed\n"})
        proc = repo.run("plan", "--commit", commit, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("introduces no changelog fragment", proc.stderr)


class MergeCommitTests(FixtureTestCase):
    def test_merge_commit_rejected(self):
        c1 = self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        self.repo.git("checkout", "-q", "-b", "topic")
        self.repo.commit("side", {".changes/TCK-9.toml": fragment_text()})
        self.repo.git("checkout", "-q", self.repo.branch)
        self.repo.git("merge", "-q", "--no-ff", "topic", "-m", "merge topic")
        head = self.repo.git("rev-parse", "HEAD").stdout.strip()
        proc = self.repo.run("plan", "--commit", head, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("is a merge commit", proc.stderr)
        self.assertIn(head[:12], proc.stderr)
        # The linear pre-merge commit still plans.
        self.assertEqual(plan_json(self.repo, c1)["version"], "0.0.1")


class SideBranchTests(FixtureTestCase):
    """First-parent traversal: commits off the planned line are never folded."""

    def setUp(self) -> None:
        super().setUp()
        self.c1 = self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        self.repo.git("checkout", "-q", "-b", "topic")
        self.s1 = self.repo.commit("side: no fragment", {"side.txt": "x\n"})
        self.s2 = self.repo.commit(
            "side: fragment", {".changes/TCK-9.toml": fragment_text(summary="Side feature")}
        )
        self.repo.git("checkout", "-q", self.repo.branch)
        self.c2 = self.repo.commit("feat: two", {".changes/TCK-2.toml": fragment_text()})

    def test_mainline_plan_ignores_side_branch(self):
        document = plan_json(self.repo, self.c2)
        self.assertEqual(document["version"], "0.0.2")
        self.assertEqual(
            [entry["commit"] for entry in document["unreleased_ancestors"]], [self.c1]
        )
        self.assertEqual(document["fragment"]["issue"], "TCK-2")
        # No side content leaks in (a --all scan would have folded TCK-9).
        proc = self.repo.run(
            "changelog", "--commit", self.c2, "--output",
            str(self.repo.root / "side-check.md"),
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        text = (self.repo.root / "side-check.md").read_text(encoding="utf-8")
        self.assertIn("TCK-1", text)
        self.assertIn("TCK-2", text)
        self.assertNotIn("TCK-9", text)
        self.assertNotIn("Side feature", text)

    def test_planning_the_side_tip_walks_only_its_own_chain(self):
        proc = self.repo.run("plan", "--commit", self.s2, "--json")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("introduces no changelog fragment", proc.stderr)
        self.assertIn(self.s1[:12], proc.stderr)


# ---- tag verification -------------------------------------------------------


class VerifyTagTests(FixtureTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.c1 = self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        self.c2 = self.repo.commit("feat: two", {".changes/TCK-2.toml": fragment_text()})

    def test_exact_target_and_name_pass(self):
        self.repo.git("tag", "v0.0.1", self.c1)
        proc = self.repo.run("verify-tag", "--tag", "v0.0.1", "--commit", self.c1)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("verify-tag: ok", proc.stdout)
        # Annotated tags dereference to the same commit.
        self.repo.git("tag", "-f", "-a", "v0.0.2", "-m", "release", self.c2)
        proc = self.repo.run("verify-tag", "--tag", "v0.0.2", "--commit", self.c2)
        self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_wrong_tag_target_rejected(self):
        self.repo.git("tag", "v0.0.1", self.c1)
        proc = self.repo.run("verify-tag", "--tag", "v0.0.1", "--commit", self.c2)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("points at", proc.stderr)
        self.assertIn(self.c1, proc.stderr)
        self.assertIn(f"expected exactly {self.c2}", proc.stderr)

    def test_tag_name_must_match_planned_version(self):
        self.repo.git("tag", "v9.9.9", self.c1)
        proc = self.repo.run("verify-tag", "--tag", "v9.9.9", "--commit", self.c1)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("does not match the planned version", proc.stderr)
        self.assertIn("expected v0.0.1", proc.stderr)

    def test_missing_tag_rejected(self):
        proc = self.repo.run("verify-tag", "--tag", "v0.0.5", "--commit", self.c1)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("does not exist", proc.stderr)

    def test_malformed_tag_name_rejected(self):
        proc = self.repo.run("verify-tag", "--tag", "release-1", "--commit", self.c1)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("invalid release tag", proc.stderr)


# ---- determinism ------------------------------------------------------------


class IdempotenceTests(FixtureTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.c1 = self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        self.c2 = self.repo.commit("feat: two", {".changes/TCK-2.toml": fragment_text()})
        self.c3 = self.repo.commit("feat: three", {".changes/TCK-3.toml": fragment_text()})

    def test_plan_rerun_is_byte_identical(self):
        first = self.repo.run("plan", "--commit", self.c3, "--json")
        second = self.repo.run("plan", "--commit", self.c3, "--json")
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout, second.stdout)

    def test_changelog_rerun_is_byte_identical(self):
        out = self.repo.root / "rendered.md"
        proc = self.repo.run("changelog", "--commit", self.c3, "--output", str(out))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        first_bytes = out.read_bytes()
        proc = self.repo.run("changelog", "--commit", self.c3, "--output", str(out))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(out.read_bytes(), first_bytes)


class DelayedPublicationTests(FixtureTestCase):
    """An out-of-order publisher computes the identical commit->version mapping."""

    def setUp(self) -> None:
        super().setUp()
        self.c1 = self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        self.c2 = self.repo.commit(
            "feat: two", {".changes/TCK-2.toml": fragment_text(bump="minor")}
        )
        self.c3 = self.repo.commit("feat: three", {".changes/TCK-3.toml": fragment_text()})

    def test_older_commit_planned_after_newer_matches(self):
        newest = plan_json(self.repo, self.c3)
        self.assertEqual(newest["version"], "0.1.1")
        entries = newest["unreleased_ancestors"]
        self.assertEqual([entry["version"] for entry in entries], ["0.0.1", "0.1.0"])

        older = plan_json(self.repo, self.c1)  # planned AFTER the newest
        self.assertEqual(
            {key: older[key] for key in entries[0]},
            entries[0],
            "plan(C1) core fields must equal plan(C3)'s first ancestor entry",
        )
        middle = plan_json(self.repo, self.c2)
        self.assertEqual(
            {key: middle[key] for key in entries[1]},
            entries[1],
            "plan(C2) core fields must equal plan(C3)'s second ancestor entry",
        )
        self.assertEqual(middle["unreleased_ancestors"][0]["commit"], self.c1)


class ChangelogRenderTests(FixtureTestCase):
    def test_dojo_style_sections_snapshot(self):
        c1 = self.repo.commit(
            "feat: search",
            {".changes/TCK-1.toml": fragment_text(summary="Add the catalog search endpoint")},
        )
        c2 = self.repo.commit(
            "feat!: drop legacy",
            {
                ".changes/TCK-2.toml": fragment_text(
                    category="Breaking",
                    summary=[
                        "Drop the legacy config format",
                        "Require static credentials",
                    ],
                    bump="minor",
                )
            },
        )
        out = self.repo.root / "rendered-changelog.md"
        proc = self.repo.run("changelog", "--commit", c2, "--output", str(out))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        expected = (
            "# Latchkey changelog\n"
            "\n"
            f"<!-- GENERATED by scripts/release.py changelog --commit {c2}.\n"
            "     Release output artifact: this file is NEVER committed to master.\n"
            "     Root CHANGELOG.md is a pointer document; see docs/releases.md. -->\n"
            "\n"
            "## v0.1.0\n"
            "\n"
            "- Drop the legacy config format (TCK-2)\n"
            "- Require static credentials (TCK-2)\n"
            "\n"
            "## v0.0.1\n"
            "\n"
            "- Add the catalog search endpoint (TCK-1)\n"
        )
        self.assertEqual(out.read_text(encoding="utf-8"), expected)
        # The plan's notes hash covers exactly that version's section.
        document = plan_json(self.repo, c2)
        section_v010 = (
            "## v0.1.0\n"
            "\n"
            "- Drop the legacy config format (TCK-2)\n"
            "- Require static credentials (TCK-2)\n"
        )
        self.assertEqual(
            document["notes_sha256"], hashlib.sha256(section_v010.encode()).hexdigest()
        )
        self.assertEqual(plan_json(self.repo, c1)["version"], "0.0.1")


class PurityTests(FixtureTestCase):
    def test_plan_and_changelog_leave_tracked_files_untouched(self):
        self.repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        c2 = self.repo.commit("feat: two", {".changes/TCK-2.toml": fragment_text()})
        before = self.repo.status()
        self.assertEqual(before, "", "fixture must start with a clean worktree")
        plan_proc = self.repo.run("plan", "--commit", c2, "--json")
        self.assertEqual(plan_proc.returncode, 0, plan_proc.stderr)
        out = self.repo.root / "generated.md"
        changelog_proc = self.repo.run(
            "changelog", "--commit", c2, "--output", str(out)
        )
        self.assertEqual(changelog_proc.returncode, 0, changelog_proc.stderr)
        out.unlink()
        self.assertEqual(self.repo.status(), before)

    def test_plan_ignores_worktree_fragment_edits(self):
        c1 = self.repo.commit(
            "feat: one", {".changes/TCK-1.toml": fragment_text(summary="Committed summary")}
        )
        before = self.repo.run("plan", "--commit", c1, "--json").stdout
        self.repo.write(
            ".changes/TCK-1.toml", fragment_text(summary="Uncommitted tampering")
        )
        after = self.repo.run("plan", "--commit", c1, "--json").stdout
        self.assertEqual(before, after, "plan must be a pure function of committed history")

        out = self.repo.root / "edited.md"
        proc = self.repo.run("changelog", "--commit", c1, "--output", str(out))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        text = out.read_text(encoding="utf-8")
        self.assertIn("Committed summary", text)
        self.assertNotIn("Uncommitted tampering", text)

    def test_changelog_refuses_tracked_output(self):
        repo = make_repo(anchor_files={"README.md": "x\n", "CHANGELOG.md": "# old\n"})
        self.addCleanup(repo.cleanup)
        commit = repo.commit("feat: one", {".changes/TCK-1.toml": fragment_text()})
        proc = repo.run(
            "changelog", "--commit", commit, "--output", str(repo.root / "CHANGELOG.md")
        )
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("refusing to write", proc.stderr)
        self.assertIn("never committed to master", proc.stderr)
        self.assertEqual(
            (repo.root / "CHANGELOG.md").read_text(encoding="utf-8"), "# old\n"
        )


# ---- the LATCH-4 suite-discovery glue --------------------------------------


def lk_script_suites(root: Path | None = None) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    if root is not None:
        env["LK_ROOT"] = str(root)
    return subprocess.run(
        ["bash", "-euo", "pipefail", "-c", f'source "{LIB}"\nlk_script_suites\n'],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(REPO),
    )


class SuiteDiscoveryGlueTests(unittest.TestCase):
    """Guards the generic multi-root rule this ticket adds to lib.sh."""

    def test_real_repo_lists_dispatch_and_release_policy(self):
        proc = lk_script_suites()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(sorted(proc.stdout.split()), ["dispatch", "release_policy"])

    def test_three_root_union_deduplicated_sorted(self):
        with tempfile.TemporaryDirectory(prefix="latchkey-suites-") as tmp:
            root = Path(tmp)
            (root / "scripts" / "dev" / "tests").mkdir(parents=True)
            (root / "scripts" / "dev" / "tests" / "test_alpha.py").write_text("")
            (root / "scripts" / "dev" / "tests" / "test_dup.py").write_text("")
            (root / "tests" / "beta").mkdir(parents=True)
            (root / "tests" / "beta" / "test_one.py").write_text("")
            (root / "tests" / "dup").mkdir(parents=True)
            (root / "tests" / "dup" / "test_dup.py").write_text("")
            (root / "tests" / "not-a-suite").mkdir()
            (root / "tests" / "contracts.rs").write_text("")
            (root / "scripts" / "ci" / "tests").mkdir(parents=True)
            (root / "scripts" / "ci" / "tests" / "test_gamma.py").write_text("")
            proc = lk_script_suites(root)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            listed = proc.stdout.split()
            self.assertEqual(listed, sorted(set(listed)))
            self.assertEqual(listed, ["alpha", "beta", "dup", "gamma"])

    def test_unknown_suite_fails_fast_listing_the_rule(self):
        proc = subprocess.run(
            ["bash", str(SCRIPT_TEST), "no-such-suite"],
            capture_output=True,
            text=True,
            env=GIT_ENV,
            cwd=str(REPO),
        )
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("no suite 'no-such-suite'", proc.stderr)
        self.assertIn("scripts/dev/tests/test_no-such-suite.py", proc.stderr)
        self.assertIn("tests/no-such-suite/", proc.stderr)
        self.assertIn("scripts/ci/tests/test_no-such-suite.py", proc.stderr)
        self.assertIn("dispatch", proc.stderr)
        self.assertIn("release_policy", proc.stderr)


if __name__ == "__main__":
    unittest.main()
