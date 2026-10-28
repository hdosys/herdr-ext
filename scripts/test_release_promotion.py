from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from scripts.delta_workflow import PROJECT_ROOT, DeltaWorkflowError
from scripts.release_promotion import WORKFLOW, select_release_revision, workflow_contract
from scripts.test_delta_workflow import DeltaFixture, run_git


def change_publication_body(workflow: str, name: str, extra: str) -> str:
    step = workflow.index(f"      - name: {name}\n")
    body = workflow.index("        run: |\n", step) + len("        run: |\n")
    return workflow[:body] + extra + workflow[body:]


class ReleasePromotionTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = DeltaFixture(Path(temporary.name)).control
        self.workflow = (PROJECT_ROOT / WORKFLOW).read_text(encoding="utf-8")
        self.candidate = self.commit({
            WORKFLOW: self.workflow,
            "CHANGELOG.md": "## Unreleased\n",
            "README.md": "Current release\n",
            "scripts/preview.py": "# Tested build input\n",
            "Cargo.lock": "# Tested dependencies\n",
        })

    def commit(self, files: dict[str, str]) -> str:
        for name, text in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(text.encode("utf-8"))
        run_git(self.root, ["add", "--", *files])
        run_git(self.root, ["commit", "-m", "fixture snapshot"])
        return run_git(self.root, ["rev-parse", "HEAD"])

    def select(self, master: str, *, promotion: str | None = None, release: str | None = None) -> tuple[str, str]:
        return select_release_revision(
            self.candidate, self.candidate, promotion or self.candidate, master,
            release_ref=release, project_root=self.root,
        )

    def test_bootstrap_and_later_notes_preserve_the_candidate_and_existing_tag(self) -> None:
        updated = change_publication_body(
            self.workflow, "Validate immutable release candidate", "          : new publication policy\n",
        )
        promotion = self.commit({
            WORKFLOW: updated,
            "scripts/release_promotion.py": "# Publication-only policy\n",
            "scripts/test_release_promotion.py": "# Publication-only checks\n",
        })
        release = self.commit({"CHANGELOG.md": "## [2026.10.27.1]\n\nVerified changes.\n"})
        self.assertEqual(self.select(release, promotion=promotion), (release, release))
        master = self.commit({"README.md": "Updated documentation\n", "website/preview.json": "{}\n"})
        self.assertEqual(self.select(master, promotion=promotion, release=release), (master, release))

    def test_build_inputs_and_nonregular_documents_are_rejected(self) -> None:
        for path in (
            "value.txt", "patches/delta/BASE", "patches/delta/series",
            "Cargo.lock", "scripts/preview.py", "scripts/unreviewed.py",
        ):
            with self.subTest(path=path):
                run_git(self.root, ["switch", "--detach", self.candidate])
                changed = self.commit({path: "Changed build input\n"})
                with self.assertRaisesRegex(DeltaWorkflowError, "build input changed"):
                    self.select(changed)
        run_git(self.root, ["switch", "--detach", self.candidate])
        blob = run_git(self.root, ["rev-parse", "HEAD:CHANGELOG.md"])
        run_git(self.root, ["update-index", "--cacheinfo", f"120000,{blob},CHANGELOG.md"])
        run_git(self.root, ["commit", "-m", "nonregular document"])
        with self.assertRaisesRegex(DeltaWorkflowError, "regular file"):
            self.select(run_git(self.root, ["rev-parse", "HEAD"]))

    def test_tag_must_be_compatible_and_on_master(self) -> None:
        bad_tag = self.commit({"value.txt": "untested source\n"})
        # A later restoration does not make the earlier, incompatible tag valid.
        master = self.commit({"value.txt": "first\n", "CHANGELOG.md": "Release notes\n"})
        self.assertEqual(self.select(master), (master, master))
        with self.assertRaisesRegex(DeltaWorkflowError, "build input changed"):
            self.select(master, release=bad_tag)
        with self.assertRaisesRegex(DeltaWorkflowError, "build input changed"):
            select_release_revision(
                self.candidate, bad_tag, self.candidate, master, project_root=self.root,
            )
        run_git(self.root, ["switch", "--detach", self.candidate])
        off_master = self.commit({"CHANGELOG.md": "Unmerged notes\n"})
        with self.assertRaisesRegex(DeltaWorkflowError, "not on the selected history"):
            self.select(master, release=off_master)
        with self.assertRaisesRegex(DeltaWorkflowError, "not on the selected history"):
            self.select(master, promotion=off_master)


class PublicationWorkflowContractTests(unittest.TestCase):
    def test_only_three_literal_shell_bodies_can_change(self) -> None:
        workflow = (PROJECT_ROOT / WORKFLOW).read_text(encoding="utf-8")
        expected = workflow_contract(workflow)
        for name in (
            "Validate immutable release candidate",
            "Verify and publish stable release",
            "Generate preview update manifest",
        ):
            changed = change_publication_body(workflow, name, "          : publication-only change\n")
            self.assertEqual(workflow_contract(changed), expected)
        changes = {
            "toolchain": workflow.replace("  RUST_TOOLCHAIN_VERSION:", "  CHANGED_TOOLCHAIN_VERSION:", 1),
            "condition": workflow.replace("if: inputs.operation == 'promote'", "if: inputs.operation == 'build'", 1),
            "extra root": workflow + "\nenv:\n  RUSTFLAGS: changed\n",
            "extra job": workflow + "\n  extra-build:\n    runs-on: ubuntu-latest\n",
            "scalar escape": change_publication_body(
                workflow, "Validate immutable release candidate",
                "          : allowed shell\n        if: always()\n",
            ),
        }
        for name, changed in changes.items():
            with self.subTest(name=name):
                self.assertNotEqual(workflow_contract(changed), expected)
        unsupported = change_publication_body(
            workflow, "Validate immutable release candidate", "            : wrong scalar indentation\n",
        )
        with self.assertRaisesRegex(DeltaWorkflowError, "ten-space indentation"):
            workflow_contract(unsupported)
        with self.assertRaisesRegex(DeltaWorkflowError, "encoding"):
            workflow_contract(workflow.replace("\n", "\r\n"))


if __name__ == "__main__":
    unittest.main()
