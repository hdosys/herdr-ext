from __future__ import annotations

import json
import re
import unittest
from pathlib import Path

from scripts.delta_workflow import CONTROL_PATH_PREFIXES, CONTROL_PATHS


PROJECT_ROOT = Path(__file__).resolve().parent.parent
DELTA_ROOT = PROJECT_ROOT / "patches" / "delta"
PATCH_NAME = re.compile(r"^[0-9]{4}-[a-z0-9-]+\.patch$")
MAILBOX_FROM = re.compile(r"^From [0-9a-f]{40} Mon Sep 17 00:00:00 2001$")
DIFF_PATH = re.compile(r"^diff --git a/(.+?) b/(.+)$", re.MULTILINE)
FORK_RELEASE_PREFIXES = (
    "https://github.com/hdosys/herdr-ext/releases/download/",
    "https://github.com/hdosys/herdr-win/releases/download/",
    "https://github.com/User-3090/herdr-win/releases/download/",
)
PORTABLE_TARGET_NAMES = {
    "linux-x86_64": re.compile(
        r"^herdr-win_v[0-9]{4}\.[0-9]{2}\.[0-9]{2}\.[1-9][0-9]*_linux_amd64$"
    ),
    "linux-aarch64": re.compile(
        r"^herdr-win_v[0-9]{4}\.[0-9]{2}\.[0-9]{2}\.[1-9][0-9]*_linux_arm64$"
    ),
    "macos-x86_64": re.compile(
        r"^herdr-win_v[0-9]{4}\.[0-9]{2}\.[0-9]{2}\.[1-9][0-9]*_macos_amd64$"
    ),
    "macos-aarch64": re.compile(
        r"^herdr-win_v[0-9]{4}\.[0-9]{2}\.[0-9]{2}\.[1-9][0-9]*_macos_arm64$"
    ),
}
WINDOWS_ZIP_TARGET = "windows-x86_64"
WINDOWS_INSTALLER_TARGET = "windows-x86_64-installer"
RELEASE_TARGETS = {
    *PORTABLE_TARGET_NAMES,
    WINDOWS_ZIP_TARGET,
    WINDOWS_INSTALLER_TARGET,
}
WINDOWS_SETUP_NAME = re.compile(
    r"^herdr-win_v[0-9]{4}\.[0-9]{2}\.[0-9]{2}\.[1-9][0-9]*_windows_amd64_setup\.exe$"
)


def series_entries() -> list[str]:
    return [
        line
        for raw_line in (DELTA_ROOT / "series").read_text(encoding="utf-8").splitlines()
        if (line := raw_line.strip()) and not line.startswith("#")
    ]


class DeltaPatchTests(unittest.TestCase):
    def test_base_is_a_full_commit_id(self) -> None:
        base = (DELTA_ROOT / "BASE").read_text(encoding="utf-8").strip()
        self.assertRegex(base, r"^[0-9a-f]{40}$")

    def test_series_is_unique_and_complete(self) -> None:
        entries = series_entries()
        self.assertGreater(len(entries), 0)
        self.assertEqual(len(entries), len(set(entries)))
        for entry in entries:
            self.assertRegex(entry, PATCH_NAME)
        self.assertEqual(
            sorted(entries), sorted(path.name for path in DELTA_ROOT.glob("*.patch"))
        )

    def test_mailboxes_are_full_git_patches_without_control_plane_files(self) -> None:
        entries = series_entries()
        commits = []
        for position, entry in enumerate(entries, start=1):
            text = (DELTA_ROOT / entry).read_text(encoding="utf-8")
            first_line = text.splitlines()[0]
            self.assertRegex(first_line, MAILBOX_FROM, entry)
            commits.append(first_line.split()[1])
            self.assertIn(f"\nSubject: [PATCH {position}/{len(entries)}] ", text, entry)
            paths = {
                path
                for before, after in DIFF_PATH.findall(text)
                for path in (before, after)
            }
            self.assertGreater(len(paths), 0, entry)
            disallowed = sorted(
                path
                for path in paths
                if path in CONTROL_PATHS
                or path.startswith(CONTROL_PATH_PREFIXES)
            )
            self.assertEqual(disallowed, [], entry)
        self.assertEqual(len(commits), len(set(commits)))

    def test_preview_manifest_is_bootstrap_empty_or_fork_owned(self) -> None:
        manifest = json.loads(
            (PROJECT_ROOT / "website" / "preview.json").read_text(encoding="utf-8")
        )
        if manifest == {}:
            return

        self.assertEqual(manifest.get("channel"), "preview")
        self.assertRegex(
            str(manifest.get("build_id", "")), r"^[0-9a-f]{12}\.[0-9a-f]{12}$"
        )
        asset_groups = [manifest.get("assets", {})]
        asset_groups.extend(
            build.get("assets", {}) for build in manifest.get("builds", {}).values()
        )
        for assets in asset_groups:
            self.assertIsInstance(assets, dict)
            self.assertIn(WINDOWS_ZIP_TARGET, assets)
            self.assertLessEqual(set(assets), RELEASE_TARGETS)
            for target, name_pattern in PORTABLE_TARGET_NAMES.items():
                if target not in assets:
                    continue
                portable = assets[target]
                self.assertTrue(str(portable.get("url", "")).startswith(FORK_RELEASE_PREFIXES))
                self.assertRegex(
                    str(portable.get("url", "")).rsplit("/", 1)[-1], name_pattern
                )
                self.assertRegex(str(portable.get("sha256", "")), r"^[0-9a-f]{64}$")
            windows = assets[WINDOWS_ZIP_TARGET]
            self.assertTrue(str(windows.get("url", "")).startswith(FORK_RELEASE_PREFIXES))
            self.assertRegex(str(windows.get("sha256", "")), r"^[0-9a-f]{64}$")
            self.assertEqual(windows.get("format"), "zip")
            if WINDOWS_INSTALLER_TARGET in assets:
                installer = assets[WINDOWS_INSTALLER_TARGET]
                self.assertTrue(
                    str(installer.get("url", "")).startswith(FORK_RELEASE_PREFIXES)
                )
                self.assertRegex(
                    str(installer.get("url", "")).rsplit("/", 1)[-1],
                    WINDOWS_SETUP_NAME,
                )
                self.assertRegex(
                    str(installer.get("sha256", "")), r"^[0-9a-f]{64}$"
                )
                self.assertEqual(installer.get("format"), "nsis")

if __name__ == "__main__":
    unittest.main()
