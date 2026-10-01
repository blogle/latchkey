#!/usr/bin/env python3
"""Run trusted foundation gate semantics against one reconstructed prefix."""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import subprocess
import sys
import unittest
from pathlib import Path
from typing import Any


FAST_GATES = {"fmt-check", "lint", "script-test-dispatch", "test-unit"}


def _run(command: list[str], root: Path, env: dict[str, str]) -> tuple[int, str]:
    proc = subprocess.run(command, cwd=root, env=env, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT)
    return proc.returncode, proc.stdout


def _cargo_gate(gate_id: str, cargo: str, root: Path, env: dict[str, str]) -> tuple[bool, str]:
    commands = {
        "fmt-check": [cargo, "fmt", "--all", "--", "--check"],
        "lint": [cargo, "clippy", "--profile", "ci", "--all-targets", "--locked", "--", "-D", "warnings"],
        "test-unit": [cargo, "test", "--profile", "ci", "--locked", "--lib"],
        "test-integration-contracts": [cargo, "test", "--profile", "ci", "--locked", "--test", "contracts"],
        "build": [cargo, "build", "--profile", "ci", "--locked"],
    }
    code, output = _run(commands[gate_id], root, env)
    if code:
        return False, output
    if gate_id in {"test-unit", "test-integration-contracts"}:
        match = re.search(r"running (\d+) tests?", output)
        if not match or int(match.group(1)) == 0:
            return False, output + "\nerror: cargo test reported zero tests\n"
    return True, output


def _discover_tests(root: Path, python: str, env: dict[str, str]) -> tuple[bool, str]:
    """Discover all script tests in this prefix, not in the final candidate."""
    roots: list[Path] = []
    for relative in ("scripts/dev/tests", "scripts/ci/tests"):
        path = root / relative
        if path.is_dir():
            roots.append(path)
    tests_root = root / "tests"
    if tests_root.is_dir():
        roots.extend(sorted(path for path in tests_root.iterdir()
                           if path.is_dir() and any(path.glob("test_*.py"))))
    loader = unittest.TestLoader()
    suite = unittest.TestSuite()
    details: list[str] = []
    if str(root) not in sys.path:
        sys.path.insert(0, str(root))
    for path in roots:
        discovered = loader.discover(str(path), pattern="test_*.py", top_level_dir=str(path))
        count = discovered.countTestCases()
        details.append(f"{path.relative_to(root)}: {count} tests")
        suite.addTests(discovered)
    count = suite.countTestCases()
    if count == 0:
        return False, "no script tests discovered in prefix\n" + "\n".join(details)
    # Run in-process with unittest's normal discovery/import semantics.
    output = io.StringIO()
    result = unittest.TextTestRunner(stream=output, verbosity=2).run(suite)
    summary = "\n".join(details) + f"\nscript tests run: {result.testsRun}\n{output.getvalue()}"
    return result.wasSuccessful() and result.testsRun > 0, summary


def run_prefix(root: Path, target: Path, cargo: str, python: str,
               gates: list[dict[str, str]], log_dir: Path) -> list[dict[str, Any]]:
    root = root.resolve()
    target = target.resolve()
    log_dir.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    for name in ("GITHUB_TOKEN", "GH_TOKEN", "GITHUB_READ_TOKEN"):
        env.pop(name, None)
    env.update({"CARGO_TARGET_DIR": str(target), "CARGO_NET_OFFLINE": "true",
                "CARGO_PROFILE": "ci", "LATCHKEY_PROFILE": "ci", "LK_ROOT": str(root)})
    outcomes: dict[str, bool] = {}
    results = []
    for gate in gates:
        gate_id, command = gate["id"], gate["command"]
        if gate_id == "pr-check":
            missing = FAST_GATES - outcomes.keys()
            passed = not missing and all(outcomes[item] for item in FAST_GATES)
            output = "verified constituent prefix gates: " + ", ".join(sorted(FAST_GATES))
            if missing:
                output += "\nmissing constituent gates: " + ", ".join(sorted(missing))
        elif gate_id == "script-test-dispatch":
            # Python is the absolute executable from the candidate's cached environment.
            passed, output = _discover_tests(root, python, env)
        elif gate_id in {"fmt-check", "lint", "test-unit", "test-integration-contracts", "build"}:
            passed, output = _cargo_gate(gate_id, cargo, root, env)
        else:
            raise ValueError(f"unsupported trusted foundation gate: {gate_id}")
        outcomes[gate_id] = passed
        (log_dir / f"{gate_id}.log").write_text(output, encoding="utf-8")
        results.append({"id": gate_id, "command": command,
                        "result": "passed" if passed else "failed",
                        "log": f"{gate_id}.log"})
    return results


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--cargo", required=True)
    parser.add_argument("--python", required=True)
    parser.add_argument("--gates", required=True)
    parser.add_argument("--logs", required=True)
    args = parser.parse_args()
    try:
        gates = json.loads(Path(args.gates).read_text(encoding="utf-8"))
        results = run_prefix(Path(args.root), Path(args.target), args.cargo, args.python,
                             gates, Path(args.logs))
        Path(args.logs, "results.json").write_text(json.dumps(results, sort_keys=True) + "\n")
        return 0 if all(item["result"] == "passed" for item in results) else 1
    except (OSError, ValueError, KeyError, TypeError) as exc:
        print(f"prefix gate execution failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
