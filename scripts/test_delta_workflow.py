from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Sequence

from scripts.delta_workflow import (
    DEVELOPMENT_BRANCH,
    DEVELOPMENT_REMOTE_REF,
    DeltaWorkflowError,
    _git_command,
    compile_delta_prefixes,
    finalize_delta_mailbox,
    integrate_development_worktree,
    issue_reference_report,
    materialize_delta_worktree,
    publish_development_worktree,
    refresh_delta,
    start_delta_worktree,
    validate_integration_asset_version_changes,
    verify_replay_tree,
)


GIT_TIMEOUT_SECONDS = 30


def run_git(
    cwd: Path,
    arguments: Sequence[str],
    *,
    strip: bool = True,
) -> str:
    result = subprocess.run(
        ["git", *arguments],
        cwd=cwd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
        timeout=GIT_TIMEOUT_SECONDS,
    )
    if result.returncode != 0:
        raise AssertionError(result.stderr or result.stdout)
    return result.stdout.strip() if strip else result.stdout


class DeltaFixture:
    def __init__(self, root: Path) -> None:
        self.control = root / "control"
        self.control.mkdir()
        run_git(self.control, ["init", "-b", "master"])
        run_git(self.control, ["config", "user.name", "Delta Test"])
        run_git(self.control, ["config", "user.email", "delta@example.com"])
        run_git(self.control, ["config", "core.autocrlf", "false"])

        (self.control / "value.txt").write_bytes(b"base\n")
        (self.control / ".gitignore").write_bytes(b".zig-cache/\nzig-out/\n")
        run_git(self.control, ["add", "value.txt", ".gitignore"])
        run_git(self.control, ["commit", "-m", "base"])
        self.base = run_git(self.control, ["rev-parse", "HEAD"])

        (self.control / "value.txt").write_bytes(b"first\n")
        run_git(self.control, ["add", "value.txt"])
        run_git(
            self.control,
            ["commit", "-m", "feat: first", "-m", "Own the first value."],
        )
        first = run_git(self.control, ["rev-parse", "HEAD"])
        first_patch = run_git(
            self.control,
            [
                "format-patch",
                "--stdout",
                "--full-index",
                "--binary",
                "--subject-prefix=PATCH 1/2",
                "-1",
                first,
            ],
            strip=False,
        )

        (self.control / "second.txt").write_bytes(b"second\n")
        run_git(self.control, ["add", "second.txt"])
        run_git(self.control, ["commit", "-m", "feat: second"])
        second = run_git(self.control, ["rev-parse", "HEAD"])
        second_patch = run_git(
            self.control,
            [
                "format-patch",
                "--stdout",
                "--full-index",
                "--binary",
                "--subject-prefix=PATCH 2/2",
                "-1",
                second,
            ],
            strip=False,
        )
        self.source_tree = run_git(self.control, ["rev-parse", "HEAD^{tree}"])

        delta_root = self.control / "patches" / "delta"
        delta_root.mkdir(parents=True)
        (delta_root / "BASE").write_bytes(f"{self.base}\n".encode())
        (delta_root / "series").write_bytes(
            b"0001-first.patch\n0002-second.patch\n"
        )
        (delta_root / "0001-first.patch").write_text(
            first_patch, encoding="utf-8", newline="\n"
        )
        (delta_root / "0002-second.patch").write_text(
            second_patch, encoding="utf-8", newline="\n"
        )
        run_git(self.control, ["add", "patches/delta"])
        run_git(self.control, ["commit", "-m", "chore: add delta"])


class DeltaWorkflowTests(unittest.TestCase):
    def test_stable_refresh_requires_exact_tree_and_preserves_retained_owner(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = DeltaFixture(Path(directory))
            head = run_git(fixture.control, ["rev-parse", "HEAD^"])
            base = run_git(fixture.control, ["rev-parse", "HEAD^^"])
            delta = fixture.control / "patches" / "delta"
            originals = {path.name: path.read_bytes() for path in delta.iterdir()}
            base_tree = run_git(fixture.control, ["rev-parse", f"{base}^{{tree}}"])
            with self.assertRaisesRegex(DeltaWorkflowError, "logical source tree differs"):
                refresh_delta(base, head, base_tree, ["0001-first.patch"], fixture.control)
            self.assertEqual({path.name: path.read_bytes() for path in delta.iterdir()}, originals)

            result = refresh_delta(base, head, fixture.source_tree, ["0001-first.patch"], fixture.control)
            self.assertEqual(result.base, base)
            self.assertEqual(result.tree, fixture.source_tree)
            self.assertEqual(result.mailboxes, ("0002-second.patch",))
            self.assertFalse((delta / "0001-first.patch").exists())
            self.assertIn(b"Subject: [PATCH 1/1] feat: second", (delta / "0002-second.patch").read_bytes())

    def test_linked_development_integration_requires_exact_clean_base(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = DeltaFixture(root)
            worktree = root / "development"
            started = start_delta_worktree("integration", worktree, fixture.control)
            run_git(worktree, ["branch", "-m", DEVELOPMENT_BRANCH])
            head = run_git(
                fixture.control,
                ["commit-tree", fixture.source_tree, "-p", started.head, "-m", "milestone"],
            )
            with self.assertRaisesRegex(DeltaWorkflowError, "stale integration base"):
                integrate_development_worktree(worktree, fixture.base, head, fixture.control)
            (worktree / "value.txt").write_bytes(b"unfinished\n")
            with self.assertRaisesRegex(DeltaWorkflowError, "source worktree must be clean"):
                integrate_development_worktree(worktree, started.head, head, fixture.control)
            (worktree / "value.txt").write_bytes(b"first\n")
            result = integrate_development_worktree(worktree, started.head, head, fixture.control)
            self.assertEqual(result.head, head)
            self.assertEqual(result.tree, fixture.source_tree)

    @staticmethod
    def integration_module(integration: str, version: int, *files: str) -> str:
        constant = integration.upper().replace("-", "_")
        includes = "\n".join(
            f'const ASSET_{index}: &str = include_str!("assets/{integration}/{name}");'
            for index, name in enumerate(files)
        )
        return (
            f"const {constant}_INTEGRATION_VERSION: u32 = {version};\n{includes}\n"
        )

    @staticmethod
    def integration_assets(
        integration: str, version: int, *files: str
    ) -> dict[str, str | None]:
        return {
            f"src/integration/assets/{integration}/{name}": (
                f"# HERDR_INTEGRATION_VERSION={version}\n{name}\n"
            )
            for name in files
        }

    def test_changed_integration_asset_requires_one_version_advance(self) -> None:
        module = self.integration_module("sample", 1, "hook.ps1")
        baseline = self.integration_assets("sample", 1, "hook.ps1")
        current = dict(baseline)
        path = "src/integration/assets/sample/hook.ps1"
        source = current[path]
        self.assertIsNotNone(source)
        current[path] = f"{source}changed\n"

        with self.assertRaisesRegex(DeltaWorkflowError, "advance beyond baseline"):
            validate_integration_asset_version_changes(
                module, current, module, baseline
            )

    def test_changed_integration_asset_requires_matching_rust_constant(self) -> None:
        baseline_module = self.integration_module("sample", 1, "hook.ps1")
        current_module = self.integration_module("sample", 1, "hook.ps1")
        baseline = self.integration_assets("sample", 1, "hook.ps1")
        current = self.integration_assets("sample", 2, "hook.ps1")

        with self.assertRaisesRegex(DeltaWorkflowError, "does not match Rust constant"):
            validate_integration_asset_version_changes(
                current_module, current, baseline_module, baseline
            )

    def test_cumulative_changed_assets_share_one_higher_version(self) -> None:
        baseline_module = self.integration_module("sample", 1, "one.js", "two.js")
        current_module = self.integration_module("sample", 3, "one.js", "two.js")
        baseline = self.integration_assets("sample", 1, "one.js", "two.js")
        current = self.integration_assets("sample", 3, "one.js", "two.js")

        self.assertEqual(
            validate_integration_asset_version_changes(
                current_module, current, baseline_module, baseline
            ),
            ("sample",),
        )

    def test_non_embedded_integration_test_change_needs_no_version_advance(self) -> None:
        module = self.integration_module("sample", 1, "hook.ps1")
        baseline = self.integration_assets("sample", 1, "hook.ps1")
        current = dict(baseline)
        current["src/integration/assets/sample/hook.test.ps1"] = "changed\n"

        self.assertEqual(
            validate_integration_asset_version_changes(
                module, current, module, baseline
            ),
            (),
        )

    def test_new_integration_starts_at_version_one(self) -> None:
        current_module = self.integration_module("sample", 1, "hook.ps1")
        current = self.integration_assets("sample", 1, "hook.ps1")

        self.assertEqual(
            validate_integration_asset_version_changes(
                current_module, current, "", {}
            ),
            ("sample",),
        )

    def test_git_commands_trust_only_control_and_selected_worktree(self) -> None:
        control = Path("C:/repo/control")
        worktree = Path("C:/repo/development")

        self.assertEqual(
            _git_command(control, ["status", "--short"], cwd=worktree),
            [
                "git",
                "-c",
                "core.longpaths=true",
                "-c",
                f"safe.directory={control.resolve().as_posix()}",
                "-c",
                f"safe.directory={worktree.resolve().as_posix()}",
                "status",
                "--short",
            ],
        )

    def test_temporary_index_replay_matches_the_source_tree(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))

            result = verify_replay_tree(fixture.source_tree, fixture.control)

            self.assertEqual(result.base, fixture.base)
            self.assertEqual(
                result.mailboxes, ("0001-first.patch", "0002-second.patch")
            )
            self.assertEqual(result.tree, fixture.source_tree)
            self.assertEqual(run_git(fixture.control, ["status", "--porcelain"]), "")

    def test_refresh_compile_checks_each_ordered_prefix(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))
            log = Path(temp_dir) / "prefixes.txt"
            workspace = Path(temp_dir) / "prefix-work"
            probe = (
                "import os, pathlib; "
                "root = pathlib.Path.cwd(); "
                "prefix = int(os.environ['HERDR_DELTA_PREFIX']); "
                "assert int(os.environ['CARGO_BUILD_JOBS']) >= 1; "
                "assert os.environ['CARGO_INCREMENTAL'] == '0'; "
                "assert (root / 'value.txt').read_text().strip() == 'first'; "
                "assert (root / 'second.txt').exists() == (prefix == 2); "
                "assert len(os.environ['HERDR_DELTA_TREE']) == 40; "
                "cache = root / 'vendor/libghostty-vt/.zig-cache/local'; "
                "cache.mkdir(parents=True, exist_ok=True); "
                "marker = cache / 'count'; "
                "marker.write_text(str(int(marker.read_text()) + 1) if marker.exists() else '1'); "
                "target = pathlib.Path(os.environ['CARGO_TARGET_DIR']); "
                "target.mkdir(exist_ok=True); (target / 'retained').touch(); "
                "pathlib.Path(os.environ['HERDR_PREFIX_LOG']).open('a', encoding='utf-8').write("
                "f\"{prefix}:{os.environ['HERDR_DELTA_MAILBOX']}\\n\")"
            )

            previous = os.environ.get("HERDR_PREFIX_LOG")
            os.environ["HERDR_PREFIX_LOG"] = str(log)
            try:
                mailboxes = compile_delta_prefixes(
                    fixture.control,
                    check_command=(sys.executable, "-c", probe),
                    work_dir=workspace,
                )
                with self.assertRaisesRegex(DeltaWorkflowError, "failed to compile"):
                    compile_delta_prefixes(fixture.control, work_dir=workspace,
                                           check_command=(sys.executable, "-c", "raise SystemExit(1)"))
                compile_delta_prefixes(fixture.control, work_dir=workspace,
                                       check_command=(sys.executable, "-c", probe))
            finally:
                if previous is None:
                    os.environ.pop("HERDR_PREFIX_LOG", None)
                else:
                    os.environ["HERDR_PREFIX_LOG"] = previous

            self.assertEqual(mailboxes, ("0001-first.patch", "0002-second.patch"))
            self.assertEqual(
                log.read_text(encoding="utf-8").splitlines(),
                ["1:0001-first.patch", "2:0002-second.patch"] * 2,
            )
            self.assertEqual((workspace / "source/vendor/libghostty-vt/.zig-cache/local/count").read_text(), "4")
            self.assertTrue((workspace / "target/retained").is_file())
            (workspace / "source/value.txt").write_text("unfinished")
            with self.assertRaisesRegex(DeltaWorkflowError, "has changes"):
                compile_delta_prefixes(fixture.control, work_dir=workspace)
            self.assertEqual((workspace / "source/value.txt").read_text(), "unfinished")

    def test_prefix_workspace_rejects_foreign_or_unsafe_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = DeltaFixture(root)
            foreign = root / "foreign"
            foreign.mkdir()
            (foreign / "keep").write_text("user data")
            for path in (foreign, fixture.control, Path("relative")):
                with self.subTest(path=path), self.assertRaises(DeltaWorkflowError):
                    compile_delta_prefixes(fixture.control, work_dir=path)
            self.assertEqual((foreign / "keep").read_text(), "user data")

    def test_issue_report_joins_only_explicit_captured_fields(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = DeltaFixture(Path(directory))
            patch = fixture.control / "patches/delta/0001-first.patch"
            patch.write_text(patch.read_text().replace("+first\n", "+first example/project#14\n").replace(
                "Own the first value.", "Refs example/project#12 twice example/project#12; bare #9."
            ))
            ledger = Path(directory) / "ledger.md"
            ledger.write_text("### example/project#12\n- Title: Captured title\n"
                              "- Local outcome: Captured behavior\n- Verification: PRIVATE\n"
                              "#### Draft post\n- Title: PRIVATE DRAFT\nsecret@example.com\n")
            public = issue_reference_report(fixture.control)
            self.assertEqual(len(public), 3)
            self.assertEqual(public[0]["reference"], "example/project#12")
            self.assertEqual(public[0]["path_owners"], ["value.txt"])
            self.assertEqual(public[1]["reference"], "example/project#14")
            self.assertIsNone(public[2]["reference"])
            self.assertIsNone(public[0]["title"])
            joined = issue_reference_report(fixture.control, ledger=ledger)
            self.assertEqual(joined[0]["title"], "Captured title")
            self.assertEqual(joined[0]["behavior"], "Captured behavior")
            self.assertEqual(len(joined[0]["source_commit"]), 40)
            self.assertEqual(len(joined[0]["mailbox_sha256"]), 64)
            output = json.dumps(joined)
            self.assertNotIn("PRIVATE", output)
            self.assertNotIn("@", output)
            self.assertEqual(joined, issue_reference_report(fixture.control, ledger=ledger))

    def test_expected_tree_mismatch_reports_the_semantic_difference(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))
            base_tree = run_git(
                fixture.control, ["rev-parse", f"{fixture.base}^{{tree}}"]
            )

            with self.assertRaisesRegex(
                DeltaWorkflowError, "does not match the tested source tree"
            ):
                verify_replay_tree(base_tree, fixture.control)

    def test_start_creates_one_replayed_task_worktree(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))
            worktrees = Path(temp_dir) / "worktrees"
            worktrees.mkdir()
            worktree = worktrees / "fast-path"

            result = start_delta_worktree("fast-path", worktree, fixture.control)

            self.assertEqual(result.branch, "agent/delta-fast-path")
            self.assertEqual(result.mailbox_count, 2)
            self.assertEqual(result.tree, fixture.source_tree)
            self.assertEqual(
                run_git(worktree, ["rev-list", "--count", f"{fixture.base}..HEAD"]),
                "2",
            )

    def test_start_refuses_to_recreate_published_development_from_base(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))
            origin = Path(temp_dir) / "origin.git"
            origin.mkdir()
            run_git(origin, ["init", "--bare"])
            run_git(fixture.control, ["remote", "add", "origin", str(origin)])
            run_git(
                fixture.control,
                ["push", "origin", f"{fixture.base}:{DEVELOPMENT_REMOTE_REF}"],
            )
            worktrees = Path(temp_dir) / "worktrees"
            worktrees.mkdir()

            with self.assertRaisesRegex(
                DeltaWorkflowError, "remote cumulative development branch already exists"
            ):
                start_delta_worktree(
                    "development", worktrees / "development", fixture.control
                )

    def test_materialize_replays_an_existing_task_worktree(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))
            worktrees = Path(temp_dir) / "worktrees"
            worktrees.mkdir()
            worktree = worktrees / "managed"
            run_git(
                fixture.control,
                [
                    "worktree",
                    "add",
                    "-b",
                    "agent/delta-managed",
                    str(worktree),
                    fixture.base,
                ],
            )

            result = materialize_delta_worktree(worktree, fixture.control)

            self.assertEqual(result.branch, "agent/delta-managed")
            self.assertEqual(result.tree, fixture.source_tree)
            self.assertEqual(
                run_git(worktree, ["rev-list", "--count", f"{fixture.base}..HEAD"]),
                "2",
            )

    def test_publish_development_pushes_only_the_exact_shared_branch(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))
            origin = Path(temp_dir) / "origin.git"
            origin.mkdir()
            run_git(origin, ["init", "--bare"])
            run_git(fixture.control, ["remote", "add", "origin", str(origin)])
            worktrees = Path(temp_dir) / "worktrees"
            worktrees.mkdir()
            worktree = worktrees / "development"
            started = start_delta_worktree("development", worktree, fixture.control)
            self.assertEqual(started.branch, DEVELOPMENT_BRANCH)

            result = publish_development_worktree(worktree, fixture.control)

            self.assertEqual(result.tree, fixture.source_tree)
            self.assertEqual(
                run_git(
                    fixture.control,
                    [
                        "ls-remote",
                        "--heads",
                        "origin",
                        DEVELOPMENT_REMOTE_REF,
                    ],
                ).split()[0],
                result.head,
            )

            topic = worktrees / "topic"
            run_git(
                fixture.control,
                [
                    "worktree",
                    "add",
                    "-b",
                    "agent/delta-topic",
                    str(topic),
                    result.head,
                ],
            )
            run_git(topic, ["config", "user.name", "Delta Test"])
            run_git(topic, ["config", "user.email", "delta@example.invalid"])
            (topic / "topic.txt").write_bytes(b"finished topic\n")
            run_git(topic, ["add", "topic.txt"])
            run_git(topic, ["commit", "-m", "topic change"])

            with self.assertRaisesRegex(
                DeltaWorkflowError, "unintegrated topic worktrees"
            ):
                publish_development_worktree(worktree, fixture.control)

            run_git(worktree, ["merge", "--ff-only", "agent/delta-topic"])
            publish_development_worktree(worktree, fixture.control)

    def test_finalize_updates_only_owner_and_reproduces_tested_tree(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))
            worktrees = Path(temp_dir) / "worktrees"
            worktrees.mkdir()
            worktree = worktrees / "finalize"
            start_delta_worktree("finalize", worktree, fixture.control)

            (worktree / "value.txt").write_bytes(b"final\n")
            run_git(worktree, ["add", "value.txt"])
            run_git(worktree, ["commit", "-m", "fix: finalize value"])
            source_head = run_git(worktree, ["rev-parse", "HEAD"])
            source_tree = run_git(worktree, ["rev-parse", "HEAD^{tree}"])
            delta_root = fixture.control / "patches" / "delta"
            later_mailbox = delta_root / "0002-second.patch"
            later_bytes = later_mailbox.read_bytes()

            result = finalize_delta_mailbox(
                worktree,
                "0001-first.patch",
                source_tree,
                fixture.control,
            )

            self.assertEqual(result.source_head, source_head)
            self.assertEqual(result.source_tree, source_tree)
            self.assertEqual(result.replay_tree, source_tree)
            self.assertEqual(run_git(worktree, ["rev-parse", "HEAD"]), source_head)
            self.assertEqual(later_mailbox.read_bytes(), later_bytes)
            self.assertIn(
                "Subject: [PATCH 1/2] feat: first",
                (delta_root / "0001-first.patch").read_text(encoding="utf-8"),
            )
            self.assertIn(
                "Own the first value.",
                (delta_root / "0001-first.patch").read_text(encoding="utf-8"),
            )
            self.assertEqual(
                verify_replay_tree(source_tree, fixture.control).tree,
                source_tree,
            )

    def test_finalize_accepts_cumulative_merge_history_at_replay_tree(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))
            worktrees = Path(temp_dir) / "worktrees"
            worktrees.mkdir()
            worktree = worktrees / "finalize-merged"
            start_delta_worktree("finalize-merged", worktree, fixture.control)

            queue_head = run_git(worktree, ["rev-parse", "HEAD"])
            merge_head = run_git(
                worktree,
                [
                    "commit-tree",
                    fixture.source_tree,
                    "-p",
                    queue_head,
                    "-p",
                    fixture.base,
                    "-m",
                    "merge cumulative development history",
                ],
            )
            run_git(worktree, ["update-ref", "HEAD", merge_head])
            (worktree / "value.txt").write_bytes(b"final\n")
            run_git(worktree, ["add", "value.txt"])
            run_git(worktree, ["commit", "-m", "fix: finalize merged value"])
            source_tree = run_git(worktree, ["rev-parse", "HEAD^{tree}"])

            result = finalize_delta_mailbox(
                worktree,
                "0001-first.patch",
                source_tree,
                fixture.control,
            )

            self.assertEqual(result.replay_tree, source_tree)

    def test_finalize_appends_one_new_mailbox_from_linear_wip(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))
            worktrees = Path(temp_dir) / "worktrees"
            worktrees.mkdir()
            worktree = worktrees / "new-mailbox"
            start_delta_worktree("new-mailbox", worktree, fixture.control)

            (worktree / "third.txt").write_bytes(b"third\n")
            run_git(worktree, ["add", "third.txt"])
            run_git(
                worktree,
                ["commit", "-m", "feat: third", "-m", "Refs example/repo#123"],
            )
            (worktree / "fourth.txt").write_bytes(b"fourth\n")
            run_git(worktree, ["add", "fourth.txt"])
            run_git(worktree, ["commit", "-m", "docs: explain third"])
            source_head = run_git(worktree, ["rev-parse", "HEAD"])
            source_tree = run_git(worktree, ["rev-parse", "HEAD^{tree}"])

            result = finalize_delta_mailbox(
                worktree,
                "0003-third.patch",
                source_tree,
                fixture.control,
                new_mailbox=True,
            )

            delta_root = fixture.control / "patches" / "delta"
            self.assertEqual(result.source_head, source_head)
            self.assertEqual(result.replay_tree, source_tree)
            self.assertEqual(
                (delta_root / "series").read_text(encoding="utf-8").splitlines(),
                ["0001-first.patch", "0002-second.patch", "0003-third.patch"],
            )
            self.assertIn(
                "Subject: [PATCH 1/3] feat: first",
                (delta_root / "0001-first.patch").read_text(encoding="utf-8"),
            )
            self.assertIn(
                "Subject: [PATCH 2/3] feat: second",
                (delta_root / "0002-second.patch").read_text(encoding="utf-8"),
            )
            new_mailbox = (delta_root / "0003-third.patch").read_text(
                encoding="utf-8"
            )
            self.assertIn("Subject: [PATCH 3/3] feat: third", new_mailbox)
            self.assertIn("Refs example/repo#123", new_mailbox)
            self.assertEqual(
                verify_replay_tree(source_tree, fixture.control).tree,
                source_tree,
            )

    def test_finalize_rejects_a_tree_not_at_source_head(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = DeltaFixture(Path(temp_dir))
            worktrees = Path(temp_dir) / "worktrees"
            worktrees.mkdir()
            worktree = worktrees / "mismatch"
            start_delta_worktree("mismatch", worktree, fixture.control)

            (worktree / "value.txt").write_bytes(b"final\n")
            run_git(worktree, ["add", "value.txt"])
            run_git(worktree, ["commit", "-m", "fix: finalize value"])
            mailbox = fixture.control / "patches" / "delta" / "0001-first.patch"
            original = mailbox.read_bytes()

            with self.assertRaisesRegex(
                DeltaWorkflowError, "changed after its tested tree was recorded"
            ):
                finalize_delta_mailbox(
                    worktree,
                    "0001-first.patch",
                    fixture.source_tree,
                    fixture.control,
                )

            self.assertEqual(mailbox.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
