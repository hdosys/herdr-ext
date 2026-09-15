from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts.delta_workflow import (
    DEVELOPMENT_BRANCH,
    DeltaWorkflowError,
)
from scripts.local_windows_installer import (
    CANDIDATE_STAMP,
    DEFAULT_PATHS,
    InstallerIdentity,
    InstallerPaths,
    LocalInstallerError,
    OUTPUT_PATH,
    TARGET_ROOT,
    _bundle_manifest,
    _candidate_build_id,
    _candidate_paths,
    _canonical_candidate_identity,
    _cargo_build_arguments,
    _cargo_test_arguments,
    _dynamic_msvc_runtime_imports,
    _git_arguments,
    _hashes,
    _isolated_candidate_paths,
    _just_test_arguments,
    _parser,
    _portable_pty_test_arguments,
    _prune_completed_candidate_outputs,
    _require_integrated_development_source,
    _require_candidate_cache_budget,
    _require_one_focused_test,
    _require_one_nextest_test,
    _run_normal_focused_test,
    _run_portable_pty_focused_test,
    _source_fingerprint,
    _source_branch,
    candidate,
    parse_identity,
    test_one,
)


BUILD_ID = "0123456789ab.cdef01234567"
BUILD_FRESHNESS = "2026.08.28.1045Z"
SOURCE_FINGERPRINT = "b" * 64
BUILD_NONCE = "c" * 32


class LocalWindowsInstallerTests(unittest.TestCase):
    def test_candidate_cache_budget_preserves_cache_on_resource_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cache = root / "x86_64-pc-windows-msvc/release/incremental"
            cache.mkdir(parents=True)
            data = cache / "retained.bin"
            data.write_bytes(b"retained")
            (cache / "compiler.lock").write_bytes(b"")
            with patch("scripts.local_windows_installer.shutil.disk_usage") as usage:
                usage.return_value.free = 0
                with self.assertRaisesRegex(LocalInstallerError, "at least 2 GiB free"):
                    _require_candidate_cache_budget(root)
                usage.return_value.free = 4 * 1024**3
                with patch("scripts.local_windows_installer.MAXIMUM_CANDIDATE_INCREMENTAL_CACHE_BYTES", 1):
                    with self.assertRaisesRegex(LocalInstallerError, "iteration budget"):
                        _require_candidate_cache_budget(root)
                _require_candidate_cache_budget(root)
            self.assertEqual(data.read_bytes(), b"retained")

    def test_identity_parser_accepts_only_local_candidate_contract(self) -> None:
        self.assertEqual(
            parse_identity(
                f"herdr-ext {BUILD_FRESHNESS} (local, Herdr 0.8.0, build {BUILD_ID})",
                BUILD_ID,
            ),
            InstallerIdentity(BUILD_ID, "0.8.0", BUILD_FRESHNESS),
        )
        with self.assertRaisesRegex(LocalInstallerError, "not an exact local build"):
            parse_identity("herdr-ext 2026.08.15.1 (Herdr 0.8.0)", BUILD_ID)
        with self.assertRaisesRegex(LocalInstallerError, "different local build"):
            parse_identity(
                f"herdr-ext {BUILD_FRESHNESS} (local, Herdr 0.8.0, build aaaaaaaaaaaa.bbbbbbbbbbbb)",
                BUILD_ID,
            )

    def test_bundle_manifest_detects_input_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            bundle = Path(temporary)
            stage = bundle / "stage"
            stage.mkdir()
            (stage / "herdr.exe").write_bytes(b"runtime")
            (bundle / "herdr-launcher.exe").write_bytes(b"launcher")
            (bundle / "herdr-installer-helper.exe").write_bytes(b"helper")
            files = _hashes(bundle)
            (bundle / "bundle.json").write_text(
                json.dumps(
                    {
                        "schema": 2,
                        "build_id": BUILD_ID,
                        "base_version": "0.8.0",
                        "build_freshness": BUILD_FRESHNESS,
                        "files": files,
                    }
                ),
                encoding="utf-8",
            )

            identity, expected = _bundle_manifest(bundle)
            self.assertEqual(
                identity, InstallerIdentity(BUILD_ID, "0.8.0", BUILD_FRESHNESS)
            )
            self.assertEqual(_hashes(bundle), expected)

            (stage / "herdr.exe").write_bytes(b"changed")
            self.assertNotEqual(_hashes(bundle), expected)
            self.assertEqual(
                expected["stage/herdr.exe"], hashlib.sha256(b"runtime").hexdigest()
            )

    def test_default_output_is_one_short_replaceable_candidate_path(self) -> None:
        self.assertEqual(OUTPUT_PATH.name, "herdr-win_local_candidate_setup.exe")
        self.assertEqual(DEFAULT_PATHS.output_path, OUTPUT_PATH)

    def test_packaging_commands_expose_one_product_name_input(self) -> None:
        parser = _parser()
        common = ["--source-worktree", "C:/development"]
        for command, arguments in (
            (
                "build",
                [*common, "--input-bundle", "C:/bundle"],
            ),
            (
                "release-precheck",
                [*common, "--input-bundle", "C:/bundle"],
            ),
            ("candidate", common),
        ):
            defaults = parser.parse_args([command, *arguments])
            custom = parser.parse_args(
                [command, *arguments, "--product-name", "Herdr Quality Test"]
            )
            self.assertIsNone(defaults.product_name)
            self.assertEqual(custom.product_name, "Herdr Quality Test")

    def test_isolated_candidate_paths_share_one_build_scoped_root(self) -> None:
        paths = _isolated_candidate_paths(BUILD_ID)
        expected_root = TARGET_ROOT / "isolated" / BUILD_ID

        self.assertEqual(paths.target_root, expected_root)
        self.assertEqual(paths.input_root, expected_root / "installer-inputs")
        self.assertEqual(
            paths.output_path,
            expected_root / "release" / "herdr-win_local_candidate_setup.exe",
        )
        self.assertEqual(paths.nsis_cache, expected_root / "tools" / "nsis-3.12")

    def test_isolated_candidate_paths_reject_unbounded_names(self) -> None:
        with self.assertRaisesRegex(LocalInstallerError, "invalid candidate build ID"):
            _isolated_candidate_paths("../other-session")

    def test_individual_candidate_defaults_to_isolated_outputs(self) -> None:
        self.assertEqual(
            _candidate_paths(
                "agent/delta-one-fix", BUILD_ID, isolated=False
            ),
            _isolated_candidate_paths(BUILD_ID),
        )

    def test_development_branch_defaults_to_canonical_output(self) -> None:
        self.assertEqual(
            _candidate_paths(
                DEVELOPMENT_BRANCH,
                BUILD_ID,
                isolated=False,
            ),
            DEFAULT_PATHS,
        )

    def test_development_branch_can_force_isolated_outputs(self) -> None:
        self.assertEqual(
            _candidate_paths(
                DEVELOPMENT_BRANCH,
                BUILD_ID,
                isolated=True,
            ),
            _isolated_candidate_paths(BUILD_ID),
        )

    def test_development_installer_requires_clean_integrated_source(self) -> None:
        source = Path("C:/development")
        with (
            patch("scripts.local_windows_installer._git") as git,
            patch("scripts.local_windows_installer._run") as run,
            patch(
                "scripts.local_windows_installer.unintegrated_topic_worktrees",
                return_value=(),
            ),
        ):
            git.side_effect = ["", "a" * 40]

            _require_integrated_development_source(
                source, DEVELOPMENT_BRANCH, isolated=False
            )

            run.assert_not_called()

        with (
            patch("scripts.local_windows_installer._git") as git,
            patch("scripts.local_windows_installer._run"),
            self.assertRaisesRegex(LocalInstallerError, "must be clean"),
        ):
            git.return_value = " M src/main.rs"
            _require_integrated_development_source(
                source, DEVELOPMENT_BRANCH, isolated=False
            )

    def test_source_branch_accepts_the_cumulative_development_branch(self) -> None:
        with patch(
            "scripts.local_windows_installer._git", return_value=DEVELOPMENT_BRANCH
        ):
            self.assertEqual(_source_branch(Path("C:/development")), DEVELOPMENT_BRANCH)

    def test_source_writer_branch_is_accepted_only_for_test_command(self) -> None:
        with patch("scripts.local_windows_installer._git", return_value="agent/v090-stabilization"):
            with self.assertRaisesRegex(LocalInstallerError, "unsupported branch"):
                _source_branch(Path("C:/writer"))
            self.assertEqual(
                _source_branch(Path("C:/writer"), test_only=True),
                "agent/v090-stabilization",
            )

    def test_legacy_combined_branch_is_not_canonical(self) -> None:
        self.assertEqual(
            _candidate_paths(
                "agent/delta-combined-acceptance-20260823",
                BUILD_ID,
                isolated=False,
            ),
            _isolated_candidate_paths(BUILD_ID),
        )

    def test_candidate_identity_tracks_source_and_build_freshness(self) -> None:
        base = "a" * 40
        source = "b" * 40
        source_fingerprint = _source_fingerprint(source, "", [])
        clean = _candidate_build_id(
            base, source_fingerprint, BUILD_FRESHNESS, BUILD_NONCE
        )

        self.assertRegex(clean, r"^a{12}\.[0-9a-f]{12}$")
        self.assertNotEqual(source_fingerprint, _source_fingerprint(source, "diff", []))
        self.assertNotEqual(
            source_fingerprint,
            _source_fingerprint(source, "", [("src/new.rs", b"content")]),
        )
        self.assertNotEqual(
            clean,
            _candidate_build_id(
                base, source_fingerprint, "2026.08.28.1046Z", BUILD_NONCE
            ),
        )
        self.assertNotEqual(
            clean,
            _candidate_build_id(
                base, source_fingerprint, BUILD_FRESHNESS, "d" * 32
            ),
        )

    def test_incomplete_candidate_reuses_its_exact_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            stamp = Path(temporary) / CANDIDATE_STAMP.name
            with (
                patch("scripts.local_windows_installer.CANDIDATE_STAMP", stamp),
                patch(
                    "scripts.local_windows_installer._new_build_freshness",
                    return_value=BUILD_FRESHNESS,
                ),
            ):
                first = _canonical_candidate_identity(
                    SOURCE_FINGERPRINT, "a" * 40
                )
                second = _canonical_candidate_identity(
                    SOURCE_FINGERPRINT, "a" * 40
                )

            self.assertEqual(first, second)
            data = json.loads(stamp.read_text(encoding="utf-8"))
            self.assertEqual(data["schema"], 2)
            self.assertRegex(data["build_nonce"], r"^[0-9a-f]{32}$")

    def test_completed_candidate_keeps_only_its_current_bundle(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            target_root = Path(temporary) / "target"
            input_root = target_root / "installer-inputs"
            current = input_root / BUILD_ID
            stale = input_root / "aaaaaaaaaaaa.bbbbbbbbbbbb"
            current.mkdir(parents=True)
            stale.mkdir()
            temporary_root = target_root / "tmp"
            temporary_root.mkdir()
            stamp = target_root / CANDIDATE_STAMP.name
            stamp.write_text("{}", encoding="utf-8")
            paths = InstallerPaths(
                target_root=target_root,
                input_root=input_root,
                output_path=target_root / "release" / OUTPUT_PATH.name,
                nsis_cache=target_root / "tools" / "nsis",
            )

            _prune_completed_candidate_outputs(paths, BUILD_ID, isolated=False)

            self.assertTrue(current.is_dir())
            self.assertFalse(stale.exists())
            self.assertFalse(stamp.exists())
            self.assertFalse(temporary_root.exists())

    def test_build_only_candidate_reuses_exact_bundle_before_cargo(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            paths = InstallerPaths(
                target_root=root / "target",
                input_root=root / "inputs",
                output_path=root / "release" / OUTPUT_PATH.name,
                nsis_cache=root / "tools" / "nsis",
            )
            bundle = paths.input_root / BUILD_ID
            bundle.mkdir(parents=True)
            options = argparse.Namespace(
                source_worktree=source,
                cargo_target_dir=root / "cargo-target",
                test_filter=None,
                portable_pty_test_filter=None,
                release_test_filter=None,
                product_name="Herdr Quality Test",
                isolated=False,
            )

            with (
                patch(
                    "scripts.local_windows_installer._source_root",
                    return_value=source,
                ),
                patch(
                    "scripts.local_windows_installer._source_branch",
                    return_value=DEVELOPMENT_BRANCH,
                ),
                patch("scripts.local_windows_installer._require_integrated_development_source"),
                patch(
                    "scripts.local_windows_installer.validate_changed_integration_asset_versions",
                    return_value=(),
                ) as version_gate,
                patch(
                    "scripts.local_windows_installer._source_build_provenance",
                    return_value=(SOURCE_FINGERPRINT, "a" * 40),
                ),
                patch(
                    "scripts.local_windows_installer._canonical_candidate_identity",
                    return_value=(BUILD_ID, BUILD_FRESHNESS),
                ),
                patch(
                    "scripts.local_windows_installer._candidate_paths",
                    return_value=paths,
                ),
                patch("scripts.local_windows_installer._directory") as directory,
                patch("scripts.local_windows_installer.build") as package,
                patch(
                    "scripts.local_windows_installer._prune_completed_candidate_outputs"
                ) as prune,
            ):
                candidate(options)

            directory.assert_not_called()
            version_gate.assert_called_once_with(source)
            package.assert_called_once()
            self.assertEqual(package.call_args.args[0].input_bundle, bundle)
            self.assertEqual(
                package.call_args.args[0].product_name, "Herdr Quality Test"
            )
            self.assertEqual(package.call_args.kwargs["paths"], paths)
            self.assertTrue(package.call_args.kwargs["run_interactive_probe"])
            prune.assert_called_once_with(paths, BUILD_ID, isolated=False)

    def test_candidate_validates_integrations_before_bundle_reuse(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            options = argparse.Namespace(
                source_worktree=source,
                cargo_target_dir=root / "cargo-target",
                test_filter=None,
                portable_pty_test_filter=None,
                release_test_filter=None,
                product_name=None,
                isolated=False,
            )
            with (
                patch(
                    "scripts.local_windows_installer._source_root",
                    return_value=source,
                ),
                patch(
                    "scripts.local_windows_installer._source_branch",
                    return_value=DEVELOPMENT_BRANCH,
                ),
                patch("scripts.local_windows_installer._require_integrated_development_source"),
                patch(
                    "scripts.local_windows_installer.validate_changed_integration_asset_versions",
                    side_effect=DeltaWorkflowError("stale marker"),
                ),
                patch("scripts.local_windows_installer.build") as package,
                self.assertRaisesRegex(
                    LocalInstallerError, "migration validation failed"
                ),
            ):
                candidate(options)
            package.assert_not_called()

    def test_candidate_build_uses_all_selected_jobs_and_only_required_bins(self) -> None:
        arguments = _cargo_build_arguments(Path("C:/cargo-target"), 16)

        self.assertEqual(arguments[arguments.index("--jobs") + 1], "16")
        bins = [
            arguments[index + 1]
            for index, value in enumerate(arguments)
            if value == "--bin"
        ]
        self.assertEqual(bins, ["herdr", "herdr-launcher", "herdr-installer-helper"])

    def test_candidate_normal_focused_test_uses_the_iteration_gate(self) -> None:
        self.assertEqual(
            _just_test_arguments("exact_test_filter"),
            ["test-one", "exact_test_filter"],
        )
        with self.assertRaisesRegex(LocalInstallerError, "non-option test filter"):
            _just_test_arguments("--all")

    def test_normal_focused_test_uses_the_shared_target_without_build_identity(
        self,
    ) -> None:
        source = Path("C:/development")
        cargo_target = Path("C:/cargo-target")
        completed = subprocess.CompletedProcess(
            ["just", "test-one", "exact_test_filter"],
            0,
            stdout="Summary [   0.038s] 1 test run: 1 passed, 2693 skipped\n",
            stderr="",
        )
        with (
            patch(
                "scripts.local_windows_installer._require_cargo_commit_headroom"
            ) as headroom,
            patch(
                "scripts.local_windows_installer._run", return_value=completed
            ) as run,
        ):
            _run_normal_focused_test(
                source, cargo_target, 16, "exact_test_filter"
            )

        headroom.assert_called_once_with()
        self.assertEqual(run.call_args.kwargs["cwd"], source)
        self.assertEqual(
            run.call_args.kwargs["environment_overrides"],
            {
                "CARGO_BUILD_JOBS": "16",
                "CARGO_TARGET_DIR": str(cargo_target),
            },
        )
        self.assertEqual(
            run.call_args.kwargs["removed_environment"],
            (
                "HERDR_BUILD_ID",
                "HERDR_BUILD_COMMIT",
                "HERDR_BUILD_FRESHNESS",
                "HERDR_RELEASE_VERSION",
            ),
        )

    def test_test_only_command_reuses_the_candidate_normal_test_owner(self) -> None:
        source = Path("C:/development")
        cargo_target = Path("C:/cargo-target")
        options = argparse.Namespace(
            source_worktree=source,
            cargo_target_dir=cargo_target,
            test_filter="exact_test_filter",
            portable_pty_test_filter=None,
        )
        with (
            patch(
                "scripts.local_windows_installer._source_root", return_value=source
            ) as source_root,
            patch(
                "scripts.local_windows_installer._directory",
                return_value=cargo_target,
            ),
            patch(
                "scripts.local_windows_installer._available_cpu_count",
                return_value=16,
            ),
            patch(
                "scripts.local_windows_installer._run_normal_focused_test"
            ) as focused_test,
        ):
            test_one(options)

        focused_test.assert_called_once_with(
            source, cargo_target, 16, "exact_test_filter"
        )
        source_root.assert_called_once_with(source, test_only=True)

    def test_portable_pty_focused_test_cleans_its_temporary_lock(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "source"
            package = source / "vendor" / "portable-pty"
            package.mkdir(parents=True)
            manifest = package / "Cargo.toml"
            manifest.write_text("[package]\nname = 'portable-pty'\n", encoding="utf-8")
            cargo_target = Path(temporary) / "cargo-target"
            completed = subprocess.CompletedProcess(
                ["cargo", "test"],
                0,
                stdout="test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured\n",
                stderr="",
            )

            with (
                patch(
                    "scripts.local_windows_installer._require_cargo_commit_headroom"
                ) as headroom,
                patch(
                    "scripts.local_windows_installer._run", return_value=completed
                ) as run,
            ):
                _run_portable_pty_focused_test(
                    source, cargo_target, 16, "exact_test_filter"
                )

            headroom.assert_called_once_with()
            self.assertFalse((package / "Cargo.lock").exists())
            arguments = run.call_args.args[1]
            self.assertEqual(arguments[0], "test")
            self.assertEqual(
                arguments[arguments.index("--manifest-path") + 1], str(manifest)
            )
            self.assertNotIn("--locked", arguments)
            self.assertEqual(
                arguments[arguments.index("--target") + 1],
                "x86_64-pc-windows-msvc",
            )
            self.assertEqual(
                arguments[arguments.index("--target-dir") + 1], str(cargo_target)
            )
            self.assertEqual(arguments[arguments.index("--jobs") + 1], "16")
            self.assertEqual(
                arguments[-3:], ["exact_test_filter", "--", "--nocapture"]
            )
            with self.assertRaisesRegex(LocalInstallerError, "non-option test filter"):
                _portable_pty_test_arguments(
                    manifest, cargo_target, 16, "--all"
                )

            with (
                patch(
                    "scripts.local_windows_installer._require_cargo_commit_headroom"
                ),
                patch(
                    "scripts.local_windows_installer._run",
                    side_effect=LocalInstallerError("expected failure"),
                ),
                self.assertRaisesRegex(LocalInstallerError, "expected failure"),
            ):
                _run_portable_pty_focused_test(
                    source, cargo_target, 16, "exact_test_filter"
                )
            self.assertFalse((package / "Cargo.lock").exists())

    def test_pre_push_command_selects_the_portable_pty_owner(self) -> None:
        source = Path("C:/development")
        cargo_target = Path("C:/cargo-target")
        options = argparse.Namespace(
            source_worktree=source,
            cargo_target_dir=cargo_target,
            test_filter=None,
            portable_pty_test_filter="exact_test_filter",
        )
        with (
            patch(
                "scripts.local_windows_installer._source_root", return_value=source
            ),
            patch(
                "scripts.local_windows_installer._directory",
                return_value=cargo_target,
            ),
            patch(
                "scripts.local_windows_installer._available_cpu_count",
                return_value=16,
            ),
            patch(
                "scripts.local_windows_installer._run_portable_pty_focused_test"
            ) as focused_test,
        ):
            test_one(options)

        focused_test.assert_called_once_with(
            source, cargo_target, 16, "exact_test_filter"
        )

    def test_candidate_release_focused_test_shares_windows_target_and_jobs(self) -> None:
        cargo_target = Path("C:/cargo-target")
        arguments = _cargo_test_arguments(cargo_target, 16, "exact_test_filter")

        self.assertEqual(arguments[0], "test")
        self.assertIn("--release", arguments)
        self.assertEqual(arguments[arguments.index("--target") + 1], "x86_64-pc-windows-msvc")
        self.assertEqual(arguments[arguments.index("--target-dir") + 1], str(cargo_target))
        self.assertEqual(arguments[arguments.index("--jobs") + 1], "16")
        self.assertEqual(arguments[arguments.index("--bin") + 1], "herdr")
        self.assertEqual(arguments[-3:], ["exact_test_filter", "--", "--nocapture"])

        with self.assertRaisesRegex(LocalInstallerError, "non-option test filter"):
            _cargo_test_arguments(cargo_target, 16, "--all")

    def test_candidate_focused_test_requires_exactly_one_result(self) -> None:
        _require_one_focused_test(
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured\n"
        )

        with self.assertRaisesRegex(LocalInstallerError, "exactly one passing test"):
            _require_one_focused_test(
                "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured\n"
            )

        _require_one_nextest_test(
            "Summary [   0.038s] 1 test run: 1 passed, 2693 skipped\n"
        )
        with self.assertRaisesRegex(LocalInstallerError, "exactly one passing test"):
            _require_one_nextest_test(
                "Summary [   0.038s] 2 tests run: 2 passed, 2692 skipped\n"
            )

    def test_dynamic_msvc_runtime_imports_are_rejected_case_insensitively(
        self,
    ) -> None:
        dependencies = """
            kernel32.dll
            VCRUNTIME140.dll
            vcruntime140_1.DLL
            MSVCP140.dll
            api-ms-win-crt-runtime-l1-1-0.dll
        """

        self.assertEqual(
            _dynamic_msvc_runtime_imports(dependencies),
            ["MSVCP140.DLL", "VCRUNTIME140.DLL", "VCRUNTIME140_1.DLL"],
        )

    def test_git_commands_trust_only_the_selected_checkout(self) -> None:
        checkout = Path("C:/selected-worktree")

        self.assertEqual(
            _git_arguments(checkout, ["status", "--short"]),
            [
                "-c",
                "core.longpaths=true",
                "-c",
                f"safe.directory={checkout.resolve().as_posix()}",
                "-C",
                str(checkout),
                "status",
                "--short",
            ],
        )


if __name__ == "__main__":
    unittest.main()
