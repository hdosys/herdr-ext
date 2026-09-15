from __future__ import annotations

import hashlib
import re
import struct
import unittest
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parent.parent
CARGO = PROJECT_ROOT / "Cargo.toml"
NSI = PROJECT_ROOT / "packaging/windows/installer/project.nsi"
REMOVED_BRIDGE = PROJECT_ROOT / "packaging/windows/installer-helper-bridge.ps1"
REMOVED_UNINSTALL_RUNNER = PROJECT_ROOT / "packaging/windows/uninstall-runner.ps1"
LEGACY_HELPER = PROJECT_ROOT / "packaging/windows/herdr-installer-helper.ps1"
LEGACY_TEST = PROJECT_ROOT / "scripts/windows_installer_test.ps1"
PACKAGER = PROJECT_ROOT / "scripts/package_windows_installer.ps1"
FAULT_TEST = PROJECT_ROOT / "scripts/windows_installer_fault_test.ps1"
HELPER_ENTRY = PROJECT_ROOT / "src/bin/herdr-installer-helper.rs"
HELPER_CLI = PROJECT_ROOT / "src/platform/windows/installer_helper.rs"
HELPER_FILES = PROJECT_ROOT / "src/platform/windows/installer_helper_files.rs"
HELPER_LIFECYCLE = PROJECT_ROOT / "src/platform/windows/installer_helper_lifecycle.rs"
HELPER_REGISTRY = PROJECT_ROOT / "src/platform/windows/installer_helper_registry.rs"
HELPER_SKILLS = PROJECT_ROOT / "src/platform/windows/installer_helper_skills.rs"
MANAGED_INSTALL = PROJECT_ROOT / "src/managed_install.rs"
WINDOWS_LAUNCHER = PROJECT_ROOT / "src/platform/windows/launcher.rs"
WINDOWS_PLATFORM = PROJECT_ROOT / "src/platform/windows.rs"
UPDATE = PROJECT_ROOT / "src/update.rs"
SKILL = PROJECT_ROOT / "skills/herdr/SKILL.md"
MANAGED_SKILL_HASHES = PROJECT_ROOT / "packaging/windows/managed-skill-hashes.txt"
ARTWORK = NSI.parent / "artwork"
ARTWORK_SOURCE = ARTWORK / "installer-welcome-finish-source.png"
ARTWORK_DERIVATIVES = {
    "installer-welcome-finish-164x314.bmp": (
        164,
        314,
        "e8a07fbbce2eabc1bd705de7f54743f14027a7d58674044691cf13275e99247c",
    ),
    "installer-welcome-finish-205x393.bmp": (
        205,
        393,
        "bc71f8adeb53809492393165533ab6fc130d4d6e570edb21e95878878b422d4c",
    ),
    "installer-welcome-finish-246x471.bmp": (
        246,
        471,
        "7e9dc0595270ca68736382bb4c852dce038ffbb3ea7bb481e77bd91d3c077edc",
    ),
    "installer-welcome-finish-287x550.bmp": (
        287,
        550,
        "8571db6e74e9bc5efef33a8708acaaa35887464a4bdeaf0453525ea5de2b371e",
    ),
    "installer-welcome-finish-328x628.bmp": (
        328,
        628,
        "e055e2515966dfc4e192daef2df76075ac23d3e68d62154b529a476081ea1c6c",
    ),
}


def text(path: Path) -> str:
    return path.read_text(encoding="utf-8")


class WindowsInstallerStaticTests(unittest.TestCase):
    def test_native_helper_is_the_only_installer_engine(self) -> None:
        for path in (
            NSI,
            PACKAGER,
            FAULT_TEST,
            HELPER_ENTRY,
            HELPER_CLI,
            HELPER_FILES,
            HELPER_LIFECYCLE,
            HELPER_REGISTRY,
            HELPER_SKILLS,
            SKILL,
            MANAGED_SKILL_HASHES,
        ):
            self.assertTrue(path.is_file(), path)
        self.assertFalse(LEGACY_HELPER.exists())
        self.assertFalse(LEGACY_TEST.exists())
        self.assertFalse(REMOVED_BRIDGE.exists())
        self.assertFalse(REMOVED_UNINSTALL_RUNNER.exists())

        cargo = text(CARGO)
        self.assertIn('name = "herdr-installer-helper"', cargo)
        self.assertIn('path = "src/bin/herdr-installer-helper.rs"', cargo)
        self.assertIn('"Win32_System_Registry"', cargo)
        self.assertIn('"Win32_Globalization"', cargo)
        self.assertIn('"Win32_System_Environment"', cargo)
        self.assertIn("scripts.test_windows_installer", text(PROJECT_ROOT / "justfile"))

    def test_native_helper_owns_exact_layout_and_lifecycle_contracts(self) -> None:
        entry = text(HELPER_ENTRY)
        cli = text(HELPER_CLI)
        files = text(HELPER_FILES)
        lifecycle = text(HELPER_LIFECYCLE)
        managed = text(MANAGED_INSTALL)
        launcher = text(WINDOWS_LAUNCHER)

        self.assertIn('#[path = "../platform/windows/installer_helper.rs"]', entry)
        self.assertIn("installer_helper::run()", entry)
        self.assertIn('"install" =>', cli)
        self.assertIn('"uninstall" =>', cli)
        self.assertIn('"quiet-uninstall" =>', cli)
        self.assertIn('"skill-removal-default" =>', cli)
        self.assertIn('"complete-maintenance" =>', cli)

        for record in (
            "herdr-install-manifest-v1",
            "herdr-runtime-manifest-v1",
            "herdr-managed-bin-v1\\n",
            "herdr-package-manager-v1\\nmanager=winget\\n",
            "herdr-uninstall-v1\\n",
            "herdr-quiet-uninstall-v1\\n",
        ):
            self.assertIn(record, files)
        self.assertIn('NATIVE_HELPER_NAME: &str = "installer-helper.exe"', files)
        self.assertNotIn("LEGACY_HELPER_NAME", files)
        self.assertIn('LAUNCHER_QUERY_ARG: &str = "--herdr-private-launcher-build-id-v1"', files)
        self.assertIn("ReplaceFileW", files)
        self.assertIn("MoveFileExW", files)
        self.assertIn("FILE_FLAG_WRITE_THROUGH", files)
        self.assertIn("[0u8; 64 * 1024]", files)

        self.assertIn('join("installer-helper.exe")', managed)
        self.assertNotIn('join("installer-helper.ps1")', managed)
        self.assertIn('Command::new(&helper)', launcher)
        self.assertIn('.arg("complete-maintenance")', launcher)
        self.assertNotIn("WindowsPowerShell", launcher)

        for contract in (
            "acquire_lifecycle_lock",
            "CreateMutexW",
            "WaitForSingleObject(handle, 0)",
            "WAIT_ABANDONED",
            "WAIT_TIMEOUT",
            "ReleaseMutex",
            "acquire_coordination",
            "lease_status",
            "process_paths",
            "uninstall.pending",
            "QuietSession",
            "quiet_uninstall",
            "AssignProcessToJobObject",
            "TerminateJobObject",
            "set_pending_launcher",
            "complete_launcher_update_locked",
            "remove_inactive_runtimes",
            "validate_uninstall_cleanup_root",
            "remove_uninstall_residual",
        ):
            self.assertIn(contract, lifecycle)
        self.assertNotIn("ManagedLegacy", lifecycle)
        self.assertNotIn("installer-helper.ps1", lifecycle)
        self.assertNotIn("installer-lifecycle.lock", lifecycle)
        self.assertIn(
            'const LIFECYCLE_MUTEX_NAME: &str = r"Local\\HerdrWinInstallerLifecycle";',
            lifecycle,
        )
        self.assertNotIn("HerdrWinInstallerLifecycle-v", lifecycle)
        self.assertLess(
            lifecycle.index("acquire_lifecycle_lock", lifecycle.index("pub(crate) fn install")),
            lifecycle.index("registry::assert_arp_ownership", lifecycle.index("pub(crate) fn install")),
        )
        self.assertNotIn("eprintln!", entry + lifecycle)
        for forbidden in (
            "invoke-webrequest",
            "start-bitstransfer",
            "http://",
            "https://",
            "taskkill",
            "shutdown.exe",
            "itemtype junction",
        ):
            self.assertNotIn(forbidden, (files + lifecycle).lower())

    def test_registry_and_skill_ownership_remain_exact(self) -> None:
        registry = text(HELPER_REGISTRY)
        skills = text(HELPER_SKILLS)

        for required in (
            "RegQueryValueExW",
            "RegSetValueExW",
            "RegDeleteTreeW",
            "REG_EXPAND_SZ",
            '"PathAdded"',
            '"PathValueCreated"',
            '"QuietUninstallString"',
            "ExpandEnvironmentStringsW",
        ):
            self.assertIn(required, registry)
        self.assertNotIn("SetEnvironmentVariable", registry)
        self.assertNotIn("refusing to modify an ARP registration", registry)
        self.assertIn("raw_owned_path_entry_equal", registry)
        self.assertIn("%LOCALAPPDATA%\\Programs\\Herdr\\bin", registry)
        self.assertIn(
            "Windows Settings > Apps > Installed apps", text(HELPER_FILES)
        )

        for required in (
            'join(".agents").join("skills")',
            'profile.join(".claude")',
            'env::var("CLAUDE_CONFIG_DIR")',
            "read_managed_skill_hashes",
            "install_skill_copies",
            "remove_skill_copies_best_effort",
            "enum SkillDisposition",
            "    Auto,",
            "    Remove,",
        ):
            self.assertIn(required, skills)
        self.assertNotIn("remove_dir_all", skills)

    def test_shipped_installer_contains_no_powershell_payload(self) -> None:
        combined = "\n".join(
            text(path)
            for path in (NSI, PACKAGER, HELPER_CLI, HELPER_LIFECYCLE, HELPER_REGISTRY)
        )
        self.assertNotIn("installer-helper-bridge.ps1", combined)
        self.assertNotIn("uninstall-runner.ps1", combined)
        self.assertNotIn("WindowsPowerShell", combined)
        self.assertNotIn("ExecutionPolicy", combined)
        self.assertNotIn("ARG_HELPER_BRIDGE_PS1", combined)
        self.assertNotIn("ARG_UNINSTALL_RUNNER_PS1", combined)

    def test_managed_skill_hash_manifest_is_exact(self) -> None:
        lines = text(MANAGED_SKILL_HASHES).splitlines()
        self.assertGreaterEqual(len(lines), 2)
        self.assertEqual(lines[0], "herdr-managed-skill-hashes-v1")
        hashes = lines[1:]
        self.assertEqual(hashes, sorted(set(hashes)))
        self.assertTrue(all(re.fullmatch(r"[0-9a-f]{64}", value) for value in hashes))
        canonical = SKILL.read_bytes().replace(b"\r\n", b"\n")
        self.assertNotIn(b"\r", canonical)
        self.assertIn(hashlib.sha256(canonical).hexdigest(), hashes)

    def test_nsis_uses_the_native_helper_and_preserves_presentation(self) -> None:
        nsi = text(NSI)
        for required in (
            "ARG_STAGE_DIR",
            "ARG_LAUNCHER_EXE",
            "ARG_HELPER_EXE",
            "ARG_SKILL_MD",
            "ARG_SKILL_HASH_MANIFEST",
            "ARG_ARTWORK_DIR",
            "APP_BUILD_ID",
            "APP_OUTPUT_PATH",
            "APP_START_GATE_ENV",
            "APP_TEST_MARKER_PREFIX",
            "INFO_PRODUCTNAME",
            "INFO_DISTRIBUTIONNAME",
            "INFO_PRODUCTURL",
            "INFO_UPSTREAMURL",
            "INFO_PRODUCTVERSION_DISPLAY",
            "INFO_PRODUCTVERSION_FIXED",
            "INFO_PRODUCTVERSION_UI",
            "INFO_UPSTREAMVERSION",
        ):
            self.assertIn(f"!ifndef {required}", nsi)
        self.assertNotRegex(nsi, re.compile(r"herdr", re.IGNORECASE))
        self.assertNotIn("ARG_HELPER_PS1", nsi)
        self.assertNotIn("PowerShellPath", nsi)
        self.assertNotIn("SetPowerShellPath", nsi)
        self.assertEqual(
            nsi.count('File /oname=installer-helper.exe "${ARG_HELPER_EXE}"'), 2
        )
        self.assertNotIn(".ps1", nsi)
        self.assertIn(
            '"$PLUGINSDIR\\installer-helper.exe" install --install-root', nsi
        )
        self.assertIn(
            '"$PLUGINSDIR\\installer-helper.exe" uninstall --install-root', nsi
        )
        self.assertIn(
            '"$INSTDIR\\state\\installer-helper.exe" skill-removal-default', nsi
        )
        self.assertIn("/NATIVE_QUIET_RUNNER_PID=", nsi)
        self.assertIn("/NATIVE_QUIET_TOKEN=", nsi)
        self.assertIn("--quiet-runner-process-id", nsi)
        self.assertIn("--quiet-token", nsi)
        self.assertIn("APP_INSTALL_FAULT_ARGS", nsi)
        self.assertIn("--install-fault", nsi)
        self.assertIn("/TIMEOUT=180000", nsi)
        self.assertIn("/TIMEOUT=30000", nsi)
        self.assertIn('StrCmp $HelperExitCode "error"', nsi)
        self.assertIn('StrCmp $HelperExitCode "timeout"', nsi)
        self.assertIn('!define APP_USER_PROFILE_ROOT "$PROFILE"', nsi)
        self.assertNotIn("REPLACED_EXECUTABLE", nsi + text(PACKAGER))
        self.assertIn("RequestExecutionLevel user", nsi)
        self.assertIn(
            'InstallDir "$LOCALAPPDATA\\Programs\\${INFO_PRODUCTNAME}"', nsi
        )
        self.assertIn('WriteUninstaller "$PLUGINSDIR\\uninstall.exe"', nsi)
        self.assertNotIn("RMDir /r", nsi)

        page_order = (
            "!insertmacro MUI_PAGE_WELCOME",
            '!insertmacro MUI_PAGE_LICENSE "${ARG_STAGE_DIR}\\LICENSE.txt"',
            "!insertmacro MUI_PAGE_INSTFILES",
            "!insertmacro MUI_PAGE_FINISH",
        )
        positions = [nsi.index(page) for page in page_order]
        self.assertEqual(positions, sorted(positions))
        self.assertIn('!include "MUI2.nsh"', nsi)
        self.assertIn("MUI_WELCOMEFINISHPAGE_BITMAP", nsi)
        self.assertIn("MUI_CUSTOMFUNCTION_GUIINIT SelectInstallerWelcomeBitmap", nsi)
        self.assertEqual(nsi.count("!pragma verifyloadimage"), 4)
        self.assertNotIn("MUI_PAGE_DIRECTORY", nsi)
        self.assertNotIn("MUI_PAGE_COMPONENTS", nsi)
        self.assertIn("SetCompressor lzma", nsi)
        self.assertIn("SetDatablockOptimize on", nsi)
        self.assertIn("SetCompressorDictSize 8", nsi)
        self.assertIn("SetCompressor /SOLID /FINAL lzma", nsi)
        self.assertIn("AllowSkipFiles off", nsi)
        self.assertIn("CRCCheck force", nsi)
        self.assertIn("ManifestDPIAware true", nsi)
        self.assertIn("SendMessageTimeoutW", nsi)
        self.assertIn(
            "Uninstall always removes the managed program, user PATH entry, and Windows Installed Apps registration",
            nsi,
        )

    def test_packager_validates_three_distinct_x64_binaries(self) -> None:
        packager = text(PACKAGER)
        self.assertIn("[string]$InstallerHelperExe", packager)
        self.assertIn("[string]$TestInstallFault", packager)
        self.assertIn("$InstallerHelperExe = (Resolve-Path", packager)
        self.assertEqual(packager.count("Assert-X64Pe -Path"), 3)
        self.assertIn("separately built native installer helper", packager)
        self.assertIn('"/DARG_HELPER_EXE=$InstallerHelperExe"', packager)
        self.assertNotIn(".ps1", packager.replace("package_windows_installer.ps1", ""))
        self.assertIn('$NsisVersion = "3.12"', packager)
        self.assertIn(
            "56581f90db321581c5381193d796fffcf2d24b2f8fed2160a6c6a3baa67f2c4f",
            packager,
        )
        self.assertIn("downloads.sourceforge.net/project/nsis", packager)
        self.assertIn(
            '$UpstreamUrl = "https://github.com/herdrdev/herdr"', packager
        )
        self.assertNotIn("https://github.com/ogulcancelik/herdr", packager)
        self.assertIn('"/WX"', packager)
        self.assertIn("Invoke-HerdrIdentityQuery", packager)
        self.assertIn('"--herdr-private-launcher-build-id-v1"', packager)
        self.assertIn("[string]$ReleaseVersion", packager)
        self.assertIn("[string]$BaseVersion", packager)
        self.assertIn('$ReleaseVersion -ceq "local"', packager)
        self.assertIn('ExpectedCli = "herdr-ext $ReleaseVersion (Herdr $BaseVersion)"', packager)
        self.assertIn("-ExpectedOutput $ExpectedCliVersion", packager)

    def test_real_fault_matrix_covers_retries_and_pending_activation(self) -> None:
        fault = text(FAULT_TEST)
        self.assertIn("[string]$InstallerHelperExe", fault)
        for stage in (
            "after-bin-directory",
            "after-uninstall-pending",
            "after-launcher-lock",
            "after-installer-helper",
            "after-state-directory",
            "before-uninstaller",
            "after-uninstaller",
            "after-user-path",
            "after-arp-registration",
            "terminate-after-installer-helper",
        ):
            self.assertIn(f'"{stage}"', fault)
        for contract in (
            "Uninstall fault retry passed",
            "Sibling-preserving skill uninstall passed",
            "Locked settings residual remained nonblocking",
            "Reparse settings residual remained nonblocking",
            "Native missing-helper repair passed",
            "Native pending-update activation passed",
            "Cross-mode uninstall retry passed",
            "Setup retry ownership passed",
            "Interrupted PATH ownership recovery passed",
            "Interrupted ARP ownership publication recovery passed",
            "Hard-termination cleanup recovery passed",
            "Incomplete current ARP update repair passed",
            "Malformed pending-launcher state did not fail closed",
            "Malformed ARP display identity did not fail closed",
            "Assert-TestUserPathRestored",
            "New-TestIdentityLauncher",
            "Start-TestLeaseHolder",
            '"state\\pending"',
            '"runtime\\$BuildId"',
            '"quiet-uninstall"',
            '"state\\installer-helper.exe"',
            '"state\\path-add.pending"',
        ):
            self.assertIn(contract, fault)
        self.assertIn("WaitForExit", fault)
        self.assertIn("taskkill.exe", fault)
        self.assertIn('("hs%-" + [Guid]::NewGuid()', fault)
        self.assertNotIn("Wait-Process", fault)

    def test_native_helper_owns_terminal_quiet_uninstall(self) -> None:
        lifecycle = text(HELPER_LIFECYCLE)
        registry = text(HELPER_REGISTRY)
        self.assertIn('arg("/S")', lifecycle)
        self.assertIn("QUIET_UNINSTALL_TIMEOUT", lifecycle)
        self.assertIn("ProcessJob::new", lifecycle)
        self.assertIn("wait_for_runner_and_remove_helper", lifecycle)
        self.assertIn("RetryOwnership", lifecycle)
        self.assertIn("restore_user_path", lifecycle)
        self.assertIn("restore_arp_registration", lifecycle)
        self.assertIn("uninstall.pending", lifecycle)
        self.assertIn("path-add.pending", lifecycle)
        self.assertIn("PATH_ADD_PENDING_CREATED_VALUE", text(HELPER_FILES))
        self.assertIn("PATH_ADD_PENDING_EXISTING_VALUE", text(HELPER_FILES))
        self.assertIn("stage_managed_directory_for_uninstall", lifecycle)
        self.assertIn("quiet-uninstall --install-root", registry)
        self.assertNotIn("powershell", registry.lower())

    def test_installer_artwork_is_an_exact_native_bmp3_set(self) -> None:
        expected_files = {"README.md", ARTWORK_SOURCE.name, *ARTWORK_DERIVATIVES}
        self.assertEqual({path.name for path in ARTWORK.iterdir()}, expected_files)

        source = ARTWORK_SOURCE.read_bytes()
        self.assertEqual(source[:8], b"\x89PNG\r\n\x1a\n")
        self.assertEqual(source[12:16], b"IHDR")
        self.assertEqual(
            struct.unpack(">IIBBBBB", source[16:29]),
            (906, 1736, 8, 2, 0, 0, 0),
        )
        self.assertEqual(
            hashlib.sha256(source).hexdigest(),
            "6bb6db2684d5b77aace0cfa7a8925277c656f74c60451013e3f890d631fbccf1",
        )

        for filename, (width, height, expected_hash) in ARTWORK_DERIVATIVES.items():
            bitmap = (ARTWORK / filename).read_bytes()
            row_size = ((width * 3 + 3) // 4) * 4
            pixel_size = row_size * height
            self.assertEqual(bitmap[:2], b"BM", filename)
            self.assertEqual(struct.unpack_from("<I", bitmap, 2)[0], len(bitmap), filename)
            self.assertEqual(struct.unpack_from("<I", bitmap, 10)[0], 54, filename)
            self.assertEqual(struct.unpack_from("<I", bitmap, 14)[0], 40, filename)
            self.assertEqual(struct.unpack_from("<ii", bitmap, 18), (width, height), filename)
            self.assertEqual(struct.unpack_from("<H", bitmap, 26)[0], 1, filename)
            self.assertEqual(struct.unpack_from("<H", bitmap, 28)[0], 24, filename)
            self.assertEqual(struct.unpack_from("<I", bitmap, 30)[0], 0, filename)
            self.assertEqual(struct.unpack_from("<I", bitmap, 34)[0], pixel_size, filename)
            self.assertEqual(len(bitmap), 54 + pixel_size, filename)
            self.assertEqual(hashlib.sha256(bitmap).hexdigest(), expected_hash, filename)

        notes = text(ARTWORK / "README.md")
        self.assertIn("ImageMagick", notes)
        self.assertIn("7.1.2-29 Q16-HDRI", notes)
        self.assertIn("-filter Lanczos", notes)
        self.assertIn("filter:lobes=3", notes)
        self.assertIn('"BMP3:installer-welcome-finish-$size.bmp"', notes)

    def test_updater_owns_bounded_installer_process_cleanup(self) -> None:
        platform = text(WINDOWS_PLATFORM)
        updater = text(UPDATE)
        self.assertIn("JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE", platform)
        self.assertIn("AssignProcessToJobObject", platform)
        self.assertIn("TerminateJobObject", platform)
        self.assertIn("wait_child_bounded", platform)
        boundary = platform[
            platform.index("pub(crate) fn wait_child_bounded") : platform.index(
                "pub fn write_clipboard"
            )
        ]
        self.assertNotIn("child.wait()", boundary)
        self.assertIn("HERDR_INSTALLER_START_GATE_V1", updater)
        self.assertIn("new_kill_on_close", updater)
        self.assertIn("terminate_and_wait", updater)


if __name__ == "__main__":
    unittest.main()
