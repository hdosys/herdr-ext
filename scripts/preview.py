#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
from datetime import date
from pathlib import Path
from typing import Any

ASSET_TARGETS = (
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
    "windows-x86_64",
    "windows-x86_64-installer",
)
HERDR_WIN_RELEASE_VERSION_RE = re.compile(
    r"^(?P<year>[0-9]{4})\.(?P<month>[0-9]{2})\.(?P<day>[0-9]{2})\.(?P<sequence>[1-9][0-9]*)$"
)
HIDDEN_SUBJECTS = (
    "docs: update website manifest",
    "docs: update preview manifest",
    "chore: approve contributor",
    "chore: approve merged contributor",
)
TYPE_HEADINGS = {
    "feat": "Added",
    "fix": "Fixed",
    "perf": "Performance",
    "docs": "Maintenance",
    "ci": "Maintenance",
    "test": "Maintenance",
    "refactor": "Maintenance",
    "chore": "Maintenance",
}
TYPE_ORDER = ("Added", "Fixed", "Performance", "Maintenance", "Other")
COMMIT_RE = re.compile(r"^(?P<kind>[a-z]+)(?:\([^)]+\))?!?:\s+(?P<body>.+)$")
ENDPOINT_PROTOCOL_SOURCE_PATH = Path("src/protocol/endpoint.rs")


def run_git(args: list[str]) -> str:
    return subprocess.check_output(["git", *args], text=True).strip()


def normalize_version(version: str) -> str:
    return version.strip().removeprefix("v")


def validate_endpoint_generation(value: int) -> int:
    if type(value) is not int or not 1 <= value <= 0xFFFFFFFF:
        raise ValueError("endpoint_generation must be a positive u32 integer")
    return value


def read_endpoint_protocol_generation(
    source_path: Path = ENDPOINT_PROTOCOL_SOURCE_PATH,
) -> int:
    content = source_path.read_text(encoding="utf-8")
    match = re.search(r"pub const ENDPOINT_PROTOCOL_GENERATION: u32 = ([0-9]+);", content)
    if not match:
        raise ValueError(
            f"could not read ENDPOINT_PROTOCOL_GENERATION from {source_path}"
        )
    return validate_endpoint_generation(int(match.group(1)))


def latest_stable_tag(ref: str | None = None) -> str:
    args = ["describe", "--tags", "--match", "v[0-9]*", "--abbrev=0"]
    if ref:
        args.append(ref)
    return run_git(args)


def git_is_ancestor(ancestor: str, descendant: str) -> bool:
    result = subprocess.run(
        ["git", "merge-base", "--is-ancestor", ancestor, descendant],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0


def read_json(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def previous_preview_commit(path: Path) -> str | None:
    data = read_json(path)
    if not data:
        return None
    commit = data.get("commit")
    return commit if isinstance(commit, str) and commit.strip() else None


def hidden_subject(subject: str) -> bool:
    lowered = subject.strip().lower()
    return any(lowered.startswith(prefix) for prefix in HIDDEN_SUBJECTS)


def latest_publishable_commit(ref: str) -> str:
    output = run_git(["log", "--pretty=format:%H%x00%s", ref])
    for line in output.splitlines():
        commit, _, subject = line.partition("\x00")
        if commit and not hidden_subject(subject):
            return commit
    raise SystemExit(f"no publishable commit found in {ref}")


def commit_subjects(previous: str, commit: str) -> list[str]:
    output = run_git(["log", "--pretty=format:%s", f"{previous}..{commit}"])
    if not output:
        return []
    subjects = []
    for line in output.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if hidden_subject(stripped):
            continue
        subjects.append(stripped)
    return subjects


def preview_range_base(previous: str, commit: str) -> str:
    try:
        stable = latest_stable_tag(commit)
    except subprocess.CalledProcessError:
        return previous
    if git_is_ancestor(previous, stable) and git_is_ancestor(stable, commit):
        return stable
    return previous


def humanize_subject(subject: str) -> tuple[str, str]:
    match = COMMIT_RE.match(subject)
    if not match:
        return "Other", subject[0].upper() + subject[1:]
    kind = match.group("kind")
    body = match.group("body").strip()
    heading = TYPE_HEADINGS.get(kind, "Other")
    if body:
        body = body[0].upper() + body[1:]
    else:
        body = subject
    return heading, body


def build_notes(previous: str, commit: str, build_id: str, base_version: str, repo: str) -> str:
    short = commit[:12]
    compare = f"https://github.com/{repo}/compare/{previous}...{commit}"
    lines = [
        f"Preview build {build_id}",
        "",
        f"Built from `{short}` on `master`.",
        f"Base stable: v{normalize_version(base_version)}",
        f"Compare: {compare}",
        "",
    ]
    grouped: dict[str, list[str]] = {heading: [] for heading in TYPE_ORDER}
    for subject in commit_subjects(previous, commit):
        heading, body = humanize_subject(subject)
        grouped.setdefault(heading, []).append(body)

    wrote = False
    for heading in TYPE_ORDER:
        items = grouped.get(heading, [])
        if not items:
            continue
        wrote = True
        lines.append(f"### {heading}")
        for item in items:
            lines.append(f"- {item}")
        lines.append("")

    if not wrote:
        lines.extend(["### Changed", "- Rebuilt preview from the current master branch.", ""])

    return "\n".join(lines).rstrip() + "\n"


def parse_herdr_win_release_version(release_version: str) -> tuple[int, int, int, int]:
    match = HERDR_WIN_RELEASE_VERSION_RE.fullmatch(release_version)
    if not match:
        raise ValueError("release_version must use YYYY.MM.DD.N CalVer")
    try:
        date(
            int(match.group("year")),
            int(match.group("month")),
            int(match.group("day")),
        )
    except ValueError as error:
        raise ValueError("release_version must contain a real UTC calendar date") from error
    sequence = int(match.group("sequence"))
    if sequence > 65535:
        raise ValueError("release_version sequence must be at most 65535")
    return (
        int(match.group("year")),
        int(match.group("month")),
        int(match.group("day")),
        sequence,
    )


def herdr_ext_asset_names(release_version: str) -> dict[str, str]:
    parse_herdr_win_release_version(release_version)
    return {
        "linux-x86_64": f"herdr-ext_v{release_version}_linux_amd64",
        "linux-aarch64": f"herdr-ext_v{release_version}_linux_arm64",
        "macos-x86_64": f"herdr-ext_v{release_version}_macos_amd64",
        "macos-aarch64": f"herdr-ext_v{release_version}_macos_arm64",
        "windows-x86_64": f"herdr-ext_v{release_version}_windows_amd64.zip",
        "windows-x86_64-installer": (
            f"herdr-ext_v{release_version}_windows_amd64_setup.exe"
        ),
    }


def require_newer_herdr_win_release(
    release_version: str, current: dict[str, Any]
) -> None:
    candidate = parse_herdr_win_release_version(release_version)
    if "release_version" not in current:
        return
    current_text = current["release_version"]
    if not isinstance(current_text, str):
        raise ValueError("current manifest has an invalid release_version")
    try:
        published = parse_herdr_win_release_version(current_text)
    except ValueError as error:
        raise ValueError("current manifest has an invalid release_version") from error
    if candidate <= published:
        raise ValueError(
            f"release_version {release_version} must be newer than published {current_text}"
        )


def candidate_build_id(
    upstream_sha: str, control_sha: str, run_id: str, run_attempt: int
) -> str:
    if not re.fullmatch(r"[0-9a-f]{40}", upstream_sha):
        raise ValueError("upstream_sha must be a full lowercase 40-hex commit")
    if not re.fullmatch(r"[0-9a-f]{40}", control_sha):
        raise ValueError("control_sha must be a full lowercase 40-hex commit")
    if not re.fullmatch(r"[1-9][0-9]*", run_id):
        raise ValueError("run_id must be a positive decimal GitHub Actions run ID")
    if run_attempt < 1:
        raise ValueError("run_attempt must be a positive GitHub Actions attempt")
    identity = f"{control_sha}\n{run_id}\n{run_attempt}\n".encode()
    candidate = hashlib.sha256(identity).hexdigest()[:12]
    return f"{upstream_sha[:12]}.{candidate}"


def default_asset_urls(repo: str, tag: str, release_version: str) -> dict[str, str]:
    names = herdr_ext_asset_names(release_version)
    expected_tag = f"v{release_version}"
    if tag != expected_tag:
        raise ValueError(
            f"tag must be {expected_tag} for release_version {release_version}"
        )
    return {
        target: f"https://github.com/{repo}/releases/download/{tag}/{names[target]}"
        for target in ASSET_TARGETS
    }


def read_sha_file(path: Path | None) -> dict[str, str]:
    if path is None:
        return {}
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise SystemExit("sha file must be a JSON object")
    return {str(key): str(value) for key, value in data.items()}


def asset_objects(urls: dict[str, str], shas: dict[str, str]) -> dict[str, dict[str, str]]:
    for target in ASSET_TARGETS:
        sha = shas.get(target)
        if not sha or not re.fullmatch(r"[0-9a-f]{64}", sha):
            raise ValueError(f"{target} requires a lowercase SHA-256 digest")

    assets: dict[str, dict[str, str]] = {}
    for target in ASSET_TARGETS:
        sha = shas[target]
        entry = {"url": urls[target], "sha256": sha}
        if target == "windows-x86_64":
            entry["format"] = "zip"
        elif target == "windows-x86_64-installer":
            entry["format"] = "nsis"
        assets[target] = entry
    return assets


def build_manifest(
    output: Path,
    repo: str,
    tag: str,
    build_id: str,
    commit: str,
    built_at: str,
    base_version: str,
    protocol: int,
    endpoint_generation: int,
    notes: str,
    shas: dict[str, str],
    retain: int,
    release_version: str,
) -> str:
    if not re.fullmatch(r"[0-9a-f]{12}\.[0-9a-f]{12}", build_id):
        raise ValueError("build_id must be two lowercase 12-hex components")
    validate_endpoint_generation(endpoint_generation)
    urls = default_asset_urls(repo, tag, release_version)
    assets = asset_objects(urls, shas)
    current = read_json(output) or {}
    require_newer_herdr_win_release(release_version, current)
    current_builds = current.get("builds")
    builds: dict[str, Any] = dict(current_builds) if isinstance(current_builds, dict) else {}
    build = {
        "base_version": normalize_version(base_version),
        "commit": commit,
        "built_at": built_at,
        "protocol": protocol,
        "endpoint_generation": endpoint_generation,
        "tag": tag,
        "prerelease": False,
        "assets": assets,
    }
    build["release_version"] = release_version
    builds[build_id] = build
    ordered_builds = {
        key: builds[key]
        for key in sorted(
            builds,
            key=lambda key: str(builds[key].get("built_at", "")),
            reverse=True,
        )[:retain]
    }
    manifest = {
        "schema_version": 1,
        "channel": "preview",
        "prerelease": False,
        "base_version": normalize_version(base_version),
        "build_id": build_id,
        "commit": commit,
        "built_at": built_at,
        "protocol": protocol,
        "endpoint_generation": endpoint_generation,
        "notes": notes.strip(),
        "assets": assets,
        "builds": ordered_builds,
    }
    manifest["release_version"] = release_version
    return json.dumps(manifest, indent=2) + "\n"


def cmd_notes(args: argparse.Namespace) -> int:
    previous = args.previous or previous_preview_commit(Path(args.manifest)) or latest_stable_tag()
    notes = build_notes(previous, args.commit, args.build_id, args.base_version, args.repo)
    Path(args.output).write_text(notes, encoding="utf-8")
    return 0


def cmd_manifest(args: argparse.Namespace) -> int:
    notes = Path(args.notes).read_text(encoding="utf-8")
    shas = read_sha_file(Path(args.sha_file) if args.sha_file else None)
    content = build_manifest(
        output=Path(args.output),
        repo=args.repo,
        tag=args.tag,
        build_id=args.build_id,
        commit=args.commit,
        built_at=args.built_at,
        base_version=args.base_version,
        protocol=args.protocol,
        endpoint_generation=args.endpoint_generation,
        notes=notes,
        shas=shas,
        retain=args.retain,
        release_version=args.release_version,
    )
    Path(args.output).write_text(content, encoding="utf-8")
    return 0


def cmd_current_commit(args: argparse.Namespace) -> int:
    commit = previous_preview_commit(Path(args.manifest))
    if commit:
        print(commit)
    return 0


def cmd_endpoint_generation(args: argparse.Namespace) -> int:
    print(read_endpoint_protocol_generation(Path(args.source)))
    return 0


def cmd_select_commit(args: argparse.Namespace) -> int:
    print(latest_publishable_commit(args.ref))
    return 0


def cmd_range_base(args: argparse.Namespace) -> int:
    print(preview_range_base(args.previous, args.commit))
    return 0


def cmd_asset_names(args: argparse.Namespace) -> int:
    print(json.dumps(herdr_ext_asset_names(args.release_version), sort_keys=True))
    return 0


def cmd_require_newer_release(args: argparse.Namespace) -> int:
    current = read_json(Path(args.manifest)) or {}
    if "release_version" not in current:
        raise ValueError("current manifest is missing release_version")
    require_newer_herdr_win_release(args.release_version, current)
    return 0


def cmd_candidate_build_id(args: argparse.Namespace) -> int:
    print(
        candidate_build_id(
            args.upstream_sha,
            args.control_sha,
            args.run_id,
            args.run_attempt,
        )
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Preview channel release helpers")
    sub = parser.add_subparsers(required=True)

    notes = sub.add_parser("notes")
    notes.add_argument("--manifest", default="website/preview.json")
    notes.add_argument("--previous")
    notes.add_argument("--commit", required=True)
    notes.add_argument("--build-id", required=True)
    notes.add_argument("--base-version", required=True)
    notes.add_argument("--repo", default="herdrdev/herdr")
    notes.add_argument("--output", required=True)
    notes.set_defaults(func=cmd_notes)

    manifest = sub.add_parser("manifest")
    manifest.add_argument("--output", default="website/preview.json")
    manifest.add_argument("--repo", default="herdrdev/herdr")
    manifest.add_argument("--tag", required=True)
    manifest.add_argument("--build-id", required=True)
    manifest.add_argument("--commit", required=True)
    manifest.add_argument("--built-at", required=True)
    manifest.add_argument("--base-version", required=True)
    manifest.add_argument("--protocol", required=True, type=int)
    manifest.add_argument("--endpoint-generation", required=True, type=int)
    manifest.add_argument("--notes", required=True)
    manifest.add_argument("--sha-file")
    manifest.add_argument("--retain", type=int, default=30)
    manifest.add_argument("--release-version", required=True)
    manifest.set_defaults(func=cmd_manifest)

    endpoint_generation = sub.add_parser("endpoint-generation")
    endpoint_generation.add_argument("--source", default=str(ENDPOINT_PROTOCOL_SOURCE_PATH))
    endpoint_generation.set_defaults(func=cmd_endpoint_generation)

    asset_names = sub.add_parser("herdr-ext-asset-names")
    asset_names.add_argument("--release-version", required=True)
    asset_names.set_defaults(func=cmd_asset_names)

    newer_release = sub.add_parser("require-newer-herdr-win-release")
    newer_release.add_argument("--release-version", required=True)
    newer_release.add_argument("--manifest", default="website/preview.json")
    newer_release.set_defaults(func=cmd_require_newer_release)

    build_id = sub.add_parser("candidate-build-id")
    build_id.add_argument("--upstream-sha", required=True)
    build_id.add_argument("--control-sha", required=True)
    build_id.add_argument("--run-id", required=True)
    build_id.add_argument("--run-attempt", required=True, type=int)
    build_id.set_defaults(func=cmd_candidate_build_id)

    current = sub.add_parser("current-commit")
    current.add_argument("--manifest", default="website/preview.json")
    current.set_defaults(func=cmd_current_commit)

    select = sub.add_parser("select-commit")
    select.add_argument("--ref", default="origin/master")
    select.set_defaults(func=cmd_select_commit)

    range_base = sub.add_parser("range-base")
    range_base.add_argument("--previous", required=True)
    range_base.add_argument("--commit", required=True)
    range_base.set_defaults(func=cmd_range_base)

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
