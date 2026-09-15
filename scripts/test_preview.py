import json
import os
import subprocess
import tempfile
import unittest
from unittest import mock
from pathlib import Path

import scripts.conventional_commits as conventional_commits
import scripts.preview as preview

VALID_SHAS = {
    target: f"{index:x}" * 64
    for index, target in enumerate(preview.ASSET_TARGETS, start=1)
}
VALID_RELEASE_VERSION = "2026.06.02.1"
VALID_RELEASE_TAG = f"v{VALID_RELEASE_VERSION}"


class PreviewNotesTests(unittest.TestCase):
    def test_humanize_groups_conventional_subjects(self):
        self.assertEqual(
            preview.humanize_subject("feat(update): add preview channel"),
            ("Added", "Add preview channel"),
        )
        self.assertEqual(
            preview.humanize_subject("fix: handle preview manifest"),
            ("Fixed", "Handle preview manifest"),
        )
        self.assertEqual(
            preview.humanize_subject("not conventional"),
            ("Other", "Not conventional"),
        )

    def test_build_manifest_archives_current_assets(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "preview.json"
            notes = "Preview notes\n"
            content = preview.build_manifest(
                output=output,
                repo="herdrdev/herdr",
                tag=VALID_RELEASE_TAG,
                build_id="abcdef123456.7890abcdef12",
                commit="abcdef1234567890",
                built_at="2026-06-02T03:00:00Z",
                base_version="0.6.6",
                protocol=12,
                endpoint_generation=7,
                notes=notes,
                shas=VALID_SHAS,
                retain=30,
                release_version=VALID_RELEASE_VERSION,
            )
            data = json.loads(content)
            self.assertEqual(data["channel"], "preview")
            self.assertIs(data["prerelease"], False)
            self.assertEqual(data["release_version"], "2026.06.02.1")
            self.assertEqual(data["build_id"], "abcdef123456.7890abcdef12")
            self.assertEqual(data["endpoint_generation"], 7)
            self.assertEqual(
                data["builds"][data["build_id"]]["endpoint_generation"], 7
            )
            self.assertEqual(
                set(data["assets"]), set(preview.ASSET_TARGETS),
            )
            self.assertEqual(
                data["assets"]["linux-x86_64"]["url"],
                "https://github.com/herdrdev/herdr/releases/download/v2026.06.02.1/herdr-ext_v2026.06.02.1_linux_amd64",
            )
            self.assertEqual(
                data["assets"]["linux-x86_64"]["sha256"],
                VALID_SHAS["linux-x86_64"],
            )
            self.assertEqual(
                data["assets"]["windows-x86_64"]["url"],
                "https://github.com/herdrdev/herdr/releases/download/v2026.06.02.1/herdr-ext_v2026.06.02.1_windows_amd64.zip",
            )
            self.assertEqual(
                data["assets"]["windows-x86_64"]["sha256"],
                VALID_SHAS["windows-x86_64"],
            )
            self.assertEqual(data["assets"]["windows-x86_64"]["format"], "zip")
            self.assertEqual(
                data["assets"]["windows-x86_64-installer"]["url"],
                "https://github.com/herdrdev/herdr/releases/download/v2026.06.02.1/herdr-ext_v2026.06.02.1_windows_amd64_setup.exe",
            )
            self.assertEqual(
                data["assets"]["windows-x86_64-installer"]["format"], "nsis"
            )
            self.assertNotEqual(
                data["assets"]["windows-x86_64"]["sha256"],
                data["assets"]["windows-x86_64-installer"]["sha256"],
            )
            self.assertIn("abcdef123456.7890abcdef12", data["builds"])
            self.assertEqual(
                data["builds"]["abcdef123456.7890abcdef12"]["release_version"],
                "2026.06.02.1",
            )
            self.assertIs(
                data["builds"]["abcdef123456.7890abcdef12"]["prerelease"],
                False,
            )

    def test_herdr_ext_asset_names_require_real_calver(self):
        self.assertEqual(
            preview.herdr_ext_asset_names("2026.07.31.1"),
            {
                "linux-x86_64": "herdr-ext_v2026.07.31.1_linux_amd64",
                "linux-aarch64": "herdr-ext_v2026.07.31.1_linux_arm64",
                "macos-x86_64": "herdr-ext_v2026.07.31.1_macos_amd64",
                "macos-aarch64": "herdr-ext_v2026.07.31.1_macos_arm64",
                "windows-x86_64": "herdr-ext_v2026.07.31.1_windows_amd64.zip",
                "windows-x86_64-installer": (
                    "herdr-ext_v2026.07.31.1_windows_amd64_setup.exe"
                ),
            },
        )
        self.assertIn(
            "65535",
            preview.herdr_ext_asset_names("2026.07.31.65535")[
                "windows-x86_64-installer"
            ],
        )
        for invalid in (
            "v2026.07.31.1",
            "2026.02.30.1",
            "2026.07.31.0",
            "2026.07.31.+1",
            "2026.07.31.65536",
        ):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(
                ValueError, "release_version"
            ):
                preview.herdr_ext_asset_names(invalid)

    def test_endpoint_generation_reads_source_and_rejects_invalid_u32(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "endpoint.rs"
            for value in (1, 7, 0xFFFFFFFF):
                with self.subTest(value=value):
                    source.write_text(
                        f"pub const ENDPOINT_PROTOCOL_GENERATION: u32 = {value};\n",
                        encoding="utf-8",
                    )
                    self.assertEqual(preview.read_endpoint_protocol_generation(source), value)
            for value in ("0", "4294967296", "-1", "1.5", "true", "missing"):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    source.write_text(
                        f"pub const ENDPOINT_PROTOCOL_GENERATION: u32 = {value};\n",
                        encoding="utf-8",
                    )
                    preview.read_endpoint_protocol_generation(source)

    def test_manifest_rejects_invalid_endpoint_generation(self):
        with tempfile.TemporaryDirectory() as tmp:
            for value in (None, True, "1", 0, -1, 1.5, 0x100000000):
                with self.subTest(value=value), self.assertRaisesRegex(
                    ValueError, "endpoint_generation must be a positive u32 integer"
                ):
                    preview.build_manifest(
                        output=Path(tmp) / "preview.json",
                        repo="hdosys/herdr-ext",
                        tag=VALID_RELEASE_TAG,
                        build_id="abcdef123456.7890abcdef12",
                        commit="abcdef",
                        built_at="2026-06-02T03:00:00Z",
                        base_version="0.9.0",
                        protocol=12,
                        endpoint_generation=value,
                        notes="test",
                        shas=VALID_SHAS,
                        retain=1,
                        release_version=VALID_RELEASE_VERSION,
                    )

    def test_release_gate_requires_newer_calver(self):
        current = {"release_version": "2026.08.05.5"}
        for stale in ("2026.08.05.5", "2026.08.05.4", "2026.08.04.99"):
            with self.subTest(stale=stale), self.assertRaisesRegex(
                ValueError, "must be newer"
            ):
                preview.require_newer_herdr_win_release(stale, current)
        preview.require_newer_herdr_win_release("2026.08.05.6", current)
        preview.require_newer_herdr_win_release("2026.08.05.6", {})
        for invalid in (20260805, "bad"):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(
                ValueError, "current manifest has an invalid release_version"
            ):
                preview.require_newer_herdr_win_release(
                    "2026.08.05.6", {"release_version": invalid}
                )

    def test_preview_assets_require_sha256(self):
        with tempfile.TemporaryDirectory() as tmp:
            for target in preview.ASSET_TARGETS:
                shas = dict(VALID_SHAS)
                shas.pop(target)
                with self.subTest(target=target), self.assertRaisesRegex(
                    ValueError, f"{target} requires"
                ):
                    preview.build_manifest(
                        output=Path(tmp) / "preview.json",
                        repo="herdrdev/herdr",
                        tag=VALID_RELEASE_TAG,
                        build_id="abcdef123456.7890abcdef12",
                        commit="abcdef",
                        built_at="2026-06-02T03:00:00Z",
                        base_version="0.6.6",
                        protocol=12,
                        endpoint_generation=1,
                        notes="test",
                        shas=shas,
                        retain=1,
                        release_version=VALID_RELEASE_VERSION,
                    )

            invalid = dict(VALID_SHAS)
            invalid["windows-x86_64-installer"] = "B" * 64
            with self.assertRaisesRegex(
                ValueError, "windows-x86_64-installer requires"
            ):
                preview.build_manifest(
                    output=Path(tmp) / "preview.json",
                    repo="herdrdev/herdr",
                    tag=VALID_RELEASE_TAG,
                    build_id="abcdef123456.7890abcdef12",
                    commit="abcdef",
                    built_at="2026-06-02T03:00:00Z",
                    base_version="0.6.6",
                    protocol=12,
                    endpoint_generation=1,
                    notes="test",
                    shas=invalid,
                    retain=1,
                    release_version=VALID_RELEASE_VERSION,
                )

    def test_manifest_binds_tag_to_release_version(self):
        with tempfile.TemporaryDirectory() as tmp, self.assertRaisesRegex(
            ValueError, "tag must be v2026.06.02.1"
        ):
            preview.build_manifest(
                output=Path(tmp) / "preview.json",
                repo="herdrdev/herdr",
                tag="v2026.06.02.2",
                build_id="abcdef123456.7890abcdef12",
                commit="abcdef",
                built_at="2026-06-02T03:00:00Z",
                base_version="0.6.6",
                protocol=12,
                endpoint_generation=1,
                notes="test",
                shas=VALID_SHAS,
                retain=1,
                release_version=VALID_RELEASE_VERSION,
            )

    def test_manifest_preserves_legacy_zip_only_archived_build(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "preview.json"
            legacy_id = "111111111111.aaaaaaaaaaaa"
            output.write_text(
                json.dumps(
                    {
                        "builds": {
                            legacy_id: {
                                "built_at": "2026-06-01T03:00:00Z",
                                "assets": {
                                    "windows-x86_64": {
                                        "url": "https://example.test/legacy.zip",
                                        "sha256": "c" * 64,
                                        "format": "zip",
                                    }
                                },
                            }
                        }
                    }
                ),
                encoding="utf-8",
            )
            content = preview.build_manifest(
                output=output,
                repo="herdrdev/herdr",
                tag=VALID_RELEASE_TAG,
                build_id="abcdef123456.7890abcdef12",
                commit="abcdef",
                built_at="2026-06-02T03:00:00Z",
                base_version="0.6.6",
                protocol=12,
                endpoint_generation=1,
                notes="test",
                shas=VALID_SHAS,
                retain=2,
                release_version=VALID_RELEASE_VERSION,
            )
            data = json.loads(content)
            self.assertNotIn("endpoint_generation", data["builds"][legacy_id])
            self.assertEqual(
                set(data["builds"][legacy_id]["assets"]), {"windows-x86_64"}
            )
            self.assertEqual(
                set(data["builds"]["abcdef123456.7890abcdef12"]["assets"]),
                set(preview.ASSET_TARGETS),
            )

    def test_manifest_build_id_uses_two_hex_components(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(ValueError, "two lowercase 12-hex"):
                preview.build_manifest(
                    output=Path(tmp) / "preview.json",
                    repo="herdrdev/herdr",
                    tag=VALID_RELEASE_TAG,
                    build_id="2026-06-02-abcdef123456",
                    commit="abcdef",
                    built_at="2026-06-02T03:00:00Z",
                    base_version="0.6.6",
                    protocol=12,
                    endpoint_generation=1,
                    notes="test",
                    shas=VALID_SHAS,
                    retain=1,
                    release_version=VALID_RELEASE_VERSION,
                )

    def test_candidate_build_id_is_stable_and_attempt_scoped(self):
        upstream = "a" * 40
        control = "b" * 40

        first = preview.candidate_build_id(upstream, control, "123456789", 1)
        repeated = preview.candidate_build_id(upstream, control, "123456789", 1)
        retry = preview.candidate_build_id(upstream, control, "123456789", 2)

        self.assertEqual(first, "aaaaaaaaaaaa.cd2554cf7a34")
        self.assertEqual(repeated, first)
        self.assertEqual(retry, "aaaaaaaaaaaa.86029009c362")
        self.assertNotEqual(retry, first)

    def test_candidate_build_id_rejects_invalid_identity(self):
        with self.assertRaisesRegex(ValueError, "upstream_sha"):
            preview.candidate_build_id("bad", "b" * 40, "1", 1)
        with self.assertRaisesRegex(ValueError, "control_sha"):
            preview.candidate_build_id("a" * 40, "bad", "1", 1)
        with self.assertRaisesRegex(ValueError, "run_id"):
            preview.candidate_build_id("a" * 40, "b" * 40, "0", 1)
        with self.assertRaisesRegex(ValueError, "run_attempt"):
            preview.candidate_build_id("a" * 40, "b" * 40, "1", 0)

    def test_hidden_subjects_include_preview_manifest_commits(self):
        self.assertTrue(preview.hidden_subject("docs: update preview manifest"))
        self.assertTrue(preview.hidden_subject("docs: update website manifest"))
        self.assertFalse(preview.hidden_subject("release: v0.7.0"))
        self.assertFalse(preview.hidden_subject("fix: repair preview manifest"))

    def test_latest_publishable_commit_keeps_release_commits(self):
        output = "\n".join(
            [
                "manifest\x00docs: update website manifest for v0.7.0",
                "release\x00release: v0.7.0",
                "feature\x00feat: add plugin v1 system",
            ]
        )
        with mock.patch.object(preview, "run_git", return_value=output):
            self.assertEqual(preview.latest_publishable_commit("origin/master"), "release")

    def test_preview_range_base_advances_to_stable_tag(self):
        with (
            mock.patch.object(preview, "latest_stable_tag", return_value="v0.7.0"),
            mock.patch.object(preview, "git_is_ancestor", return_value=True),
        ):
            self.assertEqual(
                preview.preview_range_base("previous-preview", "release"),
                "v0.7.0",
            )

    def test_preview_range_base_keeps_previous_preview_for_unreleased_work(self):
        def is_ancestor(ancestor: str, descendant: str) -> bool:
            return (ancestor, descendant) == ("v0.7.0", "new-feature")

        with (
            mock.patch.object(preview, "latest_stable_tag", return_value="v0.7.0"),
            mock.patch.object(preview, "git_is_ancestor", side_effect=is_ancestor),
        ):
            self.assertEqual(
                preview.preview_range_base("previous-preview", "new-feature"),
                "previous-preview",
            )

    def test_post_stable_history_selects_release_and_bases_range_on_stable_tag(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)

            def git(*args: str) -> str:
                return subprocess.check_output(
                    ["git", *args],
                    cwd=repo,
                    text=True,
                    stderr=subprocess.DEVNULL,
                ).strip()

            git("init")
            git("config", "user.email", "test@example.com")
            git("config", "user.name", "Test User")

            marker = repo / "marker.txt"
            marker.write_text("preview\n", encoding="utf-8")
            git("add", "marker.txt")
            git("commit", "-m", "feat: previous preview")
            previous_preview = git("rev-parse", "HEAD")

            marker.write_text("release\n", encoding="utf-8")
            git("commit", "-am", "release: v0.7.0")
            release = git("rev-parse", "HEAD")
            git("tag", "v0.7.0")

            marker.write_text("manifest\n", encoding="utf-8")
            git("commit", "-am", "docs: update website manifest for v0.7.0")

            original_cwd = os.getcwd()
            try:
                os.chdir(repo)
                self.assertEqual(preview.latest_publishable_commit("HEAD"), release)
                self.assertEqual(
                    preview.preview_range_base(previous_preview, release),
                    "v0.7.0",
                )
            finally:
                os.chdir(original_cwd)

    def test_preview_docs_rewrite_links_to_preview_namespace(self):
        source = """---
title: Install Herdr
---

import ConfigReference from '../../components/ConfigReference.astro';
import LocaleWidget from '../../../components/LocaleWidget.astro';

[Install](/docs/install/)
file: ../../../public/assets/logo.svg
"""
        output = subprocess.check_output(
            ["node", "website/scripts/prepare-docs.mjs", "--rewrite-preview-doc-fixture"],
            input=source,
            text=True,
        )
        self.assertIn("[Install](/docs/preview/install/)", output)
        self.assertIn("file: ../../../../public/assets/logo.svg", output)
        self.assertIn("from '../../../components/ConfigReference.astro'", output)
        self.assertIn("from '../../../../components/LocaleWidget.astro'", output)
        self.assertIn("Next docs describe unreleased work", output)
        self.assertIn("edit/master/docs/next/website/src/content/docs/", output)

    def test_version_docs_rewrite_links_and_source_paths(self):
        source = """---
title: Install Herdr
---

import ConfigReference from '../../components/ConfigReference.astro';

[Install](/docs/install/)
[Skill](https://github.com/ogulcancelik/herdr/blob/master/SKILL.md)
file: ../../../public/assets/logo.svg
"""
        output = subprocess.check_output(
            [
                "node",
                "website/scripts/prepare-docs.mjs",
                "--rewrite-version-doc-fixture",
                "0.7.4",
            ],
            input=source,
            text=True,
        )
        self.assertIn("[Install](/docs/0.7.4/install/)", output)
        self.assertIn("file: ../../../../../public/assets/logo.svg", output)
        self.assertIn("from '../../../../components/ConfigReference.astro'", output)
        self.assertIn("blob/v0.7.4/docs/next/website/src/content/docs/index.mdx", output)
        self.assertIn("blob/v0.7.4/SKILL.md", output)


class ConventionalCommitTests(unittest.TestCase):
    def test_valid_subjects_allow_scopes_and_bang(self):
        self.assertTrue(conventional_commits.valid_subject("fix(update): handle preview"))
        self.assertTrue(conventional_commits.valid_subject("feat!: change config"))
        self.assertFalse(conventional_commits.valid_subject("update preview channel"))

    def test_commit_message_subject_skips_comments(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "COMMIT_EDITMSG"
            path.write_text(
                "\n# Please enter the commit message\n\nfix(update): switch channel\n",
                encoding="utf-8",
            )
            self.assertEqual(
                conventional_commits.commit_message_subject(path),
                "fix(update): switch channel",
            )


if __name__ == "__main__":
    unittest.main()
