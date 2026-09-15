#!/usr/bin/env python3
"""Materialize, finalize, and verify the maintained delta."""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from contextlib import contextmanager
from datetime import datetime
from email import policy
from email.parser import BytesParser
from email.utils import parseaddr, parsedate_to_datetime
from pathlib import Path
from typing import Sequence


PROJECT_ROOT = Path(__file__).resolve().parent.parent
BASE_RE = re.compile(r"^[0-9a-f]{40}$")
PATCH_RE = re.compile(r"^[0-9]{4}-[a-z0-9-]+\.patch$")
TASK_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,39}$")
ISSUE_RE = re.compile(r"(?<![\w/.-])[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+#[1-9][0-9]*(?![\w])")
INTEGRATION_ASSET_RE = re.compile(
    r'include_str!\("(?P<path>assets/(?P<integration>[^"/]+)/[^"\)]+)"\)'
)
INTEGRATION_VERSION_RE = re.compile(r"HERDR_INTEGRATION_VERSION=(?P<version>[0-9]+)")
GIT_TIMEOUT_SECONDS = 120
PREFIX_COMPILE_TIMEOUT_SECONDS = 1200
CONTROL_PATH_PREFIXES = (".github/", "patches/")
CONTROL_PATHS = {
    "AGENTS.md", "PRODUCT.md", "ARCHITECTURE.md", "CONTRIBUTING.md",
    "README.md", "docs/next/README.md", "website/preview.json",
}
DEVELOPMENT_BRANCH = "candidate/development"
DEVELOPMENT_REMOTE_REF = f"refs/heads/{DEVELOPMENT_BRANCH}"


class DeltaWorkflowError(RuntimeError):
    """A bounded delta workflow operation could not complete safely."""


@dataclass(frozen=True)
class ReplayResult:
    base: str
    mailboxes: tuple[str, ...]
    tree: str


@dataclass(frozen=True)
class WorktreeResult:
    branch: str
    path: Path
    base: str
    head: str
    tree: str
    mailbox_count: int


@dataclass(frozen=True)
class FinalizeResult:
    mailbox: str
    source_head: str
    source_tree: str
    replay_tree: str


@dataclass(frozen=True)
class DevelopmentResult:
    path: Path
    head: str
    tree: str


@dataclass(frozen=True)
class TopicWorktree:
    path: Path
    head: str
    branch: str


@dataclass(frozen=True)
class MailboxMetadata:
    author_name: str
    author_email: str
    author_date: str
    commit_message: str


def _git_environment(overrides: dict[str, str] | None = None) -> dict[str, str]:
    environment = os.environ.copy()
    environment.update(
        {
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_MERGE_AUTOEDIT": "no",
        }
    )
    if overrides:
        environment.update(overrides)
    return environment


def _run_git(
    project_root: Path,
    arguments: Sequence[str],
    *,
    cwd: Path | None = None,
    environment: dict[str, str] | None = None,
    check: bool = True,
    input_text: str | None = None,
) -> subprocess.CompletedProcess[str]:
    command = _git_command(project_root, arguments, cwd=cwd)
    try:
        result = subprocess.run(
            command,
            cwd=cwd or project_root,
            env=_git_environment(environment),
            input=input_text,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
            timeout=GIT_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise DeltaWorkflowError(
            f"could not run {' '.join(command)!r}: {error}"
        ) from error

    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no diagnostic"
        raise DeltaWorkflowError(
            f"{' '.join(command)!r} failed with exit code {result.returncode}: {detail}"
        )
    return result


def _git_command(
    project_root: Path,
    arguments: Sequence[str],
    *,
    cwd: Path | None = None,
) -> list[str]:
    command = ["git", "-c", "core.longpaths=true"]
    trusted: list[Path] = []
    for path in (project_root, cwd or project_root):
        resolved = path.resolve()
        if resolved not in trusted:
            trusted.append(resolved)
            command.extend(["-c", f"safe.directory={resolved.as_posix()}"])
    command.extend(arguments)
    return command


def unintegrated_topic_worktrees(
    project_root: Path,
    development_head: str,
) -> tuple[TopicWorktree, ...]:
    """Return linked topic heads not contained in the development head."""

    output = _run_git(
        project_root,
        ["worktree", "list", "--porcelain", "-z"],
    ).stdout
    linked: list[TopicWorktree] = []
    path: Path | None = None
    head = ""
    branch = ""
    for field in output.split("\0"):
        if field.startswith("worktree "):
            if path is not None and head and branch:
                linked.append(TopicWorktree(path, head, branch))
            path = Path(field.removeprefix("worktree ")).resolve()
            head = ""
            branch = ""
        elif field.startswith("HEAD "):
            head = field.removeprefix("HEAD ")
        elif field.startswith("branch refs/heads/"):
            branch = field.removeprefix("branch refs/heads/")
    if path is not None and head and branch:
        linked.append(TopicWorktree(path, head, branch))

    unintegrated: list[TopicWorktree] = []
    for worktree in linked:
        if not worktree.branch.startswith("agent/delta-"):
            continue
        ancestor = _run_git(
            project_root,
            ["merge-base", "--is-ancestor", worktree.head, development_head],
            check=False,
        )
        if ancestor.returncode != 0:
            unintegrated.append(worktree)
    return tuple(unintegrated)


def _read_base(project_root: Path) -> str:
    path = project_root / "patches" / "delta" / "BASE"
    try:
        base = path.read_text(encoding="utf-8").strip()
    except OSError as error:
        raise DeltaWorkflowError(f"could not read {path}: {error}") from error
    if BASE_RE.fullmatch(base) is None:
        raise DeltaWorkflowError(f"{path} must contain one full lowercase commit ID")
    _run_git(project_root, ["cat-file", "-e", f"{base}^{{commit}}"])
    return base


def _read_series(project_root: Path) -> tuple[str, ...]:
    delta_root = project_root / "patches" / "delta"
    path = delta_root / "series"
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise DeltaWorkflowError(f"could not read {path}: {error}") from error

    mailboxes: list[str] = []
    for line_number, raw_line in enumerate(lines, start=1):
        entry = raw_line.strip()
        if not entry or entry.startswith("#"):
            continue
        if PATCH_RE.fullmatch(entry) is None:
            raise DeltaWorkflowError(
                f"{path}:{line_number} contains invalid mailbox name {entry!r}"
            )
        if not (delta_root / entry).is_file():
            raise DeltaWorkflowError(f"mailbox {delta_root / entry} does not exist")
        mailboxes.append(entry)

    if not mailboxes:
        raise DeltaWorkflowError(f"{path} does not list any mailboxes")
    if len(mailboxes) != len(set(mailboxes)):
        raise DeltaWorkflowError(f"{path} contains duplicate mailbox names")
    return tuple(mailboxes)


def _tree_after_patches(
    project_root: Path,
    initial_tree: str,
    patch_paths: Sequence[Path],
    *,
    reverse: bool = False,
) -> str:
    with tempfile.TemporaryDirectory(prefix="herdr-delta-index-") as temp_dir:
        index_path = Path(temp_dir) / "index"
        index_environment = {"GIT_INDEX_FILE": str(index_path)}
        _run_git(
            project_root,
            ["read-tree", initial_tree],
            environment=index_environment,
        )
        for patch_path in patch_paths:
            arguments = [
                "apply",
                "--cached",
                "--3way",
                "--whitespace=nowarn",
            ]
            if reverse:
                arguments.append("--reverse")
            arguments.append(str(patch_path))
            _run_git(project_root, arguments, environment=index_environment)
        tree = _run_git(
            project_root, ["write-tree"], environment=index_environment
        ).stdout.strip()

    if BASE_RE.fullmatch(tree) is None:
        raise DeltaWorkflowError(f"git write-tree returned invalid tree ID {tree!r}")
    return tree


def replay_delta_tree(project_root: Path = PROJECT_ROOT) -> ReplayResult:
    """Apply the checked-in queue to a temporary index and return its tree ID."""

    project_root = project_root.resolve()
    base = _read_base(project_root)
    mailboxes = _read_series(project_root)
    delta_root = project_root / "patches" / "delta"
    tree = _tree_after_patches(
        project_root,
        base,
        [delta_root / mailbox for mailbox in mailboxes],
    )
    _run_git(project_root, ["diff", "--check", base, tree])
    return ReplayResult(base=base, mailboxes=mailboxes, tree=tree)


def _included_integration_assets(module_source: str) -> dict[str, set[str]]:
    assets: dict[str, set[str]] = {}
    for match in INTEGRATION_ASSET_RE.finditer(module_source):
        path = f"src/integration/{match.group('path')}"
        assets.setdefault(match.group("integration"), set()).add(path)
    return assets


def _integration_marker_versions(
    integration: str,
    paths: set[str],
    assets: dict[str, str | None],
) -> set[int]:
    versions: set[int] = set()
    for path in paths:
        source = assets.get(path)
        if source is None:
            continue
        versions.update(
            int(match.group("version"))
            for match in INTEGRATION_VERSION_RE.finditer(source)
        )
    if len(versions) > 1:
        raise DeltaWorkflowError(
            f"managed integration {integration} has conflicting version markers: "
            f"{sorted(versions)}"
        )
    return versions


def validate_integration_asset_version_changes(
    current_module: str,
    current_assets: dict[str, str | None],
    baseline_module: str,
    baseline_assets: dict[str, str | None],
) -> tuple[str, ...]:
    """Require a higher migration version for each changed embedded integration."""

    current_groups = _included_integration_assets(current_module)
    baseline_groups = _included_integration_assets(baseline_module)
    changed: list[str] = []
    for integration in sorted(current_groups.keys() | baseline_groups.keys()):
        current_paths = current_groups.get(integration, set())
        baseline_paths = baseline_groups.get(integration, set())
        compared_paths = current_paths | baseline_paths
        if current_paths == baseline_paths and all(
            current_assets.get(path) == baseline_assets.get(path)
            for path in compared_paths
        ):
            continue
        if not current_paths:
            raise DeltaWorkflowError(
                f"managed integration {integration} was removed without a current "
                "migration-version owner"
            )

        current_versions = _integration_marker_versions(
            integration, current_paths, current_assets
        )
        if len(current_versions) != 1:
            raise DeltaWorkflowError(
                f"changed managed integration {integration} must have one embedded "
                "HERDR_INTEGRATION_VERSION marker"
            )
        current_version = next(iter(current_versions))

        baseline_versions = _integration_marker_versions(
            integration, baseline_paths, baseline_assets
        )
        if baseline_paths and len(baseline_versions) != 1:
            raise DeltaWorkflowError(
                f"baseline managed integration {integration} must have one embedded "
                "HERDR_INTEGRATION_VERSION marker"
            )
        baseline_version = next(iter(baseline_versions), 0)
        if current_version <= baseline_version:
            raise DeltaWorkflowError(
                f"changed managed integration {integration} must advance beyond baseline: "
                f"baseline {baseline_version}, current {current_version}"
            )

        constant_name = re.sub(r"[^A-Za-z0-9]+", "_", integration).upper()
        constant_match = re.search(
            rf"\bconst\s+{re.escape(constant_name)}_INTEGRATION_VERSION\s*:\s*u32\s*=\s*([0-9]+)\s*;",
            current_module,
        )
        if constant_match is None:
            raise DeltaWorkflowError(
                f"changed managed integration {integration} has no matching "
                f"{constant_name}_INTEGRATION_VERSION constant"
            )
        constant_version = int(constant_match.group(1))
        if constant_version != current_version:
            raise DeltaWorkflowError(
                f"changed managed integration {integration} marker {current_version} "
                f"does not match Rust constant {constant_version}"
            )
        changed.append(integration)
    return tuple(changed)


def _tree_text(project_root: Path, tree: str, path: str) -> str | None:
    result = _run_git(project_root, ["show", f"{tree}:{path}"], check=False)
    return result.stdout if result.returncode == 0 else None


def validate_changed_integration_asset_versions(
    source_worktree: Path,
    project_root: Path = PROJECT_ROOT,
) -> tuple[str, ...]:
    """Compare embedded integration assets with the accepted checked-in queue."""

    project_root = project_root.resolve()
    source_worktree = source_worktree.resolve()
    replay = replay_delta_tree(project_root)
    module_path = "src/integration/mod.rs"
    try:
        current_module = (source_worktree / module_path).read_text(encoding="utf-8")
    except OSError as error:
        raise DeltaWorkflowError(
            f"could not read managed integration registry {source_worktree / module_path}: {error}"
        ) from error
    baseline_module = _tree_text(project_root, replay.tree, module_path)
    if baseline_module is None:
        raise DeltaWorkflowError(
            f"accepted delta tree {replay.tree} has no {module_path}"
        )

    current_groups = _included_integration_assets(current_module)
    baseline_groups = _included_integration_assets(baseline_module)
    paths = set().union(*current_groups.values(), *baseline_groups.values())
    current_assets: dict[str, str | None] = {}
    baseline_assets: dict[str, str | None] = {}
    for path in paths:
        current_path = source_worktree / path
        try:
            current_assets[path] = (
                current_path.read_text(encoding="utf-8")
                if current_path.is_file()
                else None
            )
        except OSError as error:
            raise DeltaWorkflowError(
                f"could not read managed integration asset {current_path}: {error}"
            ) from error
        baseline_assets[path] = _tree_text(project_root, replay.tree, path)
    return validate_integration_asset_version_changes(
        current_module,
        current_assets,
        baseline_module,
        baseline_assets,
    )


@contextmanager
def _prefix_replay_directory(work_dir: Path | None = None):
    if work_dir is not None:
        if not work_dir.is_absolute():
            raise DeltaWorkflowError("--work-dir must be an absolute caller-owned path")
        for part in (work_dir, *work_dir.parents):
            if part.is_symlink() or part.is_junction():
                raise DeltaWorkflowError("--work-dir must not traverse a link or junction")
        path = work_dir.resolve()
        if not path.parent.is_dir():
            raise DeltaWorkflowError("--work-dir parent must already exist")
        path.mkdir(exist_ok=True)
        yield path
        return
    path = Path(tempfile.mkdtemp(prefix="herdr-delta-prefixes-"))
    try:
        yield path
    finally:
        _remove_prefix_replay(path)


def _remove_prefix_replay(path: Path) -> None:
    # Zig dependency paths exceed MAX_PATH. Cleanup must address those same files.
    cleanup_path = f"\\\\?\\{path.resolve()}" if os.name == "nt" else str(path)

    def remove_readonly(function, name, error):
        if not isinstance(error, PermissionError):
            raise error
        os.chmod(name, stat.S_IWRITE)
        function(name)

    shutil.rmtree(cleanup_path, onexc=remove_readonly)


def compile_delta_prefixes(
    project_root: Path = PROJECT_ROOT,
    *,
    check_command: Sequence[str] = (
        "cargo", "check", "--locked", "--bins", "--target", "x86_64-pc-windows-msvc",
    ),
    target_dir: Path | None = None,
    work_dir: Path | None = None,
) -> tuple[str, ...]:
    """Replay and compile each ordered mailbox prefix for an explicit refresh."""

    project_root = project_root.resolve()
    if not check_command:
        raise DeltaWorkflowError("prefix compile command must not be empty")
    if work_dir is not None and target_dir is not None:
        raise DeltaWorkflowError("use --work-dir or --target-dir, not both")
    if work_dir is not None and (
        work_dir.resolve().is_relative_to(project_root)
        or project_root.is_relative_to(work_dir.resolve())
    ):
        raise DeltaWorkflowError("--work-dir must be separate from the control checkout")
    base = _read_base(project_root)
    mailboxes = _read_series(project_root)
    replay_environment = {
        "GIT_COMMITTER_NAME": "Herdr Extended replay",
        "GIT_COMMITTER_EMAIL": "41898282+github-actions[bot]@users.noreply.github.com",
    }
    with _prefix_replay_directory(work_dir) as temporary:
        checkout = temporary / "source"
        if not any(temporary.iterdir()):
            _run_git(
                project_root,
                ["clone", "--shared", "--no-checkout", str(project_root), str(checkout)],
            )
            _run_git(project_root, ["update-ref", "refs/herdr-prefix-base", base], cwd=checkout)
        else:
            if any(path.name not in {"source", "target"} for path in temporary.iterdir()):
                raise DeltaWorkflowError("prefix workspace contains foreign entries; preserve it")
            if not (checkout / ".git").is_dir() or any(
                path.is_symlink() or path.is_junction()
                for path in (checkout, checkout / ".git", temporary / "target")
            ):
                raise DeltaWorkflowError("prefix workspace is not an owned standalone checkout")
            origin = _run_git(project_root, ["remote", "get-url", "origin"], cwd=checkout).stdout.strip()
            saved_base = _run_git(project_root, ["rev-parse", "refs/herdr-prefix-base"], cwd=checkout).stdout.strip()
            if origin != str(project_root) or saved_base != base:
                raise DeltaWorkflowError("prefix workspace repository or BASE differs; preserve it")
            if _run_git(project_root, ["status", "--porcelain", "--untracked-files=all"], cwd=checkout).stdout.strip():
                raise DeltaWorkflowError("prefix checkout has changes; preserve and inspect it")
            if any((checkout / ".git" / marker).exists() for marker in (
                "rebase-apply", "rebase-merge", "MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD",
            )):
                raise DeltaWorkflowError("prefix replay is unfinished; preserve and inspect it")
        # Never force/reset/clean: Git refuses to overwrite changes or unknown files.
        _run_git(project_root, ["checkout", "--no-overwrite-ignore", "--detach", base], cwd=checkout)
        environment = os.environ.copy()
        environment.update(
            {
                "CARGO_BUILD_JOBS": str(os.cpu_count() or 1),
                "CARGO_INCREMENTAL": "0",
                "CARGO_TARGET_DIR": str(
                    target_dir.resolve() if target_dir else temporary / "target"
                ),
                "GIT_TERMINAL_PROMPT": "0",
            }
        )
        for prefix, mailbox in enumerate(mailboxes, start=1):
            if _run_git(project_root, ["status", "--porcelain", "--untracked-files=all"], cwd=checkout).stdout.strip():
                raise DeltaWorkflowError("prefix checkout has changes; preserve and inspect it")
            _run_git(
                project_root,
                ["am", "--3way", str(project_root / "patches" / "delta" / mailbox)],
                cwd=checkout,
                environment=replay_environment,
            )
            environment["HERDR_DELTA_PREFIX"] = str(prefix)
            environment["HERDR_DELTA_MAILBOX"] = mailbox
            tree = _run_git(project_root, ["rev-parse", "HEAD^{tree}"], cwd=checkout).stdout.strip()
            environment["HERDR_DELTA_TREE"] = tree
            print(f"compiling-prefix: {prefix}/{len(mailboxes)} {mailbox} tree={tree}", flush=True)
            compile_started = time.monotonic()
            try:
                result = subprocess.run(
                    list(check_command),
                    cwd=checkout,
                    env=environment,
                    capture_output=True,
                    text=True,
                    encoding="utf-8",
                    errors="replace",
                    check=False,
                    timeout=PREFIX_COMPILE_TIMEOUT_SECONDS,
                )
            except (OSError, subprocess.TimeoutExpired) as error:
                raise DeltaWorkflowError(
                    f"could not compile delta prefix through {mailbox}: {error}"
                ) from error
            if result.returncode != 0:
                detail = result.stderr.strip() or result.stdout.strip() or "no diagnostic"
                raise DeltaWorkflowError(
                    f"delta prefix through {mailbox} failed to compile: {detail}"
                )
            if _run_git(project_root, ["status", "--porcelain", "--untracked-files=all"], cwd=checkout).stdout.strip():
                raise DeltaWorkflowError("prefix check changed source; preserve and inspect it")
            print(
                f"compiled-prefix: {prefix}/{len(mailboxes)} {mailbox} "
                f"elapsed_seconds={time.monotonic() - compile_started:.3f}",
                flush=True,
            )
    return mailboxes


def issue_reference_report(
    project_root: Path = PROJECT_ROOT, *, ledger: Path | None = None,
) -> list[dict[str, object]]:
    """Inventory mailbox provenance, never query upstream or print ledger drafts."""
    captured: dict[str, dict[str, str]] = {}
    if ledger is not None:
        try:
            text = ledger.read_text(encoding="utf-8")
        except OSError as error:
            raise DeltaWorkflowError(f"could not read explicit ledger: {error}") from error
        key = None
        for line in text.splitlines():
            if line.startswith("### "):
                heading = line[4:].strip()
                key = heading if ISSUE_RE.fullmatch(heading) else None
                if key is not None:
                    captured.setdefault(key, {})
            elif line.startswith("#"):
                key = None
            elif key is not None:
                for label, field in (("Title", "title"), ("Local outcome", "behavior")):
                    if line.startswith(f"- {label}: "):
                        captured[key][field] = line[len(label) + 4:].strip()

    mailboxes = _read_series(project_root)
    rows: list[dict[str, object]] = []
    for position, mailbox in enumerate(mailboxes, start=1):
        path = project_root / "patches" / "delta" / mailbox
        metadata = _read_mailbox_metadata(path, position, len(mailboxes))
        content = path.read_bytes()
        text = content.decode("utf-8").replace("\r\n", "\n")
        source = re.match(r"From ([0-9a-f]{40}) ", text)
        owners: set[str] = set()
        for line in text.splitlines():
            if line.startswith(("--- ", "+++ ")):
                value = line[4:]
                if value.startswith('"'):
                    try:
                        value = ast.literal_eval("b" + value).decode("utf-8")
                    except (ValueError, SyntaxError, UnicodeError, AttributeError):
                        raise DeltaWorkflowError(f"invalid quoted patch path in {mailbox}")
                if value.startswith(("a/", "b/")):
                    owners.add(value[2:])
        # References in changed source are useful too; duplicate mentions are one row.
        references = sorted(set(ISSUE_RE.findall(metadata.commit_message + "\n" + text.split("\n---\n", 1)[-1])))
        for reference in references or [None]:
            fields = captured.get(reference, {})
            rows.append({
                "reference": reference,
                "mailbox": f"patches/delta/{mailbox}",
                "subject": metadata.commit_message.splitlines()[0],
                "path_owners": sorted(owners),
                "mailbox_sha256": hashlib.sha256(content).hexdigest(),
                "source_commit": source.group(1) if source else None,
                "title": fields.get("title"),
                "behavior": fields.get("behavior"),
            })
    return rows


def refresh_delta(
    base: str,
    head: str,
    expected_tree: str,
    dropped: Sequence[str],
    project_root: Path = PROJECT_ROOT,
) -> ReplayResult:
    """Publish a reviewed logical stable-refresh stack, never the merged source history."""
    project_root = project_root.resolve()
    _require_clean_delta(project_root)
    _require_tree_object(project_root, expected_tree, "--expected-tree")
    for commit in (base, head):
        if BASE_RE.fullmatch(commit) is None:
            raise DeltaWorkflowError("refresh requires full base and head commit IDs")
        _run_git(project_root, ["cat-file", "-e", f"{commit}^{{commit}}"])
    entries = _read_series(project_root)
    if len(set(dropped)) != len(dropped) or not set(dropped).issubset(entries):
        raise DeltaWorkflowError("dropped mailboxes must be unique current series entries")
    retained = tuple(entry for entry in entries if entry not in dropped)
    commits = _run_git(
        project_root, ["rev-list", "--reverse", f"{base}..{head}"]
    ).stdout.splitlines()
    if not retained or len(commits) != len(retained):
        raise DeltaWorkflowError(
            "refresh requires exactly one logical commit per retained mailbox"
        )
    actual_tree = _run_git(project_root, ["rev-parse", f"{head}^{{tree}}"]).stdout.strip()
    if actual_tree != expected_tree:
        raise DeltaWorkflowError(
            f"logical source tree differs: expected {expected_tree}, found {actual_tree}"
        )
    delta_root = project_root / "patches" / "delta"
    originals = {
        delta_root / entry: (delta_root / entry).read_bytes()
        for entry in (*entries, "BASE", "series")
    }
    candidates: dict[Path, bytes] = {}
    parent = base
    for position, (entry, commit) in enumerate(zip(retained, commits), start=1):
        parents = _run_git(project_root, ["show", "-s", "--format=%P", commit]).stdout.strip()
        if parents != parent:
            raise DeltaWorkflowError(f"logical refresh stack is not linear at {entry}")
        changed = _run_git(
            project_root, ["diff", "--name-only", "--no-renames", parent, commit]
        ).stdout.splitlines()
        if not changed or any(
            path in CONTROL_PATHS or path.startswith(CONTROL_PATH_PREFIXES)
            for path in changed
        ):
            raise DeltaWorkflowError(f"empty or control-plane responsibility in {entry}")
        metadata = _read_mailbox_metadata(
            delta_root / entry, entries.index(entry) + 1, len(entries)
        )
        current_metadata = _read_commit_metadata(project_root, commit)
        # Git normalizes the author date's spelling, not its instant.
        if (metadata.author_name, metadata.author_email, metadata.commit_message) != (
            current_metadata.author_name,
            current_metadata.author_email,
            current_metadata.commit_message,
        ) or parsedate_to_datetime(metadata.author_date) != datetime.fromisoformat(
            current_metadata.author_date
        ):
            raise DeltaWorkflowError(f"retained mailbox metadata differs in {entry}")
        candidates[delta_root / entry] = _run_git(project_root, [
            "format-patch", "--stdout", "--full-index", "--binary",
            f"--subject-prefix=PATCH {position}/{len(retained)}", "-1", commit,
        ]).stdout.encode("utf-8")
        parent = commit
    candidates[delta_root / "series"] = ("\n".join(retained) + "\n").encode()
    candidates[delta_root / "BASE"] = (base + "\n").encode()

    # Candidate inputs stay private until the complete replay proves exact identity.
    with tempfile.TemporaryDirectory(prefix="herdr-delta-refresh-") as temporary:
        staged_delta = Path(temporary)
        for path, content in candidates.items():
            (staged_delta / path.name).write_bytes(content)
        tree = _tree_after_patches(
            project_root, base, [staged_delta / entry for entry in retained]
        )
        if tree != expected_tree:
            raise DeltaWorkflowError(
                f"staged refresh replay differs: expected {expected_tree}, found {tree}"
            )
        _run_git(project_root, ["diff", "--check", base, tree])
        result = ReplayResult(base, retained, tree)
        for path, content in originals.items():
            if path.read_bytes() != content:
                raise DeltaWorkflowError(f"delta changed during refresh: {path.name}")
        try:
            for path, content in candidates.items():
                _atomic_replace(path, content)
            for entry in dropped:
                (delta_root / entry).unlink()
            verify_replay_tree(expected_tree, project_root)
        except (OSError, DeltaWorkflowError):
            for path, content in originals.items():
                _atomic_replace(path, content)
            raise
    return result


def _require_tree_object(project_root: Path, tree: str, label: str) -> None:
    if BASE_RE.fullmatch(tree) is None:
        raise DeltaWorkflowError(f"{label} must be one full lowercase Git tree ID")
    object_type = _run_git(project_root, ["cat-file", "-t", tree]).stdout.strip()
    if object_type != "tree":
        raise DeltaWorkflowError(f"{label} identifies a {object_type}, not a tree")


def verify_replay_tree(
    expected_tree: str | None,
    project_root: Path = PROJECT_ROOT,
) -> ReplayResult:
    """Replay the queue and optionally require one exact, previously tested tree."""

    project_root = project_root.resolve()
    if expected_tree is not None:
        _require_tree_object(project_root, expected_tree, "--expected-tree")

    result = replay_delta_tree(project_root)
    if expected_tree is not None and result.tree != expected_tree:
        difference = _run_git(
            project_root,
            ["diff", "--stat", expected_tree, result.tree],
            check=False,
        ).stdout.strip()
        detail = f"\n{difference}" if difference else ""
        raise DeltaWorkflowError(
            "replayed delta tree does not match the tested source tree: "
            f"expected {expected_tree}, found {result.tree}{detail}"
        )
    return result


def _require_clean_delta(project_root: Path) -> None:
    status = _run_git(
        project_root,
        [
            "status",
            "--porcelain=v1",
            "--untracked-files=no",
            "--",
            "patches/delta/BASE",
            "patches/delta/series",
            "patches/delta/*.patch",
        ],
    ).stdout.strip()
    if status:
        raise DeltaWorkflowError(
            "delta inputs must be clean before this operation:\n"
            f"{status}"
        )


def _require_control_master(project_root: Path) -> None:
    current_branch = _run_git(
        project_root, ["symbolic-ref", "--short", "HEAD"]
    ).stdout.strip()
    if current_branch != "master":
        raise DeltaWorkflowError(
            f"control checkout must be on master, found {current_branch!r}"
        )


def _normalized_git_path(path: str) -> str:
    return os.path.normcase(os.path.realpath(path))


def _require_clean_delta_worktree(
    project_root: Path,
    worktree: Path,
) -> tuple[Path, str]:
    if not worktree.is_absolute():
        raise DeltaWorkflowError("--worktree must be an absolute path")
    worktree = worktree.resolve()
    if not worktree.is_dir():
        raise DeltaWorkflowError(f"source worktree does not exist: {worktree}")

    source_root = Path(
        _run_git(worktree, ["rev-parse", "--show-toplevel"], cwd=worktree)
        .stdout.strip()
    ).resolve()
    if source_root != worktree:
        raise DeltaWorkflowError(
            f"--worktree must identify its Git root, found {source_root}"
        )

    control_common = _run_git(
        project_root,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ).stdout.strip()
    source_common = _run_git(
        worktree,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd=worktree,
    ).stdout.strip()
    if _normalized_git_path(control_common) != _normalized_git_path(source_common):
        raise DeltaWorkflowError("source worktree belongs to another Git repository")

    branch = _run_git(
        worktree, ["symbolic-ref", "--short", "HEAD"], cwd=worktree
    ).stdout.strip()
    if branch != DEVELOPMENT_BRANCH and not branch.startswith("agent/delta-"):
        raise DeltaWorkflowError(
            "source worktree must use the cumulative development branch or an "
            f"agent/delta-* topic branch, found {branch!r}"
        )

    status = _run_git(
        worktree,
        ["status", "--porcelain=v1", "--untracked-files=all"],
        cwd=worktree,
    ).stdout.strip()
    if status:
        raise DeltaWorkflowError(f"source worktree must be clean:\n{status}")

    for marker in (
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ):
        marker_path = _run_git(
            worktree,
            ["rev-parse", "--path-format=absolute", "--git-path", marker],
            cwd=worktree,
        ).stdout.strip()
        if Path(marker_path).exists():
            raise DeltaWorkflowError(
                f"source worktree has an in-progress Git operation: {marker}"
            )
    return worktree, branch


def _first_parent_commit_with_tree(
    worktree: Path, base: str, tree: str
) -> str | None:
    first_parent = _run_git(
        worktree,
        ["log", "--first-parent", "--format=%H %T", f"{base}..HEAD"],
        cwd=worktree,
    ).stdout.splitlines()
    return next(
        (
            commit
            for line in first_parent
            for commit, commit_tree in [line.split()]
            if commit_tree == tree
        ),
        None,
    )


def _require_source_worktree(
    project_root: Path,
    worktree: Path,
    expected_tree: str,
    replay: ReplayResult,
) -> tuple[str, str]:
    worktree, _ = _require_clean_delta_worktree(project_root, worktree)

    ancestor = _run_git(
        worktree,
        ["merge-base", "--is-ancestor", replay.base, "HEAD"],
        cwd=worktree,
        check=False,
    )
    if ancestor.returncode != 0:
        raise DeltaWorkflowError("recorded BASE is not an ancestor of source HEAD")
    source_head = _run_git(worktree, ["rev-parse", "HEAD"], cwd=worktree).stdout.strip()
    queue_head = _first_parent_commit_with_tree(worktree, replay.base, replay.tree)
    if queue_head is None:
        raise DeltaWorkflowError(
            "source worktree was not started from the current checked-in queue"
        )
    wip_commits = _run_git(
        worktree,
        ["rev-list", f"{queue_head}..{source_head}"],
        cwd=worktree,
    ).stdout.strip()
    if not wip_commits:
        raise DeltaWorkflowError("source worktree has no committed WIP change")

    source_tree = _run_git(
        worktree, ["rev-parse", "HEAD^{tree}"], cwd=worktree
    ).stdout.strip()
    if source_tree != expected_tree:
        raise DeltaWorkflowError(
            "source worktree changed after its tested tree was recorded: "
            f"expected {expected_tree}, found {source_tree}"
        )
    if source_tree == replay.tree:
        raise DeltaWorkflowError("source worktree has no net tree change to finalize")
    return source_head, source_tree


def _read_mailbox_metadata(
    path: Path,
    position: int,
    mailbox_count: int,
) -> MailboxMetadata:
    try:
        message = BytesParser(policy=policy.default).parsebytes(path.read_bytes())
    except (OSError, UnicodeError) as error:
        raise DeltaWorkflowError(f"could not parse mailbox {path}: {error}") from error

    subject = str(message.get("Subject", "")).strip()
    expected_prefix = f"[PATCH {position}/{mailbox_count}] "
    if not subject.startswith(expected_prefix):
        raise DeltaWorkflowError(
            f"{path} subject must start with {expected_prefix!r}, found {subject!r}"
        )
    commit_subject = subject[len(expected_prefix) :].strip()
    author_name, author_email = parseaddr(str(message.get("From", "")))
    author_date = str(message.get("Date", "")).strip()
    if not commit_subject or not author_name or not author_email or not author_date:
        raise DeltaWorkflowError(f"{path} has incomplete commit metadata")

    payload = message.get_payload(decode=True)
    if not isinstance(payload, bytes):
        raise DeltaWorkflowError(f"{path} must contain one plain-text patch payload")
    text = payload.decode("utf-8", errors="strict").replace("\r\n", "\n")
    lines = text.splitlines()
    try:
        separator_index = lines.index("---")
    except ValueError:
        raise DeltaWorkflowError(f"{path} is missing the format-patch separator")
    body = "\n".join(lines[:separator_index]).strip("\n")
    commit_message = commit_subject
    if body:
        commit_message = f"{commit_message}\n\n{body}"
    return MailboxMetadata(
        author_name=author_name,
        author_email=author_email,
        author_date=author_date,
        commit_message=f"{commit_message}\n",
    )


def _read_commit_metadata(worktree: Path, commit: str) -> MailboxMetadata:
    fields = _run_git(
        worktree,
        ["show", "-s", "--format=%an%x00%ae%x00%aI%x00%B", commit],
        cwd=worktree,
    ).stdout.split("\0", 3)
    if len(fields) != 4:
        raise DeltaWorkflowError(f"could not read source commit metadata from {commit}")
    author_name, author_email, author_date, commit_message = fields
    commit_message = commit_message.rstrip("\n")
    if not author_name or not author_email or not author_date or not commit_message:
        raise DeltaWorkflowError(f"source commit {commit} has incomplete metadata")
    return MailboxMetadata(
        author_name=author_name,
        author_email=author_email,
        author_date=author_date,
        commit_message=f"{commit_message}\n",
    )


def _commit_tree(
    project_root: Path,
    tree: str,
    parent: str,
    message: str,
    environment: dict[str, str],
) -> str:
    return _run_git(
        project_root,
        ["commit-tree", tree, "-p", parent],
        environment=environment,
        input_text=message,
    ).stdout.strip()


def _candidate_mailbox(
    project_root: Path,
    prefix_tree: str,
    owner_tree: str,
    metadata: MailboxMetadata,
    position: int,
    mailbox_count: int,
) -> str:
    replay_identity = {
        "GIT_AUTHOR_NAME": "Herdr Extended replay",
        "GIT_AUTHOR_EMAIL": "41898282+github-actions[bot]@users.noreply.github.com",
        "GIT_COMMITTER_NAME": "Herdr Extended replay",
        "GIT_COMMITTER_EMAIL": "41898282+github-actions[bot]@users.noreply.github.com",
    }
    prefix_commit = _commit_tree(
        project_root,
        prefix_tree,
        _read_base(project_root),
        "Herdr Extended delta prefix\n",
        replay_identity,
    )
    owner_environment = {
        **replay_identity,
        "GIT_AUTHOR_NAME": metadata.author_name,
        "GIT_AUTHOR_EMAIL": metadata.author_email,
        "GIT_AUTHOR_DATE": metadata.author_date,
    }
    owner_commit = _commit_tree(
        project_root,
        owner_tree,
        prefix_commit,
        metadata.commit_message,
        owner_environment,
    )
    return _run_git(
        project_root,
        [
            "format-patch",
            "--stdout",
            "--full-index",
            "--binary",
            f"--subject-prefix=PATCH {position}/{mailbox_count}",
            "-1",
            owner_commit,
        ],
    ).stdout


def _atomic_replace(path: Path, content: bytes) -> None:
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary_path = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary_path, path)
    finally:
        temporary_path.unlink(missing_ok=True)


def _renumber_mailbox_subject(
    path: Path,
    content: bytes,
    position: int,
    old_count: int,
    new_count: int,
) -> bytes:
    old = f"Subject: [PATCH {position}/{old_count}] ".encode()
    if content.count(old) != 1:
        raise DeltaWorkflowError(
            f"{path} must contain exactly one {old.decode()!r} header"
        )
    return content.replace(
        old,
        f"Subject: [PATCH {position}/{new_count}] ".encode(),
        1,
    )


def _finalize_new_delta_mailbox(
    worktree: Path,
    mailbox: str,
    expected_tree: str,
    replay: ReplayResult,
    project_root: Path,
) -> FinalizeResult:
    if PATCH_RE.fullmatch(mailbox) is None:
        raise DeltaWorkflowError(f"invalid new mailbox name: {mailbox!r}")
    if mailbox in replay.mailboxes:
        raise DeltaWorkflowError(f"new mailbox is already listed in series: {mailbox}")

    delta_root = project_root / "patches" / "delta"
    mailbox_path = delta_root / mailbox
    if mailbox_path.exists():
        raise DeltaWorkflowError(f"new mailbox path already exists: {mailbox_path}")
    new_number = int(mailbox[:4])
    if new_number <= max(int(entry[:4]) for entry in replay.mailboxes):
        raise DeltaWorkflowError("a new mailbox must append a higher logical slot")

    source_head, source_tree = _require_source_worktree(
        project_root,
        worktree,
        expected_tree,
        replay,
    )
    queue_head = _first_parent_commit_with_tree(worktree, replay.base, replay.tree)
    if queue_head is None:
        raise DeltaWorkflowError(
            "new mailbox source was not started from the current checked-in queue"
        )
    merges = _run_git(
        worktree,
        ["rev-list", "--merges", f"{queue_head}..{source_head}"],
        cwd=worktree,
    ).stdout.strip()
    if merges:
        raise DeltaWorkflowError("new mailbox WIP commits must be linear")
    wip_commits = _run_git(
        worktree,
        ["rev-list", "--first-parent", "--reverse", f"{queue_head}..{source_head}"],
        cwd=worktree,
    ).stdout.splitlines()
    if not wip_commits:
        raise DeltaWorkflowError("new mailbox source must contain committed WIP changes")

    old_count = len(replay.mailboxes)
    new_count = old_count + 1
    metadata = _read_commit_metadata(worktree, wip_commits[0])
    candidate = _candidate_mailbox(
        project_root,
        replay.tree,
        expected_tree,
        metadata,
        new_count,
        new_count,
    ).encode("utf-8")

    original_mailboxes: dict[Path, bytes] = {}
    renumbered_mailboxes: dict[Path, bytes] = {}
    for position, entry in enumerate(replay.mailboxes, start=1):
        path = delta_root / entry
        original = path.read_bytes()
        _read_mailbox_metadata(path, position, old_count)
        original_mailboxes[path] = original
        renumbered_mailboxes[path] = _renumber_mailbox_subject(
            path,
            original,
            position,
            old_count,
            new_count,
        )

    series_path = delta_root / "series"
    original_series = series_path.read_bytes()
    separator = b"" if original_series.endswith(b"\n") else b"\n"
    candidate_series = original_series + separator + mailbox.encode() + b"\n"

    with tempfile.TemporaryDirectory(prefix="herdr-delta-finalize-new-") as temp_dir:
        candidate_paths: list[Path] = []
        for path in original_mailboxes:
            candidate_path = Path(temp_dir) / path.name
            candidate_path.write_bytes(renumbered_mailboxes[path])
            candidate_paths.append(candidate_path)
        candidate_path = Path(temp_dir) / mailbox
        candidate_path.write_bytes(candidate)
        candidate_paths.append(candidate_path)
        candidate_tree = _tree_after_patches(
            project_root,
            replay.base,
            candidate_paths,
        )
    if candidate_tree != expected_tree:
        difference = _run_git(
            project_root,
            ["diff", "--stat", expected_tree, candidate_tree],
            check=False,
        ).stdout.strip()
        detail = f"\n{difference}" if difference else ""
        raise DeltaWorkflowError(
            "new mailbox does not reproduce the tested source tree: "
            f"expected {expected_tree}, found {candidate_tree}{detail}"
        )
    _run_git(project_root, ["diff", "--check", replay.base, candidate_tree])

    for path, original in original_mailboxes.items():
        if path.read_bytes() != original:
            raise DeltaWorkflowError(f"mailbox changed concurrently: {path.name}")
    if series_path.read_bytes() != original_series or mailbox_path.exists():
        raise DeltaWorkflowError("delta series changed concurrently")

    try:
        for path, content in renumbered_mailboxes.items():
            _atomic_replace(path, content)
        _atomic_replace(mailbox_path, candidate)
        _atomic_replace(series_path, candidate_series)
        verified = verify_replay_tree(expected_tree, project_root)
    except (DeltaWorkflowError, OSError) as error:
        _atomic_replace(series_path, original_series)
        for path, content in original_mailboxes.items():
            _atomic_replace(path, content)
        mailbox_path.unlink(missing_ok=True)
        raise DeltaWorkflowError(
            f"restored delta after new mailbox finalization failed: {error}"
        ) from error

    return FinalizeResult(
        mailbox=mailbox,
        source_head=source_head,
        source_tree=source_tree,
        replay_tree=verified.tree,
    )


def finalize_delta_mailbox(
    worktree: Path,
    mailbox: str,
    expected_tree: str,
    project_root: Path = PROJECT_ROOT,
    *,
    new_mailbox: bool = False,
) -> FinalizeResult:
    """Fold one tested WIP tree into one mailbox and prove exact queue replay."""

    project_root = project_root.resolve()
    _require_control_master(project_root)
    _require_clean_delta(project_root)
    _require_tree_object(project_root, expected_tree, "--expected-tree")

    replay = replay_delta_tree(project_root)
    if new_mailbox:
        return _finalize_new_delta_mailbox(
            worktree,
            mailbox,
            expected_tree,
            replay,
            project_root,
        )
    if mailbox not in replay.mailboxes:
        raise DeltaWorkflowError(f"--mailbox is not listed in series: {mailbox}")
    source_head, source_tree = _require_source_worktree(
        project_root,
        worktree,
        expected_tree,
        replay,
    )

    delta_root = project_root / "patches" / "delta"
    position = replay.mailboxes.index(mailbox) + 1
    mailbox_path = delta_root / mailbox
    original_mailbox = mailbox_path.read_bytes()
    metadata = _read_mailbox_metadata(
        mailbox_path,
        position,
        len(replay.mailboxes),
    )

    prefix_paths = [
        delta_root / entry for entry in replay.mailboxes[: position - 1]
    ]
    later_paths = [
        delta_root / entry for entry in reversed(replay.mailboxes[position:])
    ]
    prefix_tree = _tree_after_patches(project_root, replay.base, prefix_paths)
    owner_tree = _tree_after_patches(
        project_root,
        expected_tree,
        later_paths,
        reverse=True,
    )
    unchanged = _run_git(
        project_root,
        ["diff", "--quiet", prefix_tree, owner_tree],
        check=False,
    )
    if unchanged.returncode == 0:
        raise DeltaWorkflowError(f"finalized mailbox would be empty: {mailbox}")
    if unchanged.returncode not in (0, 1):
        raise DeltaWorkflowError("could not compare finalized mailbox trees")

    candidate = _candidate_mailbox(
        project_root,
        prefix_tree,
        owner_tree,
        metadata,
        position,
        len(replay.mailboxes),
    ).encode("utf-8")

    with tempfile.TemporaryDirectory(prefix="herdr-delta-finalize-") as temp_dir:
        candidate_path = Path(temp_dir) / mailbox
        candidate_path.write_bytes(candidate)
        candidate_paths = [
            candidate_path if entry == mailbox else delta_root / entry
            for entry in replay.mailboxes
        ]
        candidate_tree = _tree_after_patches(
            project_root,
            replay.base,
            candidate_paths,
        )
    if candidate_tree != expected_tree:
        difference = _run_git(
            project_root,
            ["diff", "--stat", expected_tree, candidate_tree],
            check=False,
        ).stdout.strip()
        detail = f"\n{difference}" if difference else ""
        raise DeltaWorkflowError(
            "finalized queue does not reproduce the tested source tree: "
            f"expected {expected_tree}, found {candidate_tree}{detail}"
        )
    _run_git(project_root, ["diff", "--check", replay.base, candidate_tree])

    if mailbox_path.read_bytes() != original_mailbox:
        raise DeltaWorkflowError(f"mailbox changed concurrently: {mailbox}")
    _atomic_replace(mailbox_path, candidate)
    try:
        verified = verify_replay_tree(expected_tree, project_root)
    except DeltaWorkflowError as error:
        _atomic_replace(mailbox_path, original_mailbox)
        raise DeltaWorkflowError(
            f"restored {mailbox} after final on-disk verification failed: {error}"
        ) from error

    return FinalizeResult(
        mailbox=mailbox,
        source_head=source_head,
        source_tree=source_tree,
        replay_tree=verified.tree,
    )


def materialize_delta_worktree(
    worktree: Path,
    project_root: Path = PROJECT_ROOT,
) -> WorktreeResult:
    """Replay the queue into an existing clean task worktree at recorded BASE."""

    project_root = project_root.resolve()
    _require_control_master(project_root)
    _require_clean_delta(project_root)
    base = _read_base(project_root)
    mailboxes = _read_series(project_root)
    worktree, branch = _require_clean_delta_worktree(project_root, worktree)

    initial_head = _run_git(
        worktree, ["rev-parse", "HEAD"], cwd=worktree
    ).stdout.strip()
    if initial_head != base:
        raise DeltaWorkflowError(
            "source worktree must start at the exact commit recorded in BASE: "
            f"expected {base}, found {initial_head}"
        )

    replay_environment = {
        "GIT_COMMITTER_NAME": "Herdr Extended replay",
        "GIT_COMMITTER_EMAIL": "41898282+github-actions[bot]@users.noreply.github.com",
    }
    try:
        for mailbox in mailboxes:
            _run_git(
                project_root,
                [
                    "am",
                    "--3way",
                    str(project_root / "patches" / "delta" / mailbox),
                ],
                cwd=worktree,
                environment=replay_environment,
            )
        _run_git(worktree, ["diff", "--check", f"{base}..HEAD"], cwd=worktree)
    except DeltaWorkflowError as error:
        raise DeltaWorkflowError(
            f"delta replay stopped in {worktree}; preserve and inspect that worktree: {error}"
        ) from error

    head = _run_git(worktree, ["rev-parse", "HEAD"], cwd=worktree).stdout.strip()
    tree = _run_git(
        worktree, ["rev-parse", "HEAD^{tree}"], cwd=worktree
    ).stdout.strip()
    return WorktreeResult(
        branch=branch,
        path=worktree,
        base=base,
        head=head,
        tree=tree,
        mailbox_count=len(mailboxes),
    )


def integrate_development_worktree(
    worktree: Path,
    base: str,
    head: str,
    project_root: Path = PROJECT_ROOT,
) -> DevelopmentResult:
    """Fast-forward the linked source target inside the caller's resource lock."""

    _require_control_master(project_root)
    worktree, branch = _require_clean_delta_worktree(project_root, worktree)
    if branch != DEVELOPMENT_BRANCH:
        raise DeltaWorkflowError(f"integration target must use {DEVELOPMENT_BRANCH}")
    if BASE_RE.fullmatch(base) is None or BASE_RE.fullmatch(head) is None:
        raise DeltaWorkflowError("integration requires full base and head commit IDs")
    current = _run_git(worktree, ["rev-parse", "HEAD"]).stdout.strip()
    if current != base:
        raise DeltaWorkflowError(f"stale integration base: expected {base}, found {current}")
    _run_git(worktree, ["cat-file", "-e", f"{head}^{{commit}}"])
    _run_git(worktree, ["merge-base", "--is-ancestor", base, head])
    _run_git(worktree, ["merge", "--ff-only", head])
    actual = _run_git(worktree, ["rev-parse", "HEAD"]).stdout.strip()
    if actual != head:
        raise DeltaWorkflowError(f"integration head changed: expected {head}, found {actual}")
    tree = _run_git(worktree, ["rev-parse", "HEAD^{tree}"]).stdout.strip()
    return DevelopmentResult(path=worktree, head=actual, tree=tree)


def publish_development_worktree(
    worktree: Path,
    project_root: Path = PROJECT_ROOT,
) -> DevelopmentResult:
    """Push the one clean cumulative development source state."""

    project_root = project_root.resolve()
    _require_control_master(project_root)
    _require_clean_delta(project_root)
    replay = replay_delta_tree(project_root)
    worktree, branch = _require_clean_delta_worktree(project_root, worktree)
    if branch != DEVELOPMENT_BRANCH:
        raise DeltaWorkflowError(
            f"development worktree must use {DEVELOPMENT_BRANCH!r}, found {branch!r}"
        )
    ancestor = _run_git(
        worktree,
        ["merge-base", "--is-ancestor", replay.base, "HEAD"],
        cwd=worktree,
        check=False,
    )
    if ancestor.returncode != 0:
        raise DeltaWorkflowError("recorded BASE is not an ancestor of development HEAD")
    reachable_trees = _run_git(
        worktree,
        ["log", "--format=%T", f"{replay.base}..HEAD"],
        cwd=worktree,
    ).stdout.splitlines()
    if replay.tree not in reachable_trees:
        raise DeltaWorkflowError(
            "development history does not contain the current checked-in replay tree"
        )
    head = _run_git(worktree, ["rev-parse", "HEAD"], cwd=worktree).stdout.strip()
    unintegrated = unintegrated_topic_worktrees(project_root, head)
    if unintegrated:
        detail = ", ".join(
            f"{topic.branch} ({topic.path})" for topic in unintegrated
        )
        raise DeltaWorkflowError(
            f"unintegrated topic worktrees block development publication: {detail}"
        )
    _run_git(worktree, ["diff", "--check", f"{replay.base}..HEAD"], cwd=worktree)
    _run_git(
        worktree,
        ["push", "origin", f"HEAD:{DEVELOPMENT_REMOTE_REF}"],
        cwd=worktree,
    )
    tree = _run_git(
        worktree, ["rev-parse", "HEAD^{tree}"], cwd=worktree
    ).stdout.strip()
    return DevelopmentResult(path=worktree, head=head, tree=tree)


def start_delta_worktree(
    name: str,
    path: Path,
    project_root: Path = PROJECT_ROOT,
) -> WorktreeResult:
    """Create one task worktree and replay the queue into it exactly once."""

    project_root = project_root.resolve()
    if TASK_RE.fullmatch(name) is None:
        raise DeltaWorkflowError(
            "task name must use 1 to 40 lowercase letters, digits, or hyphens"
        )
    if not path.is_absolute():
        raise DeltaWorkflowError("worktree path must be absolute")
    path = path.resolve()
    try:
        path.relative_to(project_root)
    except ValueError:
        pass
    else:
        raise DeltaWorkflowError("worktree path must be outside the control checkout")
    if path.exists():
        raise DeltaWorkflowError(f"worktree path already exists: {path}")
    if not path.parent.is_dir():
        raise DeltaWorkflowError(
            f"worktree parent directory does not exist: {path.parent}"
        )

    _require_control_master(project_root)
    _require_clean_delta(project_root)

    base = _read_base(project_root)
    branch = DEVELOPMENT_BRANCH if name == "development" else f"agent/delta-{name}"
    _run_git(project_root, ["check-ref-format", "--branch", branch])
    if branch == DEVELOPMENT_BRANCH:
        remote_exists = _run_git(
            project_root,
            ["ls-remote", "--exit-code", "--heads", "origin", DEVELOPMENT_REMOTE_REF],
            check=False,
        )
        if remote_exists.returncode == 0:
            raise DeltaWorkflowError(
                "remote cumulative development branch already exists; create the "
                "local development branch from its exact fetched tip"
            )
        if remote_exists.returncode != 2:
            detail = remote_exists.stderr.strip() or "could not inspect remote branch"
            raise DeltaWorkflowError(detail)
    branch_exists = _run_git(
        project_root,
        ["show-ref", "--verify", "--quiet", f"refs/heads/{branch}"],
        check=False,
    )
    if branch_exists.returncode == 0:
        raise DeltaWorkflowError(f"worktree branch already exists: {branch}")
    if branch_exists.returncode not in (0, 1):
        detail = branch_exists.stderr.strip() or "could not inspect branch"
        raise DeltaWorkflowError(detail)

    _run_git(project_root, ["worktree", "add", "-b", branch, str(path), base])
    return materialize_delta_worktree(path, project_root)


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Materialize, finalize, and verify the Herdr Extended delta."
    )
    commands = parser.add_subparsers(dest="command", required=True)

    start = commands.add_parser(
        "start", help="create one task worktree and replay the queue once"
    )
    start.add_argument("--name", required=True, help="lowercase task slug")
    start.add_argument(
        "--path",
        required=True,
        type=Path,
        help="absolute path below an existing parent",
    )

    materialize = commands.add_parser(
        "materialize",
        help="replay the queue into an existing clean task worktree at BASE",
    )
    materialize.add_argument(
        "--worktree",
        required=True,
        type=Path,
        help="absolute path to an existing delta worktree at BASE",
    )

    integrate_development = commands.add_parser(
        "integrate-development",
        help="under the repository resource lock, fast-forward the linked source target",
    )
    integrate_development.add_argument("--worktree", required=True, type=Path)
    integrate_development.add_argument("--base", required=True)
    integrate_development.add_argument("--head", required=True)

    publish_development = commands.add_parser(
        "publish-development",
        help="push the clean cumulative development source state",
    )
    publish_development.add_argument(
        "--worktree",
        required=True,
        type=Path,
        help=f"absolute path to the shared {DEVELOPMENT_BRANCH} worktree",
    )

    prefixes = commands.add_parser(
        "compile-prefixes",
        help="refresh-only replay and compile of every ordered mailbox prefix",
    )
    prefixes.add_argument("--target-dir", type=Path, help="task-owned shared Cargo cache")
    prefixes.add_argument("--work-dir", type=Path, help="caller-owned checkout and compiler caches for one refresh")

    report = commands.add_parser("issue-report", help="read-only local mailbox issue inventory as JSON; null means unknown")
    report.add_argument("--ledger", type=Path, help="explicit private ledger; include only Title and Local outcome fields")

    refresh = commands.add_parser("refresh", help="verify and finalize an authorized stable-refresh logical stack")
    refresh.add_argument("--base", required=True)
    refresh.add_argument("--head", required=True)
    refresh.add_argument("--expected-tree", required=True)
    refresh.add_argument("--drop-mailbox", action="append", default=[])

    check = commands.add_parser(
        "check", help="replay into a temporary index without another checkout"
    )
    check.add_argument(
        "--expected-tree",
        help="exact tested Git tree ID that the checked-in queue must reproduce",
    )

    finalize = commands.add_parser(
        "finalize",
        help="fold one tested WIP tree into its owning mailbox",
    )
    finalize.add_argument(
        "--worktree",
        required=True,
        type=Path,
        help="absolute path to the clean replayed WIP worktree",
    )
    finalize.add_argument(
        "--mailbox",
        required=True,
        help="exact owning mailbox name from patches/delta/series",
    )
    finalize.add_argument(
        "--expected-tree",
        required=True,
        help="exact tested Git tree ID that final replay must reproduce",
    )
    finalize.add_argument(
        "--new-mailbox",
        action="store_true",
        help="append one higher-numbered mailbox from exactly one WIP commit",
    )
    return parser


def main(arguments: Sequence[str] | None = None) -> int:
    parser = _build_parser()
    options = parser.parse_args(arguments)
    try:
        if options.command == "start":
            result = start_delta_worktree(options.name, options.path)
            print(f"worktree: {result.path}")
            print(f"branch: {result.branch}")
            print(f"base: {result.base}")
            print(f"head: {result.head}")
            print(f"tree: {result.tree}")
            print(f"mailboxes: {result.mailbox_count}")
            return 0

        if options.command == "materialize":
            result = materialize_delta_worktree(options.worktree)
            print(f"worktree: {result.path}")
            print(f"branch: {result.branch}")
            print(f"base: {result.base}")
            print(f"head: {result.head}")
            print(f"tree: {result.tree}")
            print(f"mailboxes: {result.mailbox_count}")
            return 0

        if options.command == "finalize":
            result = finalize_delta_mailbox(
                options.worktree,
                options.mailbox,
                options.expected_tree,
                new_mailbox=options.new_mailbox,
            )
            print(f"mailbox: {result.mailbox}")
            print(f"source-head: {result.source_head}")
            print(f"source-tree: {result.source_tree}")
            print(f"replay-tree: {result.replay_tree}")
            print("mailbox-updated: yes")
            return 0

        if options.command == "integrate-development":
            result = integrate_development_worktree(
                options.worktree, options.base, options.head
            )
            print(f"worktree: {result.path}")
            print(f"head: {result.head}")
            print(f"tree: {result.tree}")
            print("development-integrated: yes")
            return 0

        if options.command == "publish-development":
            result = publish_development_worktree(options.worktree)
            print(f"worktree: {result.path}")
            print(f"head: {result.head}")
            print(f"tree: {result.tree}")
            print("development-pushed: yes")
            return 0

        if options.command == "compile-prefixes":
            mailboxes = compile_delta_prefixes(target_dir=options.target_dir, work_dir=options.work_dir)
            print(f"compiled-prefixes: {len(mailboxes)}")
            print(f"last-mailbox: {mailboxes[-1]}")
            return 0

        if options.command == "issue-report":
            print(json.dumps(issue_reference_report(ledger=options.ledger), indent=2, ensure_ascii=True))
            return 0

        if options.command == "refresh":
            result = refresh_delta(options.base, options.head, options.expected_tree, options.drop_mailbox)
            print(f"base: {result.base}")
            print(f"tree: {result.tree}")
            print(f"mailboxes: {len(result.mailboxes)}")
            print("refresh-finalized: yes")
            return 0

        result = verify_replay_tree(options.expected_tree)
        print(f"base: {result.base}")
        print(f"tree: {result.tree}")
        print(f"mailboxes: {len(result.mailboxes)}")
        if options.expected_tree is not None:
            print("tested-tree-match: yes")
        return 0
    except DeltaWorkflowError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
