"""Select a release snapshot without rebuilding unchanged candidate artifacts."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

if __package__:
    from .delta_workflow import (
        BASE_RE, GIT_TIMEOUT_SECONDS, PROJECT_ROOT, DeltaWorkflowError,
        _git_command, _git_environment, _run_git,
    )
else:
    from delta_workflow import (
        BASE_RE, GIT_TIMEOUT_SECONDS, PROJECT_ROOT, DeltaWorkflowError,
        _git_command, _git_environment, _run_git,
    )


WORKFLOW = ".github/workflows/release.yml"
RELEASE_ONLY_PATHS = frozenset({
    "CHANGELOG.md", "README.md", "docs/next/README.md",
    "ARCHITECTURE.md", "CONTRIBUTING.md", "AGENT_IMPROVEMENTS.md",
    "website/preview.json",
    "scripts/release_promotion.py", "scripts/test_release_promotion.py",
})
PUBLICATION_STEPS = (
    ("Validate immutable release candidate", "candidate"),
    ("Verify and publish stable release", "release"),
    ("Generate preview update manifest", "manifest"),
)


def workflow_contract(text: str) -> str:
    """Keep every workflow byte except three existing publication shell bodies.

    This accepts only the repository's literal `run: |` blocks with ten-space
    content indentation. It is not a general YAML parser: an unsupported or
    ambiguous layout fails closed. Dedented content always remains in the contract.
    """
    if any(character in text for character in ("\r", "\x00", "\v", "\f", "\x85", "\u2028", "\u2029")):
        raise DeltaWorkflowError("unsupported release workflow text encoding")
    lines = text.splitlines(keepends=True)

    def unique(line: str, start: int = 0, end: int | None = None) -> int:
        matches = [i for i in range(start, len(lines) if end is None else end) if lines[i] == line]
        if len(matches) != 1:
            raise DeltaWorkflowError(f"unsupported release workflow boundary: {line.strip()!r}")
        return matches[0]

    jobs = unique("jobs:\n")
    publish = unique("  publish:\n", jobs + 1)
    finish = unique("  verify-update-feed:\n", publish + 1)
    result: list[str] = []
    cursor = 0
    for name, identity in PUBLICATION_STEPS:
        step = unique(f"      - name: {name}\n", publish + 1, finish)
        if step < cursor:
            raise DeltaWorkflowError("publication steps changed order")
        next_step = next(
            (i for i in range(step + 1, finish) if lines[i].startswith("      - ")),
            finish,
        )
        body = unique("        run: |\n", step + 1, next_step) + 1
        unique(f"        id: {identity}\n", step + 1, body - 1)
        unique("        shell: bash\n", step + 1, body - 1)
        if body >= next_step or not lines[body].startswith("          ") or lines[body][10:11].isspace():
            raise DeltaWorkflowError("publication shell must start at ten-space indentation")
        end = body
        while end < next_step and (not lines[end].strip() or lines[end].startswith("          ")):
            end += 1
        result.extend(lines[cursor:body])
        result.append(f"          <publication-only:{identity}>\n")
        cursor = end
    result.extend(lines[cursor:])
    return "".join(result)


def _commit(project_root: Path, ref: str) -> str:
    return _run_git(project_root, [
        "rev-parse", "--verify", "--end-of-options", f"{ref}^{{commit}}",
    ]).stdout.strip()


def _ancestor(project_root: Path, ancestor: str, descendant: str) -> None:
    if ancestor == descendant:
        return
    result = _run_git(project_root, [
        "merge-base", "--is-ancestor", ancestor, descendant,
    ], check=False)
    if result.returncode != 0:
        raise DeltaWorkflowError(f"revision {ancestor} is not on the selected history {descendant}")


def _workflow(project_root: Path, revision: str) -> str:
    # Policy inputs need exact UTF-8, not the general Git runner's lossy decoding
    # and universal-newline conversion used for command diagnostics.
    try:
        data = subprocess.check_output(
            _git_command(project_root, ["show", f"{revision}:{WORKFLOW}"]),
            cwd=project_root, env=_git_environment(), stderr=subprocess.PIPE,
            timeout=GIT_TIMEOUT_SECONDS,
        )
        return data.decode("utf-8")
    except (OSError, subprocess.SubprocessError, UnicodeDecodeError) as error:
        raise DeltaWorkflowError(f"could not read UTF-8 release workflow at {revision}: {error}") from error


def _validate_snapshot(project_root: Path, candidate: str, revision: str, contract: str) -> None:
    changes = _run_git(project_root, [
        "diff", "--raw", "--no-abbrev", "--no-renames", "-z", candidate, revision, "--",
    ]).stdout.split("\0")
    if changes[-1] != "" or len(changes) % 2 != 1:
        raise DeltaWorkflowError("could not read an unambiguous candidate tree difference")
    for header, path in zip(changes[0:-1:2], changes[1:-1:2]):
        if path not in RELEASE_ONLY_PATHS and path != WORKFLOW:
            raise DeltaWorkflowError(f"candidate build input changed: {path!r}")
        fields = header.removeprefix(":").split()
        if len(fields) != 5 or fields[0] not in {"000000", "100644", "100755"} or fields[1] not in {"100644", "100755"}:
            raise DeltaWorkflowError(f"release-only path must remain a regular file: {path!r}")
    if workflow_contract(_workflow(project_root, revision)) != contract:
        raise DeltaWorkflowError("candidate build or workflow scheduling contract changed")


def select_release_revision(
    candidate_control: str,
    candidate_workflow: str,
    promotion_ref: str,
    master_ref: str,
    *,
    release_ref: str | None = None,
    project_root: Path = PROJECT_ROOT,
) -> tuple[str, str]:
    """Return validated master and tag targets, preserving an existing tag target."""
    for value in (candidate_control, candidate_workflow):
        if BASE_RE.fullmatch(value) is None:
            raise DeltaWorkflowError("candidate identities must be full lowercase commit IDs")
    candidate = _commit(project_root, candidate_control)
    workflow = _commit(project_root, candidate_workflow)
    promotion = _commit(project_root, promotion_ref)
    master = _commit(project_root, master_ref)
    release = master if release_ref is None else _commit(project_root, release_ref)
    for revision in dict.fromkeys((workflow, promotion, release)):
        _ancestor(project_root, candidate, revision)
        _ancestor(project_root, revision, master)
    contract = workflow_contract(_workflow(project_root, workflow))
    for revision in dict.fromkeys((workflow, promotion, master, release)):
        if revision == candidate == workflow:
            continue
        _validate_snapshot(project_root, candidate, revision, contract)
    return master, release


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate-control", required=True)
    parser.add_argument("--candidate-workflow", required=True)
    parser.add_argument("--promotion-ref", required=True)
    parser.add_argument("--ref", default="origin/master")
    parser.add_argument("--release-ref")
    options = parser.parse_args()
    try:
        master, release = select_release_revision(
            options.candidate_control, options.candidate_workflow,
            options.promotion_ref, options.ref, release_ref=options.release_ref,
        )
    except DeltaWorkflowError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(json.dumps({"master_sha": master, "release_sha": release}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
