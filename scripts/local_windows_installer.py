#!/usr/bin/env python3
"""Build and reuse one validated local Windows installer candidate."""

from __future__ import annotations

import argparse
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
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Sequence

try:
    from scripts.delta_workflow import (
        DEVELOPMENT_BRANCH,
        DeltaWorkflowError,
        unintegrated_topic_worktrees,
        validate_changed_integration_asset_versions,
    )
except ModuleNotFoundError:
    from delta_workflow import (  # type: ignore[no-redef]
        DEVELOPMENT_BRANCH,
        DeltaWorkflowError,
        unintegrated_topic_worktrees,
        validate_changed_integration_asset_versions,
    )


PROJECT_ROOT = Path(__file__).resolve().parent.parent
TARGET_ROOT = PROJECT_ROOT / "target" / "x86_64-pc-windows-msvc"
INPUT_ROOT = TARGET_ROOT / "installer-inputs"
OUTPUT_PATH = TARGET_ROOT / "release" / "herdr-ext_local_candidate_setup.exe"
FAULT_OUTPUT_ROOT = PROJECT_ROOT / "target" / "installer-faults"
NSIS_CACHE = TARGET_ROOT / "tools" / "nsis-3.12"
WINDOWS_TARGET = "x86_64-pc-windows-msvc"
DEFAULT_CARGO_TARGET = (
    Path(tempfile.gettempdir()) / "opencode" / "herdr-win-cargo-target"
)
BUILD_ID_RE = re.compile(r"^[0-9a-f]{12}\.[0-9a-f]{12}$")
BUILD_FRESHNESS_RE = re.compile(
    r"^(?P<year>[0-9]{4})\.(?P<month>[0-9]{2})\.(?P<day>[0-9]{2})\."
    r"(?P<hour>[0-9]{2})(?P<minute>[0-9]{2})Z$"
)
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
SOURCE_FINGERPRINT_RE = re.compile(r"^[0-9a-f]{64}$")
BUILD_NONCE_RE = re.compile(r"^[0-9a-f]{32}$")
DYNAMIC_MSVC_RUNTIME_IMPORT = re.compile(
    r"(?im)^\s*((?:VCRUNTIME|MSVCP)[A-Z0-9_]*\.dll)\s*$"
)
FOCUSED_TEST_RESULT_RE = re.compile(r"(?m)^test result: ok\. (?P<passed>[0-9]+) passed;")
NEXTEST_SUMMARY_RE = re.compile(
    r"(?m)^\s*Summary \[[^\]]+\] (?P<run>[0-9]+) tests? run: "
    r"(?P<passed>[0-9]+) passed(?:,|$)"
)
LOCAL_VERSION_RE = re.compile(
    r"^herdr-ext (?P<freshness>[0-9]{4}\.[0-9]{2}\.[0-9]{2}\.[0-9]{4}Z) "
    r"\(local, Herdr (?P<base>[0-9]+\.[0-9]+\.[0-9]+), "
    r"build (?P<build>[0-9a-f]{12}\.[0-9a-f]{12})\)$"
)
REPARSE_POINT = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
GIBIBYTE = 1024**3
MINIMUM_CARGO_COMMIT_HEADROOM_BYTES = 3 * GIBIBYTE
MINIMUM_CANDIDATE_DISK_HEADROOM_BYTES = 2 * GIBIBYTE
MAXIMUM_CANDIDATE_INCREMENTAL_CACHE_BYTES = 2 * GIBIBYTE


class LocalInstallerError(RuntimeError):
    pass


@dataclass(frozen=True)
class InstallerIdentity:
    build_id: str
    base_version: str
    build_freshness: str


@dataclass(frozen=True)
class InstallerPaths:
    target_root: Path
    input_root: Path
    output_path: Path
    nsis_cache: Path


DEFAULT_PATHS = InstallerPaths(
    target_root=TARGET_ROOT,
    input_root=INPUT_ROOT,
    output_path=OUTPUT_PATH,
    nsis_cache=NSIS_CACHE,
)
CANDIDATE_STAMP = TARGET_ROOT / ".candidate-build.json"


def _isolated_candidate_paths(build_id: str) -> InstallerPaths:
    if BUILD_ID_RE.fullmatch(build_id) is None:
        raise LocalInstallerError(f"invalid candidate build ID {build_id!r}")
    target_root = TARGET_ROOT / "isolated" / build_id
    return InstallerPaths(
        target_root=target_root,
        input_root=target_root / "installer-inputs",
        output_path=target_root / "release" / OUTPUT_PATH.name,
        nsis_cache=target_root / "tools" / "nsis-3.12",
    )


def _candidate_paths(
    branch: str, build_id: str, *, isolated: bool
) -> InstallerPaths:
    if isolated or branch != DEVELOPMENT_BRANCH:
        return _isolated_candidate_paths(build_id)
    return DEFAULT_PATHS


def _canonical_candidate_identity(
    source_fingerprint: str, base_commit: str
) -> tuple[str, str]:
    target_root = _directory(CANDIDATE_STAMP.parent, "candidate target root")
    stamp_path = target_root / CANDIDATE_STAMP.name
    if stamp_path.exists():
        stamp = _safe_path(stamp_path, "candidate build stamp", directory=False)
        try:
            data = json.loads(stamp.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as error:
            raise LocalInstallerError(f"could not read candidate build stamp: {error}") from error
        expected_fields = {
            "schema",
            "source_fingerprint",
            "base_commit",
            "build_id",
            "build_freshness",
            "build_nonce",
        }
        if (
            not isinstance(data, dict)
            or set(data) != expected_fields
            or data.get("schema") != 2
        ):
            raise LocalInstallerError("candidate build stamp has an unsupported schema")
        if (
            data["source_fingerprint"] == source_fingerprint
            and data["base_commit"] == base_commit
        ):
            build_freshness = _validate_build_freshness(str(data["build_freshness"]))
            build_id = _candidate_build_id(
                base_commit,
                source_fingerprint,
                build_freshness,
                str(data["build_nonce"]),
            )
            if data["build_id"] != build_id:
                raise LocalInstallerError("candidate build stamp identity is inconsistent")
            return build_id, build_freshness

    build_freshness = _new_build_freshness()
    build_nonce = uuid.uuid4().hex
    build_id = _candidate_build_id(
        base_commit, source_fingerprint, build_freshness, build_nonce
    )
    temporary = stamp_path.with_name(
        f".{stamp_path.name}.write-{uuid.uuid4().hex}"
    )
    try:
        temporary.write_text(
            json.dumps(
                {
                    "schema": 2,
                    "source_fingerprint": source_fingerprint,
                    "base_commit": base_commit,
                    "build_id": build_id,
                    "build_freshness": build_freshness,
                    "build_nonce": build_nonce,
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
            newline="\n",
        )
        os.replace(temporary, stamp_path)
    finally:
        if temporary.exists():
            temporary.unlink()
    return build_id, build_freshness


def _prune_completed_candidate_outputs(
    paths: InstallerPaths, build_id: str, *, isolated: bool
) -> None:
    if isolated:
        root = _safe_path(paths.target_root, "isolated candidate root", directory=True)
        shutil.rmtree(root)
        print("isolated_outputs_removed=yes")
        return

    target_root = _safe_path(paths.target_root, "candidate target root", directory=True)
    input_root = _safe_path(paths.input_root, "candidate input root", directory=True)
    _safe_path(
        input_root / build_id,
        "current candidate bundle",
        directory=True,
    )
    removed = 0
    for child in input_root.iterdir():
        if child.name == build_id:
            continue
        stale = _safe_path(child, "superseded candidate bundle", directory=True)
        shutil.rmtree(stale)
        removed += 1
    stamp_path = target_root / CANDIDATE_STAMP.name
    if stamp_path.exists():
        stamp = _safe_path(stamp_path, "candidate build stamp", directory=False)
        stamp.unlink()
    temporary_root = target_root / "tmp"
    if temporary_root.exists():
        temporary = _safe_path(temporary_root, "candidate temporary root", directory=True)
        try:
            temporary.rmdir()
        except OSError:
            pass
    print(f"superseded_bundles_removed={removed}")


def _run(
    command: Path | str,
    arguments: Sequence[str],
    *,
    cwd: Path | None = None,
    timeout: int,
    clean_runtime_environment: bool = False,
    environment_overrides: dict[str, str] | None = None,
    removed_environment: Sequence[str] = (),
) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    environment["GIT_TERMINAL_PROMPT"] = "0"
    for name in removed_environment:
        environment.pop(name, None)
    if environment_overrides:
        environment.update(environment_overrides)
    if clean_runtime_environment:
        environment.pop("HERDR_REMOTE_SIDECAR_V1", None)
    try:
        result = subprocess.run(
            [str(command), *arguments],
            cwd=cwd,
            env=environment,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
            timeout=timeout,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise LocalInstallerError(f"could not run {command!s}: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no diagnostic"
        raise LocalInstallerError(
            f"{command!s} failed with exit code {result.returncode}: {detail}"
        )
    return result


def _print_process_output(result: subprocess.CompletedProcess[str]) -> None:
    if result.stdout:
        print(result.stdout, end="" if result.stdout.endswith("\n") else "\n")
    if result.stderr:
        print(
            result.stderr,
            file=sys.stderr,
            end="" if result.stderr.endswith("\n") else "\n",
        )


def _safe_path(path: Path, label: str, *, directory: bool) -> Path:
    if not path.is_absolute():
        raise LocalInstallerError(f"{label} must be an absolute path")
    try:
        original_status = path.lstat()
        if path.is_symlink() or bool(
            getattr(original_status, "st_file_attributes", 0) & REPARSE_POINT
        ):
            raise LocalInstallerError(f"{label} must not be a reparse path: {path}")
        resolved = path.resolve(strict=True)
        status = resolved.lstat()
    except LocalInstallerError:
        raise
    except OSError as error:
        raise LocalInstallerError(f"could not resolve {label} {path}: {error}") from error
    expected = stat.S_ISDIR(status.st_mode) if directory else stat.S_ISREG(status.st_mode)
    reparse = bool(getattr(status, "st_file_attributes", 0) & REPARSE_POINT)
    if not expected or reparse or (not directory and status.st_size <= 0):
        kind = "directory" if directory else "nonempty file"
        raise LocalInstallerError(f"{label} must be a regular non-reparse {kind}: {resolved}")
    return resolved


def _git_arguments(path: Path, arguments: Sequence[str]) -> list[str]:
    safe_directory = path.resolve().as_posix()
    return [
        "-c",
        "core.longpaths=true",
        "-c",
        f"safe.directory={safe_directory}",
        "-C",
        str(path),
        *arguments,
    ]


def _git(path: Path, arguments: Sequence[str]) -> str:
    return _run(
        "git",
        _git_arguments(path, arguments),
        timeout=10,
    ).stdout.strip()


def _source_root(path: Path, *, test_only: bool = False) -> Path:
    source = _safe_path(path, "--source-worktree", directory=True)
    if Path(_git(source, ["rev-parse", "--show-toplevel"])).resolve() != source:
        raise LocalInstallerError("--source-worktree must identify its Git root")
    control_common = Path(
        _git(PROJECT_ROOT, ["rev-parse", "--path-format=absolute", "--git-common-dir"])
    ).resolve()
    source_common = Path(
        _git(source, ["rev-parse", "--path-format=absolute", "--git-common-dir"])
    ).resolve()
    if os.path.normcase(control_common) != os.path.normcase(source_common):
        raise LocalInstallerError("source worktree belongs to another Git repository")
    _source_branch(source, test_only=test_only)
    for relative in (
        "scripts/package_windows_conpty.py",
        "scripts/package_windows_installer.ps1",
        "scripts/windows_installer_fault_test.ps1",
    ):
        _safe_path(source / relative, f"source {relative}", directory=False)
    return source


def _source_branch(source: Path, *, test_only: bool = False) -> str:
    branch = _git(source, ["symbolic-ref", "--short", "HEAD"])
    if not test_only and branch != DEVELOPMENT_BRANCH and not branch.startswith("agent/delta-"):
        raise LocalInstallerError(f"source worktree uses unsupported branch {branch!r}")
    return branch


def _require_integrated_development_source(
    source: Path, branch: str, *, isolated: bool
) -> None:
    if isolated or branch != DEVELOPMENT_BRANCH:
        return
    status = _git(source, ["status", "--porcelain=v1", "--untracked-files=all"])
    if status:
        raise LocalInstallerError("development worktree must be clean before packaging")
    local_head = _git(source, ["rev-parse", "HEAD"])
    try:
        unintegrated = unintegrated_topic_worktrees(PROJECT_ROOT, local_head)
    except DeltaWorkflowError as error:
        raise LocalInstallerError(
            f"could not verify development worktree integration: {error}"
        ) from error
    if unintegrated:
        detail = ", ".join(
            f"{topic.branch} ({topic.path})" for topic in unintegrated
        )
        raise LocalInstallerError(
            f"unintegrated topic worktrees block the development installer: {detail}"
        )


def _validate_build_freshness(value: str) -> str:
    match = BUILD_FRESHNESS_RE.fullmatch(value)
    if match is None:
        raise LocalInstallerError(
            f"invalid build freshness {value!r}; expected UTC YYYY.MM.DD.HHMMZ"
        )
    try:
        datetime(
            int(match.group("year")),
            int(match.group("month")),
            int(match.group("day")),
            int(match.group("hour")),
            int(match.group("minute")),
            tzinfo=timezone.utc,
        )
    except ValueError as error:
        raise LocalInstallerError(f"invalid build freshness {value!r}: {error}") from error
    return value


def _new_build_freshness() -> str:
    return datetime.now(timezone.utc).strftime("%Y.%m.%d.%H%MZ")


def parse_identity(version: str, launcher_build_id: str) -> InstallerIdentity:
    if BUILD_ID_RE.fullmatch(launcher_build_id) is None:
        raise LocalInstallerError(f"launcher returned invalid build ID {launcher_build_id!r}")
    match = LOCAL_VERSION_RE.fullmatch(version)
    if match is None:
        raise LocalInstallerError(f"runtime is not an exact local build: {version!r}")
    if match.group("build") != launcher_build_id:
        raise LocalInstallerError("runtime and launcher report different local build identities")
    return InstallerIdentity(
        launcher_build_id,
        match.group("base"),
        _validate_build_freshness(match.group("freshness")),
    )


def _source_fingerprint(
    source_commit: str,
    tracked_diff: str,
    untracked_files: Sequence[tuple[str, bytes]],
) -> str:
    if COMMIT_RE.fullmatch(source_commit) is None:
        raise LocalInstallerError("candidate identity requires a full lowercase source commit")
    digest = hashlib.sha256()
    digest.update(f"source-commit\0{source_commit}\0tracked-diff\0".encode())
    digest.update(tracked_diff.encode("utf-8"))
    for name, payload in sorted(untracked_files):
        digest.update(f"\0untracked\0{name}\0{len(payload)}\0".encode())
        digest.update(payload)
    return digest.hexdigest()


def _candidate_build_id(
    base_commit: str,
    source_fingerprint: str,
    build_freshness: str,
    build_nonce: str,
) -> str:
    if COMMIT_RE.fullmatch(base_commit) is None or SOURCE_FINGERPRINT_RE.fullmatch(
        source_fingerprint
    ) is None or BUILD_NONCE_RE.fullmatch(build_nonce) is None:
        raise LocalInstallerError(
            "candidate identity requires exact source and candidate provenance"
        )
    _validate_build_freshness(build_freshness)
    digest = hashlib.sha256(
        (
            f"source\0{source_fingerprint}\0freshness\0{build_freshness}\0"
            f"nonce\0{build_nonce}\0"
        ).encode()
    ).hexdigest()
    return f"{base_commit[:12]}.{digest[:12]}"


def _source_build_provenance(source: Path) -> tuple[str, str]:
    base_path = _safe_path(
        PROJECT_ROOT / "patches" / "delta" / "BASE",
        "delta BASE",
        directory=False,
    )
    base_commit = base_path.read_text(encoding="utf-8").strip()
    source_commit = _git(source, ["rev-parse", "HEAD"])
    tracked_diff = _run(
        "git",
        _git_arguments(source, ["diff", "--binary", "--no-ext-diff", "HEAD"]),
        timeout=10,
    ).stdout
    untracked_output = _run(
        "git",
        _git_arguments(
            source, ["ls-files", "--others", "--exclude-standard", "-z"]
        ),
        timeout=10,
    ).stdout
    untracked_files: list[tuple[str, bytes]] = []
    for relative in filter(None, untracked_output.split("\0")):
        path = Path(relative)
        if path.is_absolute() or ".." in path.parts:
            raise LocalInstallerError(
                f"untracked source path escapes the worktree: {relative}"
            )
        source_file = _safe_path(
            source / path, f"untracked source {relative}", directory=False
        )
        untracked_files.append((path.as_posix(), source_file.read_bytes()))
    return (
        _source_fingerprint(source_commit, tracked_diff, untracked_files),
        base_commit,
    )


def _require_candidate_cache_budget(cargo_target: Path) -> None:
    free = shutil.disk_usage(cargo_target).free
    if free < MINIMUM_CANDIDATE_DISK_HEADROOM_BYTES:
        raise LocalInstallerError(
            "Candidate compilation requires at least 2 GiB free on the Cargo target "
            "volume; inspect disk usage before retrying. No cache was deleted."
        )
    cache = cargo_target / WINDOWS_TARGET / "release" / "incremental"
    size = 0
    if cache.exists():
        _safe_path(cache, "incremental cache", directory=True)
        for directory, folders, files in os.walk(cache, followlinks=False):
            for name in folders:
                _safe_path(Path(directory) / name, "cache directory", directory=True)
            for name in files:
                path = Path(directory) / name
                info = path.lstat()
                if not stat.S_ISREG(info.st_mode) or getattr(info, "st_file_attributes", 0) & REPARSE_POINT:
                    raise LocalInstallerError(f"incremental cache contains an unsafe entry: {path}")
                # Empty compiler lock files are valid, unlike installer payload files.
                size += info.st_size
    if size > MAXIMUM_CANDIDATE_INCREMENTAL_CACHE_BYTES:
        raise LocalInstallerError(
            f"Candidate incremental cache exceeds its 2 GiB iteration budget: {cache}. "
            "After all users of this Cargo target have stopped, remove only that "
            "compiler cache and rebuild. No cache was deleted."
        )
    print(f"cargo_disk_free_gib={free / GIBIBYTE:.2f}")
    print(f"candidate_incremental_cache_gib={size / GIBIBYTE:.2f}")


def _cargo_build_arguments(cargo_target: Path, jobs: int) -> list[str]:
    if jobs < 1:
        raise LocalInstallerError("Cargo requires at least one build job")
    return [
        "build",
        "--release",
        "--locked",
        "--target",
        WINDOWS_TARGET,
        "--target-dir",
        str(cargo_target),
        "--jobs",
        str(jobs),
        "--bin",
        "herdr",
        "--bin",
        "herdr-launcher",
        "--bin",
        "herdr-installer-helper",
    ]


def _cargo_test_arguments(
    cargo_target: Path, jobs: int, test_filter: str
) -> list[str]:
    if jobs < 1:
        raise LocalInstallerError("Cargo requires at least one build job")
    if not test_filter.strip() or test_filter.startswith("-"):
        raise LocalInstallerError(
            "--release-test-filter must be one non-option test filter"
        )
    return [
        "test",
        "--release",
        "--locked",
        "--target",
        WINDOWS_TARGET,
        "--target-dir",
        str(cargo_target),
        "--jobs",
        str(jobs),
        "--bin",
        "herdr",
        test_filter,
        "--",
        "--nocapture",
    ]


def _portable_pty_test_arguments(
    manifest: Path, cargo_target: Path, jobs: int, test_filter: str
) -> list[str]:
    if jobs < 1:
        raise LocalInstallerError("Cargo requires at least one build job")
    if not test_filter.strip() or test_filter.startswith("-"):
        raise LocalInstallerError(
            "--portable-pty-test-filter must be one non-option test filter"
        )
    return [
        "test",
        "--manifest-path",
        str(manifest),
        "--lib",
        "--target",
        WINDOWS_TARGET,
        "--target-dir",
        str(cargo_target),
        "--jobs",
        str(jobs),
        test_filter,
        "--",
        "--nocapture",
    ]


def _just_test_arguments(test_filter: str) -> list[str]:
    if not test_filter.strip() or test_filter.startswith("-"):
        raise LocalInstallerError("--test-filter must be one non-option test filter")
    return ["test-one", test_filter]


def _require_one_focused_test(output: str) -> None:
    passed = [int(match.group("passed")) for match in FOCUSED_TEST_RESULT_RE.finditer(output)]
    if passed != [1]:
        raise LocalInstallerError("--test-filter must run exactly one passing test")


def _require_one_nextest_test(output: str) -> None:
    matches = [
        (int(match.group("run")), int(match.group("passed")))
        for match in NEXTEST_SUMMARY_RE.finditer(output)
    ]
    if matches != [(1, 1)]:
        raise LocalInstallerError("--test-filter must run exactly one passing test")


def _dynamic_msvc_runtime_imports(dependencies: str) -> list[str]:
    return sorted(
        {
            match.group(1).upper()
            for match in DYNAMIC_MSVC_RUNTIME_IMPORT.finditer(dependencies)
        }
    )


def _verify_self_contained_windows_executables(paths: Sequence[Path]) -> None:
    for path in paths:
        dependencies = _run(
            "dumpbin.exe",
            ["/DEPENDENTS", str(path)],
            timeout=30,
        ).stdout
        imports = _dynamic_msvc_runtime_imports(dependencies)
        if imports:
            raise LocalInstallerError(
                f"{path.name} requires a non-inbox MSVC runtime: {', '.join(imports)}"
            )


def _available_cpu_count() -> int:
    process_cpu_count = getattr(os, "process_cpu_count", os.cpu_count)
    return process_cpu_count() or 1


def _directory(path: Path, label: str) -> Path:
    if not path.is_absolute():
        raise LocalInstallerError(f"{label} must be an absolute path")
    path.mkdir(parents=True, exist_ok=True)
    return _safe_path(path, label, directory=True)


def _windows_powershell() -> Path:
    return _safe_path(
        Path(os.environ["SystemRoot"])
        / "System32/WindowsPowerShell/v1.0/powershell.exe",
        "Windows PowerShell 5.1",
        directory=False,
    )


def _require_cargo_commit_headroom() -> None:
    script = _safe_path(
        PROJECT_ROOT / "scripts" / "windows_commit_headroom.ps1",
        "Windows commit-headroom helper",
        directory=False,
    )
    result = _run(
        _windows_powershell(),
        [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            str(script),
            "-MinimumHeadroomBytes",
            str(MINIMUM_CARGO_COMMIT_HEADROOM_BYTES),
        ],
        cwd=PROJECT_ROOT,
        timeout=15,
    )
    _print_process_output(result)


def _run_normal_focused_test(
    source: Path, cargo_target: Path, jobs: int, test_filter: str
) -> None:
    _require_cargo_commit_headroom()
    print(f"focused_test={test_filter}")
    print("focused_test_profile=normal")
    test_started = time.monotonic()
    test_result = _run(
        "just",
        _just_test_arguments(test_filter),
        cwd=source,
        timeout=1200,
        environment_overrides={
            "CARGO_BUILD_JOBS": str(jobs),
            "CARGO_TARGET_DIR": str(cargo_target),
        },
        removed_environment=(
            "HERDR_BUILD_ID",
            "HERDR_BUILD_COMMIT",
            "HERDR_BUILD_FRESHNESS",
            "HERDR_RELEASE_VERSION",
        ),
    )
    _print_process_output(test_result)
    _require_one_nextest_test(f"{test_result.stdout}\n{test_result.stderr}")
    print(f"focused_test_elapsed_seconds={time.monotonic() - test_started:.3f}")


def _run_portable_pty_focused_test(
    source: Path, cargo_target: Path, jobs: int, test_filter: str
) -> None:
    _require_cargo_commit_headroom()
    package = _safe_path(
        source / "vendor" / "portable-pty",
        "vendored portable-pty package",
        directory=True,
    )
    manifest = _safe_path(
        package / "Cargo.toml", "vendored portable-pty manifest", directory=False
    )
    package_lock = package / "Cargo.lock"
    if package_lock.exists() or package_lock.is_symlink():
        raise LocalInstallerError(
            "vendored portable-pty Cargo.lock already exists; preserve it for its owner"
        )

    lock_created = False
    print(f"focused_test={test_filter}")
    print("focused_test_owner=vendor/portable-pty")
    print("focused_test_profile=normal")
    test_started = time.monotonic()
    try:
        try:
            with package_lock.open("xb") as temporary_lock:
                lock_created = True
                temporary_lock.write(b"version = 4\n")
        except OSError as error:
            raise LocalInstallerError(
                f"could not create temporary portable-pty Cargo lock: {error}"
            ) from error

        test_result = _run(
            "cargo",
            _portable_pty_test_arguments(
                manifest, cargo_target, jobs, test_filter
            ),
            cwd=source,
            timeout=300,
            environment_overrides={"CARGO_BUILD_JOBS": str(jobs)},
            removed_environment=(
                "HERDR_BUILD_ID",
                "HERDR_BUILD_COMMIT",
                "HERDR_BUILD_FRESHNESS",
                "HERDR_RELEASE_VERSION",
            ),
        )
        _print_process_output(test_result)
        _require_one_focused_test(test_result.stdout)
    finally:
        if lock_created:
            owned_lock = _safe_path(
                package_lock, "temporary portable-pty Cargo lock", directory=False
            )
            try:
                owned_lock.unlink()
            except OSError as error:
                raise LocalInstallerError(
                    f"could not remove temporary portable-pty Cargo lock: {error}"
                ) from error
    print(f"focused_test_elapsed_seconds={time.monotonic() - test_started:.3f}")


def _run_interactive_server_launch_probe(source: Path, runtime: Path) -> None:
    script = _safe_path(
        source / "scripts" / "windows_interactive_server_launch_probe.ps1",
        "interactive server launch probe",
        directory=False,
    )
    runtime = _safe_path(runtime, "interactive server probe runtime", directory=False)
    started = time.monotonic()
    result = _run(
        _windows_powershell(),
        [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            str(script),
            "-ExePath",
            str(runtime),
        ],
        cwd=source,
        timeout=60,
        clean_runtime_environment=True,
    )
    _print_process_output(result)
    print(f"interactive_server_probe_elapsed_seconds={time.monotonic() - started:.3f}")


def _conpty_package_path(source: Path) -> Path:
    metadata = _safe_path(
        source / "packaging" / "windows" / "conpty.json",
        "source ConPTY metadata",
        directory=False,
    )
    try:
        package = json.loads(metadata.read_text(encoding="utf-8"))["package"]
        package_id = str(package["id"])
        version = str(package["version"])
    except (OSError, UnicodeError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise LocalInstallerError(
            f"could not read source ConPTY package identity: {error}"
        ) from error
    if re.fullmatch(r"[A-Za-z0-9.]+", package_id) is None or re.fullmatch(
        r"[0-9]+(?:\.[0-9]+)+", version
    ) is None:
        raise LocalInstallerError("source ConPTY package identity is invalid")
    return (
        PROJECT_ROOT
        / "target"
        / "package-cache"
        / f"{package_id}.{version}.nupkg"
    )


def _identity(stage: Path, launcher: Path) -> InstallerIdentity:
    runtime = _safe_path(stage / "herdr.exe", "bundle runtime", directory=False)
    launcher = _safe_path(launcher, "bundle launcher", directory=False)
    build_id = _run(
        launcher,
        ["--herdr-private-launcher-build-id-v1"],
        timeout=10,
    ).stdout.strip()
    version = _run(
        runtime,
        ["--version"],
        timeout=10,
        clean_runtime_environment=True,
    ).stdout.strip()
    return parse_identity(version, build_id)


def _files(root: Path) -> dict[str, Path]:
    result: dict[str, Path] = {}
    for current, directories, names in os.walk(root, followlinks=False):
        current_path = Path(current)
        for name in directories:
            _safe_path(current_path / name, "bundle entry", directory=True)
        for name in names:
            path = _safe_path(current_path / name, "bundle entry", directory=False)
            result[path.relative_to(root).as_posix()] = path
    return result


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _hashes(root: Path) -> dict[str, str]:
    return {
        name: _sha256(path)
        for name, path in sorted(_files(root).items())
        if name != "bundle.json"
    }


def _validate_stage(source: Path, stage: Path) -> None:
    _run(
        sys.executable,
        [
            str(source / "scripts" / "package_windows_conpty.py"),
            "validate",
            "--stage-dir",
            str(stage),
        ],
        cwd=source,
        timeout=60,
    )


def _bundle_manifest(bundle: Path) -> tuple[InstallerIdentity, dict[str, str]]:
    manifest = _safe_path(bundle / "bundle.json", "bundle manifest", directory=False)
    try:
        data = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise LocalInstallerError(f"could not read bundle manifest: {error}") from error
    if set(data) != {
        "schema",
        "build_id",
        "base_version",
        "build_freshness",
        "files",
    }:
        raise LocalInstallerError("bundle manifest has unsupported fields")
    if data["schema"] != 2 or not isinstance(data["files"], dict):
        raise LocalInstallerError("bundle manifest has unsupported schema")
    identity = InstallerIdentity(
        str(data["build_id"]),
        str(data["base_version"]),
        _validate_build_freshness(str(data["build_freshness"])),
    )
    hashes = {str(name): str(value) for name, value in data["files"].items()}
    if BUILD_ID_RE.fullmatch(identity.build_id) is None or any(
        re.fullmatch(r"[0-9a-f]{64}", value) is None for value in hashes.values()
    ):
        raise LocalInstallerError("bundle manifest contains invalid identity or hash")
    return identity, hashes


def validate_bundle(
    source: Path, path: Path, *, input_root: Path = INPUT_ROOT
) -> InstallerIdentity:
    bundle = _safe_path(path, "--input-bundle", directory=True)
    try:
        bundle.relative_to(input_root.resolve())
    except ValueError as error:
        raise LocalInstallerError(f"input bundle must remain below {input_root}") from error
    recorded_identity, recorded_hashes = _bundle_manifest(bundle)
    if _hashes(bundle) != recorded_hashes:
        raise LocalInstallerError("installer input bundle files or hashes changed")
    stage = _safe_path(bundle / "stage", "bundle stage", directory=True)
    runtime = _safe_path(stage / "herdr.exe", "bundle runtime", directory=False)
    launcher = _safe_path(
        bundle / "herdr-launcher.exe", "bundle launcher", directory=False
    )
    helper = _safe_path(
        bundle / "herdr-installer-helper.exe", "bundle helper", directory=False
    )
    _verify_self_contained_windows_executables((runtime, launcher, helper))
    _validate_stage(source, stage)
    if _identity(stage, launcher) != recorded_identity:
        raise LocalInstallerError("bundle manifest does not match executable identities")
    return recorded_identity


def prepare(
    options: argparse.Namespace, *, paths: InstallerPaths | None = None
) -> None:
    source = _source_root(options.source_worktree)
    stage = _safe_path(options.stage_dir, "--stage-dir", directory=True)
    launcher = _safe_path(options.launcher_exe, "--launcher-exe", directory=False)
    helper = _safe_path(options.installer_helper_exe, "--installer-helper-exe", directory=False)
    _verify_self_contained_windows_executables(
        (
            _safe_path(stage / "herdr.exe", "staged runtime", directory=False),
            launcher,
            helper,
        )
    )
    _validate_stage(source, stage)
    identity = _identity(stage, launcher)
    if paths is None:
        paths = _candidate_paths(
            _source_branch(source), identity.build_id, isolated=options.isolated
        )
    paths.input_root.mkdir(parents=True, exist_ok=True)
    destination = paths.input_root / identity.build_id
    if destination.exists():
        if (
            validate_bundle(source, destination, input_root=paths.input_root)
            != identity
        ):
            raise LocalInstallerError("existing bundle uses another identity")
        print(f"bundle={destination}")
        print("reused=yes")
        return

    temporary = paths.input_root / f".{identity.build_id}.prepare-{uuid.uuid4().hex}"
    temporary.mkdir()
    try:
        (temporary / "stage").mkdir()
        for relative, path in _files(stage).items():
            target = temporary / "stage" / Path(relative)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
        shutil.copy2(launcher, temporary / "herdr-launcher.exe")
        shutil.copy2(helper, temporary / "herdr-installer-helper.exe")
        manifest = {
            "schema": 2,
            "build_id": identity.build_id,
            "base_version": identity.base_version,
            "build_freshness": identity.build_freshness,
            "files": _hashes(temporary),
        }
        (temporary / "bundle.json").write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
            newline="\n",
        )
        validate_bundle(source, temporary, input_root=paths.input_root)
        temporary.rename(destination)
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)
    print(f"bundle={destination}")
    print("reused=no")


def build(
    options: argparse.Namespace,
    *,
    paths: InstallerPaths | None = None,
    run_interactive_probe: bool = False,
) -> None:
    source = _source_root(options.source_worktree)
    bundle = _safe_path(options.input_bundle, "--input-bundle", directory=True)
    if paths is None:
        recorded_identity, _ = _bundle_manifest(bundle)
        paths = _candidate_paths(
            _source_branch(source),
            recorded_identity.build_id,
            isolated=options.isolated,
        )
    identity = validate_bundle(source, bundle, input_root=paths.input_root)
    if run_interactive_probe:
        _run_interactive_server_launch_probe(source, bundle / "stage" / "herdr.exe")
    paths.output_path.parent.mkdir(parents=True, exist_ok=True)
    powershell = _windows_powershell()
    packager_arguments = [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        str(source / "scripts/package_windows_installer.ps1"),
        "-StageDir",
        str(bundle / "stage"),
        "-LauncherExe",
        str(bundle / "herdr-launcher.exe"),
        "-InstallerHelperExe",
        str(bundle / "herdr-installer-helper.exe"),
        "-BuildId",
        identity.build_id,
        "-BuildFreshness",
        identity.build_freshness,
        "-ReleaseVersion",
        "local",
        "-BaseVersion",
        identity.base_version,
    ]
    if options.product_name is not None:
        packager_arguments.extend(["-ProductName", options.product_name])
    packager_arguments.extend(
        [
            "-OutputPath",
            str(paths.output_path),
            "-NsisCacheDir",
            str(paths.nsis_cache),
        ]
    )
    started = time.monotonic()
    result = _run(
        powershell,
        packager_arguments,
        cwd=source,
        timeout=300,
    )
    if result.stdout:
        print(result.stdout, end="" if result.stdout.endswith("\n") else "\n")
    if result.stderr:
        print(
            result.stderr,
            file=sys.stderr,
            end="" if result.stderr.endswith("\n") else "\n",
        )
    selected_product_name = (
        json.dumps(options.product_name)
        if options.product_name is not None
        else "packager-default"
    )
    print(f"product_name={selected_product_name}")
    print(f"elapsed_seconds={time.monotonic() - started:.3f}")


def release_precheck(options: argparse.Namespace) -> None:
    source = _source_root(options.source_worktree)
    bundle = _safe_path(options.input_bundle, "--input-bundle", directory=True)
    identity = validate_bundle(source, bundle)
    output_root = _directory(FAULT_OUTPUT_ROOT, "installer fault output root")
    precheck_arguments = [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        str(source / "scripts/windows_installer_fault_test.ps1"),
        "-StageDir",
        str(bundle / "stage"),
        "-LauncherExe",
        str(bundle / "herdr-launcher.exe"),
        "-InstallerHelperExe",
        str(bundle / "herdr-installer-helper.exe"),
        "-BuildId",
        identity.build_id,
        "-BuildFreshness",
        identity.build_freshness,
        "-ReleaseVersion",
        "local",
        "-BaseVersion",
        identity.base_version,
    ]
    if options.product_name is not None:
        precheck_arguments.extend(["-ProductName", options.product_name])
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="run-", dir=output_root) as temporary:
        precheck_arguments.extend(["-OutputDir", temporary])
        result = _run(
            _windows_powershell(),
            precheck_arguments,
            cwd=source,
            timeout=1800,
        )
    if result.stdout:
        print(result.stdout, end="" if result.stdout.endswith("\n") else "\n")
    if result.stderr:
        print(result.stderr, file=sys.stderr, end="" if result.stderr.endswith("\n") else "\n")
    selected_product_name = (
        json.dumps(options.product_name)
        if options.product_name is not None
        else "packager-default"
    )
    print(f"product_name={selected_product_name}")
    print(f"elapsed_seconds={time.monotonic() - started:.3f}")


def test_one(options: argparse.Namespace) -> None:
    source = _source_root(options.source_worktree, test_only=True)
    cargo_target = _directory(options.cargo_target_dir, "--cargo-target-dir")
    jobs = _available_cpu_count()
    print(f"cargo_jobs={jobs}")
    print(f"cargo_target={cargo_target}")
    if options.portable_pty_test_filter is not None:
        _run_portable_pty_focused_test(
            source, cargo_target, jobs, options.portable_pty_test_filter
        )
    else:
        _run_normal_focused_test(source, cargo_target, jobs, options.test_filter)


def candidate(options: argparse.Namespace) -> None:
    total_started = time.monotonic()
    source = _source_root(options.source_worktree)
    source_branch = _source_branch(source)
    _require_integrated_development_source(
        source, source_branch, isolated=options.isolated
    )
    try:
        changed_integrations = validate_changed_integration_asset_versions(source)
    except DeltaWorkflowError as error:
        raise LocalInstallerError(
            f"managed integration migration validation failed: {error}"
        ) from error
    print(
        "integration_version_gate="
        + (",".join(changed_integrations) if changed_integrations else "unchanged")
    )
    source_fingerprint, base_commit = _source_build_provenance(source)
    canonical = source_branch == DEVELOPMENT_BRANCH and not options.isolated
    if canonical:
        build_id, build_freshness = _canonical_candidate_identity(
            source_fingerprint, base_commit
        )
    else:
        build_freshness = _new_build_freshness()
        build_id = _candidate_build_id(
            base_commit,
            source_fingerprint,
            build_freshness,
            uuid.uuid4().hex,
        )
    paths = _candidate_paths(source_branch, build_id, isolated=options.isolated)
    isolated = not canonical
    print(f"build_id={build_id}")
    print(f"build_freshness={build_freshness}")
    print(f"source_branch={source_branch}")
    if isolated:
        print(f"isolation_root={paths.target_root}")
    else:
        print("acceptance_output=canonical")

    existing_bundle = paths.input_root / build_id
    if (
        options.test_filter is None
        and options.portable_pty_test_filter is None
        and options.release_test_filter is None
        and existing_bundle.exists()
    ):
        print(f"bundle={existing_bundle}")
        build(
            argparse.Namespace(
                source_worktree=source,
                input_bundle=existing_bundle,
                product_name=options.product_name,
            ),
            paths=paths,
            run_interactive_probe=True,
        )
        print("reused=yes")
        _prune_completed_candidate_outputs(paths, build_id, isolated=isolated)
        print(f"total_elapsed_seconds={time.monotonic() - total_started:.3f}")
        return

    cargo_target = _directory(options.cargo_target_dir, "--cargo-target-dir")
    jobs = _available_cpu_count()
    print(f"cargo_jobs={jobs}")
    print(f"cargo_target={cargo_target}")

    build_environment = {
        "HERDR_BUILD_ID": build_id,
        "HERDR_BUILD_COMMIT": base_commit,
        "HERDR_BUILD_FRESHNESS": build_freshness,
        "CARGO_INCREMENTAL": "1",
        # Preserve release's 16 codegen units rather than incremental's 256 default.
        "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16",
    }
    if options.test_filter is not None:
        _run_normal_focused_test(
            source, cargo_target, jobs, options.test_filter
        )
    elif options.portable_pty_test_filter is not None:
        _run_portable_pty_focused_test(
            source, cargo_target, jobs, options.portable_pty_test_filter
        )
    elif options.release_test_filter is not None:
        _require_candidate_cache_budget(cargo_target)
        _require_cargo_commit_headroom()
        print(f"focused_test={options.release_test_filter}")
        print("focused_test_profile=release")
        test_started = time.monotonic()
        test_result = _run(
            "cargo",
            _cargo_test_arguments(cargo_target, jobs, options.release_test_filter),
            cwd=source,
            timeout=1200,
            environment_overrides=build_environment,
            removed_environment=("HERDR_RELEASE_VERSION",),
        )
        _print_process_output(test_result)
        _require_one_focused_test(test_result.stdout)
        print(f"focused_test_elapsed_seconds={time.monotonic() - test_started:.3f}")

    _require_candidate_cache_budget(cargo_target)
    _require_cargo_commit_headroom()
    print("candidate_incremental=true")
    cargo_started = time.monotonic()
    cargo_result = _run(
        "cargo",
        _cargo_build_arguments(cargo_target, jobs),
        cwd=source,
        timeout=1200,
        environment_overrides=build_environment,
        removed_environment=("HERDR_RELEASE_VERSION",),
    )
    _print_process_output(cargo_result)
    print(f"cargo_elapsed_seconds={time.monotonic() - cargo_started:.3f}")

    release = cargo_target / WINDOWS_TARGET / "release"
    runtime = _safe_path(release / "herdr.exe", "candidate runtime", directory=False)
    launcher = _safe_path(
        release / "herdr-launcher.exe", "candidate launcher", directory=False
    )
    helper = _safe_path(
        release / "herdr-installer-helper.exe",
        "candidate installer helper",
        directory=False,
    )
    temporary_parent = _directory(
        paths.target_root / "tmp", "candidate temporary root"
    )
    with tempfile.TemporaryDirectory(
        prefix=f"candidate-{build_id}-", dir=temporary_parent
    ) as temporary:
        stage = Path(temporary) / "stage"
        _run(
            sys.executable,
            [
                str(source / "scripts" / "package_windows_conpty.py"),
                "stage",
                "--package",
                str(_conpty_package_path(source)),
                "--herdr-exe",
                str(runtime),
                "--output-dir",
                str(stage),
            ],
            cwd=source,
            timeout=180,
        )
        prepare(
            argparse.Namespace(
                source_worktree=source,
                stage_dir=stage,
                launcher_exe=launcher,
                installer_helper_exe=helper,
            ),
            paths=paths,
        )

    build(
        argparse.Namespace(
            source_worktree=source,
            input_bundle=paths.input_root / build_id,
            product_name=options.product_name,
        ),
        paths=paths,
        run_interactive_probe=True,
    )
    _prune_completed_candidate_outputs(paths, build_id, isolated=isolated)
    print(f"total_elapsed_seconds={time.monotonic() - total_started:.3f}")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_command = commands.add_parser("prepare")
    prepare_command.add_argument("--source-worktree", required=True, type=Path)
    prepare_command.add_argument("--stage-dir", required=True, type=Path)
    prepare_command.add_argument("--launcher-exe", required=True, type=Path)
    prepare_command.add_argument("--installer-helper-exe", required=True, type=Path)
    prepare_command.add_argument("--isolated", action="store_true")
    build_command = commands.add_parser("build")
    build_command.add_argument("--source-worktree", required=True, type=Path)
    build_command.add_argument("--input-bundle", required=True, type=Path)
    build_command.add_argument(
        "--product-name",
        help="validated runtime product and managed install-root name",
    )
    build_command.add_argument("--isolated", action="store_true")
    precheck_command = commands.add_parser(
        "release-precheck",
        help="manual installer diagnostics in an isolated environment, not a release gate",
        description=(
            "Manual installer fault diagnostics. Mutates the current user's registry "
            "and PATH; use only an explicitly isolated Windows environment, never an "
            "active working profile. Not required before a GitHub release build."
        ),
    )
    precheck_command.add_argument("--source-worktree", required=True, type=Path)
    precheck_command.add_argument("--input-bundle", required=True, type=Path)
    precheck_command.add_argument(
        "--product-name",
        help="runtime product name used by the installer under test",
    )
    test_command = commands.add_parser("test-one")
    test_command.add_argument("--source-worktree", required=True, type=Path)
    test_command.add_argument(
        "--cargo-target-dir", type=Path, default=DEFAULT_CARGO_TARGET
    )
    test_selector = test_command.add_mutually_exclusive_group(required=True)
    test_selector.add_argument(
        "--test-filter",
        help="run one focused herdr test through the normal just test-one gate",
    )
    test_selector.add_argument(
        "--portable-pty-test-filter",
        help="run one focused vendored portable-pty library test",
    )
    candidate_command = commands.add_parser("candidate")
    candidate_command.add_argument("--source-worktree", required=True, type=Path)
    candidate_command.add_argument(
        "--cargo-target-dir", type=Path, default=DEFAULT_CARGO_TARGET
    )
    candidate_test = candidate_command.add_mutually_exclusive_group()
    candidate_test.add_argument(
        "--test-filter",
        help="run one focused herdr test through the normal just test-one gate before packaging",
    )
    candidate_test.add_argument(
        "--portable-pty-test-filter",
        help="run one focused vendored portable-pty library test before packaging",
    )
    candidate_test.add_argument(
        "--release-test-filter",
        help="run one focused herdr test in the release profile when that boundary is required",
    )
    candidate_command.add_argument(
        "--product-name",
        help="validated runtime product and managed install-root name",
    )
    candidate_command.add_argument(
        "--isolated",
        action="store_true",
        help="force build-scoped outputs even for the development branch",
    )
    return parser


def main(arguments: Sequence[str] | None = None) -> int:
    options = _parser().parse_args(arguments)
    try:
        {
            "prepare": prepare,
            "build": build,
            "release-precheck": release_precheck,
            "test-one": test_one,
            "candidate": candidate,
        }[options.command](options)
        return 0
    except LocalInstallerError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
