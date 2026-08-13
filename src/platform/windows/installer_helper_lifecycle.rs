use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    marker::PhantomData,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::{MetadataExt as _, OpenOptionsExt as _},
        process::CommandExt as _,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE,
        WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Storage::FileSystem::{
        CreateFileW, FileDispositionInfoEx, FileRenameInfo, SetFileInformationByHandle, DELETE,
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_FLAG_DELETE,
        FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO_EX,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_RENAME_INFO, OPEN_EXISTING,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            CreateMutexW, OpenProcess, QueryFullProcessImageNameW, ReleaseMutex,
            WaitForSingleObject, CREATE_NO_WINDOW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

use crate::{
    managed_install::{
        BuildId, ManagedInstall, INSTALLER_STOP_SESSIONS_COMMAND, PENDING_LAUNCHER_PREFIX,
        PENDING_LAUNCHER_SUFFIX, UNINSTALL_PENDING_MARKER, WINGET_PACKAGE_MANAGER_RECORD,
    },
    windows_managed_install::CoordinationLease,
    windows_process_job::ChildProcessJob,
};

use super::{
    installer_helper_files as files, installer_helper_registry as registry,
    installer_helper_skills::{self as skills, SkillDisposition},
};

const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_RETRY: Duration = Duration::from_millis(50);
const QUIET_UNINSTALL_OPERATION_TIMEOUT: Duration = Duration::from_secs(25);
const QUIET_UNINSTALL_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const INSTALLER_STOP_TIMEOUT: Duration = Duration::from_secs(35);
const INSTALLER_STOP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const INSTALLER_STOP_RETRY_GUIDANCE: &str = "Close all managed Herdr sessions and retry uninstall.";
const LIFECYCLE_MUTEX_NAME: &str = r"Local\HerdrWinInstallerLifecycle";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallManager {
    Direct,
    WinGet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsDisposition {
    Keep,
    Remove,
}

#[derive(Clone, Debug)]
pub(crate) struct InstallOptions {
    pub(crate) install_root: PathBuf,
    pub(crate) user_profile_root: PathBuf,
    pub(crate) package_root: PathBuf,
    pub(crate) build_id: BuildId,
    pub(crate) display_version: String,
    pub(crate) numeric_version: String,
    pub(crate) install_manager: InstallManager,
    pub(crate) fault: Option<String>,
    pub(crate) fault_marker_prefix: String,
}

#[derive(Clone, Debug)]
pub(crate) struct UninstallOptions {
    pub(crate) install_root: PathBuf,
    pub(crate) user_profile_root: PathBuf,
    pub(crate) skill_hash_manifest: PathBuf,
    pub(crate) settings_disposition: SettingsDisposition,
    pub(crate) skill_disposition: SkillDisposition,
    pub(crate) fault: Option<String>,
    pub(crate) fault_marker_prefix: String,
    pub(crate) quiet_runner: Option<QuietRunnerOptions>,
}

#[derive(Clone, Debug)]
pub(crate) struct QuietRunnerOptions {
    pub(crate) process_id: u32,
    pub(crate) token: String,
}

#[derive(Clone, Debug)]
pub(crate) struct QuietUninstallOptions {
    pub(crate) install_root: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct SkillDefaultOptions {
    pub(crate) user_profile_root: PathBuf,
    pub(crate) skill_hash_manifest: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct MaintenanceOptions {
    pub(crate) install_root: PathBuf,
    pub(crate) parent_process_id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootKind {
    New,
    ManagedNative,
    UninstallRetry,
    UninstallResidual,
}

#[derive(Debug)]
struct Staging {
    path: PathBuf,
    kind: &'static str,
    install_root: PathBuf,
}

#[derive(Debug)]
struct LeaseStatus {
    active: Vec<PathBuf>,
    stale: Vec<PathBuf>,
    ambiguous: Vec<PathBuf>,
}

#[derive(Debug)]
struct ProcessHandle(HANDLE);

#[derive(Clone, Debug)]
struct QuietSession {
    process_id: u32,
    result_path: PathBuf,
    moved_helper_path: PathBuf,
}

struct LifecycleMutex {
    handle: HANDLE,
    _same_thread: PhantomData<Rc<()>>,
}

#[derive(Clone, Debug)]
struct FileSnapshot {
    bytes: Vec<u8>,
    sha256: String,
}

#[derive(Clone, Debug)]
struct RetryOwnership {
    helper: FileSnapshot,
    launcher_lock: FileSnapshot,
    path_add_pending: Option<FileSnapshot>,
    uninstaller: FileSnapshot,
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: this wrapper owns the process or snapshot handle.
            unsafe { CloseHandle(self.0) };
        }
    }
}

impl Drop for LifecycleMutex {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: successful acquisition is retained on this same thread until drop.
            let _ = unsafe { ReleaseMutex(self.handle) };
            // SAFETY: this guard exclusively owns the non-null mutex handle.
            unsafe { CloseHandle(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}

impl QuietSession {
    fn new(install_root: &Path, options: &QuietRunnerOptions) -> io::Result<Self> {
        validate_quiet_token(&options.token)?;
        if options.process_id == 0 || options.process_id == std::process::id() {
            return Err(files::invalid_data(
                "quiet-uninstall runner process ID is invalid",
            ));
        }
        let expected = install_root.join("state").join(files::NATIVE_HELPER_NAME);
        files::assert_regular_file(&expected)?;
        let actual = process_path(options.process_id).ok_or_else(|| {
            files::invalid_data("quiet-uninstall runner process is not available")
        })?;
        if !files::path_eq(&actual, &expected)? {
            return Err(files::invalid_data(
                "quiet-uninstall runner is not the installed native helper",
            ));
        }
        let (result_path, moved_helper_path) = quiet_paths(install_root, &options.token)?;
        files::assert_regular_file(&result_path)?;
        if fs::read(&result_path)? != files::QUIET_UNINSTALL_PENDING {
            return Err(files::invalid_data(
                "quiet-uninstall rendezvous is not pending",
            ));
        }
        if files::path_exists(&moved_helper_path)? {
            return Err(files::invalid_data(
                "quiet-uninstall helper handoff path already exists",
            ));
        }
        Ok(Self {
            process_id: options.process_id,
            result_path,
            moved_helper_path,
        })
    }

    fn publish_result(&self, success: bool) -> io::Result<()> {
        files::assert_regular_file(&self.result_path)?;
        if fs::read(&self.result_path)? != files::QUIET_UNINSTALL_PENDING {
            return Err(files::invalid_data(
                "quiet-uninstall rendezvous changed before completion",
            ));
        }
        let replacement = self
            .result_path
            .with_file_name(format!("quiet-result.{}.new", files::unique_hex()));
        files::write_durable(
            &replacement,
            if success {
                files::QUIET_UNINSTALL_SUCCESS
            } else {
                files::QUIET_UNINSTALL_FAILURE
            },
        )?;
        files::replace_file(&self.result_path, &replacement, None)
    }
}

impl FileSnapshot {
    fn capture(path: &Path) -> io::Result<Self> {
        files::assert_regular_file(path)?;
        let bytes = fs::read(path)?;
        Ok(Self {
            sha256: files::sha256_bytes(&bytes),
            bytes,
        })
    }

    fn empty() -> Self {
        Self {
            bytes: Vec::new(),
            sha256: files::sha256_bytes(&[]),
        }
    }

    fn restore(&self, path: &Path) -> io::Result<()> {
        if files::path_exists(path)? {
            files::assert_regular_file(path)?;
            if files::sha256(path)? != self.sha256 {
                return Err(files::invalid_data(format!(
                    "retry ownership file changed before restoration: {}",
                    path.display()
                )));
            }
        } else {
            files::write_durable(path, &self.bytes)?;
        }
        if files::sha256(path)? != self.sha256 {
            return Err(files::invalid_data(format!(
                "restored retry ownership file differs from its snapshot: {}",
                path.display()
            )));
        }
        Ok(())
    }
}

impl RetryOwnership {
    fn capture(install_root: &Path) -> io::Result<Self> {
        let state = install_root.join("state");
        let installed_helper = state.join(files::NATIVE_HELPER_NAME);
        let helper_source = if files::path_exists(&installed_helper)? {
            installed_helper
        } else {
            std::env::current_exe()?
        };
        let launcher_lock = state.join("launcher.lock");
        let path_add_pending = state.join("path-add.pending");
        Ok(Self {
            helper: FileSnapshot::capture(&helper_source)?,
            launcher_lock: if files::path_exists(&launcher_lock)? {
                FileSnapshot::capture(&launcher_lock)?
            } else {
                FileSnapshot::empty()
            },
            path_add_pending: if registry::path_add_pending_value_created(&path_add_pending)?
                .is_some()
            {
                Some(FileSnapshot::capture(&path_add_pending)?)
            } else {
                None
            },
            uninstaller: FileSnapshot::capture(&install_root.join("uninstall.exe"))?,
        })
    }

    fn restore(&self, install_root: &Path) -> io::Result<()> {
        if files::path_exists(install_root)? {
            files::assert_regular_dir(install_root)?;
        } else {
            let parent = install_root
                .parent()
                .ok_or_else(|| files::invalid_data("install root has no parent"))?;
            files::assert_regular_dir(parent)?;
            fs::create_dir(install_root)?;
        }
        let state = install_root.join("state");
        if files::path_exists(&state)? {
            files::assert_regular_dir(&state)?;
        } else {
            fs::create_dir(&state)?;
        }
        self.helper
            .restore(&state.join(files::NATIVE_HELPER_NAME))?;
        self.launcher_lock.restore(&state.join("launcher.lock"))?;
        if let Some(path_add_pending) = &self.path_add_pending {
            path_add_pending.restore(&state.join("path-add.pending"))?;
        }
        let marker = state.join(UNINSTALL_PENDING_MARKER);
        if files::path_exists(&marker)? {
            files::assert_regular_file(&marker)?;
            if fs::read(&marker)? != files::UNINSTALL_MARKER {
                return Err(files::invalid_data(
                    "uninstall retry ownership marker changed before restoration",
                ));
            }
        } else {
            files::write_durable(&marker, files::UNINSTALL_MARKER)?;
        }
        self.uninstaller
            .restore(&install_root.join("uninstall.exe"))
    }
}

fn validate_quiet_token(token: &str) -> io::Result<()> {
    if token.len() != 32
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(files::invalid_data("invalid quiet-uninstall token"));
    }
    Ok(())
}

fn quiet_paths(install_root: &Path, token: &str) -> io::Result<(PathBuf, PathBuf)> {
    validate_quiet_token(token)?;
    let parent = install_root
        .parent()
        .ok_or_else(|| files::invalid_data("install root has no parent"))?;
    files::assert_regular_dir(parent)?;
    let leaf = install_root
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| files::invalid_data("install root has no UTF-8 leaf"))?;
    Ok((
        parent.join(format!("{leaf}.quiet-uninstall.{token}.result")),
        parent.join(format!("{leaf}.quiet-uninstall.{token}.exe")),
    ))
}

pub(crate) fn install(options: InstallOptions) -> io::Result<String> {
    let install_root = files::full_path(&options.install_root)?;
    let profile = skills::user_profile_root(&options.user_profile_root)?;
    let _lifecycle = acquire_lifecycle_lock()?;
    registry::assert_arp_ownership(&install_root)?;
    let allow_convergence = registry::arp_exists()?;
    let path_add_pending = install_root.join("state").join("path-add.pending");
    let previous_path_ownership = current_path_ownership(&install_root, &path_add_pending)?;
    let agent_root = skills::agent_skills_root(&profile)?;
    let claude_root = if skills::claude_installed(&profile)? {
        Some(skills::claude_skills_root(
            &profile,
            std::env::var("CLAUDE_CONFIG_DIR").ok().as_deref(),
        )?)
    } else {
        None
    };
    let result = install_layout(
        &install_root,
        &options.package_root,
        &options.build_id,
        &options.display_version,
        &options.numeric_version,
        options.install_manager,
        &agent_root,
        claude_root.as_deref(),
        allow_convergence,
    )?;
    let path_update = registry::add_user_path(
        &install_root.join("bin"),
        previous_path_ownership,
        &path_add_pending,
    )?;
    inject_fault(
        "install-after-user-path",
        options.fault.as_deref(),
        &options.fault_marker_prefix,
    )?;
    registry::set_arp_registration(
        &install_root,
        &options.display_version,
        &options.numeric_version,
        path_update.owned,
        path_update.value_created,
        || {
            inject_fault(
                "install-after-arp-path-added",
                options.fault.as_deref(),
                &options.fault_marker_prefix,
            )
        },
    )?;
    remove_file_if_exists(&path_add_pending)?;
    Ok(result)
}

pub(crate) fn uninstall(options: UninstallOptions) -> io::Result<String> {
    let install_root = files::full_path(&options.install_root)?;
    let quiet = options
        .quiet_runner
        .as_ref()
        .map(|value| QuietSession::new(&install_root, value))
        .transpose()?;
    let _lifecycle = acquire_lifecycle_lock()?;
    let retry_ownership = if files::path_exists(&install_root)? {
        Some(RetryOwnership::capture(&install_root)?)
    } else {
        None
    };
    let arp_snapshot = registry::snapshot_arp_registration(&install_root)?;
    let mut path_rollback = None;
    let mut arp_removed = false;
    let mut residual_ready = false;
    let result = (|| {
        let profile = skills::user_profile_root(&options.user_profile_root)?;
        let known = skills::read_managed_skill_hashes(&options.skill_hash_manifest, None)?;
        registry::assert_arp_ownership(&install_root)?;
        let allow_convergence = registry::arp_exists()?;
        let path_add_pending = install_root.join("state").join("path-add.pending");
        let path_ownership = current_path_ownership(&install_root, &path_add_pending)?;
        let agent_root = skills::agent_skills_root(&profile)?;
        let claude_roots = skills::claude_roots_for_removal(&profile)?;
        let mut warnings = Vec::new();
        let (preserved, warning) = uninstall_layout(
            &install_root,
            &agent_root,
            &claude_roots,
            &known,
            options.skill_disposition,
            allow_convergence,
            quiet.as_ref(),
            options.fault.as_deref(),
            &options.fault_marker_prefix,
        )?;
        if let Some(warning) = warning {
            warnings.push(warning);
        }
        residual_ready = true;
        let (_, rollback) = registry::remove_user_path(&install_root.join("bin"), path_ownership)?;
        path_rollback = rollback;
        arp_removed = registry::remove_arp_registration(&install_root)?;
        inject_fault(
            "after-arp-registration",
            options.fault.as_deref(),
            &options.fault_marker_prefix,
        )?;
        remove_uninstall_residual(
            &install_root,
            options.fault.as_deref(),
            &options.fault_marker_prefix,
            quiet.as_ref(),
        )?;
        if options.settings_disposition == SettingsDisposition::Remove {
            if let Err(err) = skills::remove_user_settings(&profile) {
                warnings.push(format!(
                "Warning: Selected Herdr settings cleanup was incomplete; locked or unsafe settings were preserved. {err}"
            ));
            }
        }
        remove_fault_marker(options.fault.as_deref(), &options.fault_marker_prefix)?;
        let mut output = String::from("Herdr Win uninstall cleanup is ready.");
        for path in preserved {
            output.push_str(&format!("\nPreserved Herdr skill: {}", path.display()));
        }
        for warning in warnings {
            output.push_str(&format!("\n{warning}"));
        }
        Ok(output)
    })();
    let result = if let Err(original) = result {
        if residual_ready {
            let mut restore_errors = Vec::new();
            if let Some(retry_ownership) = &retry_ownership {
                if let Err(err) = retry_ownership.restore(&install_root) {
                    restore_errors.push(format!("filesystem ownership: {err}"));
                }
            }
            if let Some(rollback) = &path_rollback {
                if let Err(err) = registry::restore_user_path(rollback) {
                    restore_errors.push(format!("PATH ownership: {err}"));
                }
            }
            if arp_removed {
                if let Some(snapshot) = &arp_snapshot {
                    if let Err(err) = registry::restore_arp_registration(snapshot) {
                        restore_errors.push(format!("ARP ownership: {err}"));
                    }
                }
            }
            if restore_errors.is_empty() {
                Err(original)
            } else {
                Err(io::Error::other(format!(
                    "{original}; uninstall retry restoration failed: {}",
                    restore_errors.join("; ")
                )))
            }
        } else {
            Err(original)
        }
    } else {
        result
    };
    if let Some(quiet) = &quiet {
        let success = result.is_ok();
        let moved_helper_exists = files::path_exists(&quiet.moved_helper_path)?;
        if success && !moved_helper_exists {
            return Err(files::invalid_data(
                "quiet uninstall completed without its terminal runner handoff image",
            ));
        }
        if moved_helper_exists {
            delete_running_executable(&quiet.moved_helper_path)?;
        }
        quiet.publish_result(success)?;
    }
    result
}

pub(crate) fn quiet_uninstall(options: QuietUninstallOptions) -> io::Result<String> {
    let install_root = files::full_path(&options.install_root)?;
    let expected_helper = install_root.join("state").join(files::NATIVE_HELPER_NAME);
    let current = std::env::current_exe()?;
    if !files::path_eq(&current, &expected_helper)? {
        return Err(files::invalid_data(
            "quiet uninstall must run from the installed native helper",
        ));
    }
    if !matches!(
        classify_root(&install_root, false)?,
        RootKind::ManagedNative | RootKind::UninstallRetry | RootKind::UninstallResidual
    ) {
        return Err(files::invalid_data(
            "quiet uninstall requires an exact native managed or retry root",
        ));
    }
    registry::assert_arp_ownership(&install_root)?;
    let token = files::unique_hex();
    let (result_path, moved_helper_path) = quiet_paths(&install_root, &token)?;
    if files::path_exists(&result_path)? || files::path_exists(&moved_helper_path)? {
        return Err(files::invalid_data(
            "quiet-uninstall rendezvous path already exists",
        ));
    }
    files::write_durable(&result_path, files::QUIET_UNINSTALL_PENDING)?;
    let uninstaller = install_root.join("uninstall.exe");
    files::assert_regular_file(&uninstaller)?;
    let job = ChildProcessJob::new_kill_on_close()?;
    let mut child = Command::new(&uninstaller)
        .arg("/S")
        .arg(format!("/NATIVE_QUIET_RUNNER_PID={}", std::process::id()))
        .arg(format!("/NATIVE_QUIET_TOKEN={token}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| files::contextual(err, "failed to start native quiet uninstall"))?;
    if let Err(err) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        remove_file_if_exists(&result_path)?;
        return Err(files::contextual(
            err,
            "failed to contain native quiet uninstall process tree",
        ));
    }
    let deadline = Instant::now() + QUIET_UNINSTALL_OPERATION_TIMEOUT;
    loop {
        if let Some(status) = read_quiet_result(&result_path)? {
            if status != files::QUIET_UNINSTALL_PENDING {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if let Err(err) = job.wait_for_empty(
                    &mut child,
                    remaining,
                    "quiet-uninstall process tree remained active after terminal result",
                ) {
                    if err.kind() == io::ErrorKind::TimedOut {
                        job.terminate_and_wait(&mut child, QUIET_UNINSTALL_CLEANUP_TIMEOUT)?;
                    }
                    remove_file_if_exists(&result_path)?;
                    return Err(err);
                }
                remove_file_if_exists(&result_path)?;
                return match status.as_slice() {
                    files::QUIET_UNINSTALL_SUCCESS => {
                        Ok("Herdr Win quiet uninstall completed.".to_string())
                    }
                    files::QUIET_UNINSTALL_FAILURE => Err(files::invalid_data(
                        "native quiet uninstall reported failure",
                    )),
                    _ => Err(files::invalid_data(
                        "native quiet uninstall returned malformed status",
                    )),
                };
            }
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                if job.active_processes()? != 0 {
                    job.terminate_and_wait(&mut child, QUIET_UNINSTALL_CLEANUP_TIMEOUT)?;
                }
                remove_file_if_exists(&result_path)?;
                return Err(files::invalid_data(format!(
                    "native quiet uninstall bootstrap exited with {status}"
                )));
            }
        }
        if Instant::now() >= deadline {
            job.terminate_and_wait(&mut child, QUIET_UNINSTALL_CLEANUP_TIMEOUT)?;
            remove_file_if_exists(&result_path)?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native quiet uninstall exceeded its 30 second total deadline",
            ));
        }
        thread::sleep(LOCK_RETRY);
    }
}

fn read_quiet_result(path: &Path) -> io::Result<Option<Vec<u8>>> {
    if !files::path_exists(path)? {
        return Ok(None);
    }
    if let Err(err) = files::assert_regular_file(path) {
        return if err.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(err)
        };
    }
    match fs::read(path) {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(files::contextual(
            err,
            format!("failed to read quiet-uninstall result: {}", path.display()),
        )),
    }
}

pub(crate) fn skill_removal_default(options: SkillDefaultOptions) -> io::Result<String> {
    let profile = skills::user_profile_root(&options.user_profile_root)?;
    let known = skills::read_managed_skill_hashes(&options.skill_hash_manifest, None)?;
    let agent = skills::agent_skills_root(&profile)?;
    let claude = skills::claude_roots_for_removal(&profile)?;
    Ok(skills::skill_removal_default(&agent, &claude, &known)?.to_string())
}

pub(crate) fn complete_maintenance(options: MaintenanceOptions) -> io::Result<String> {
    if !files::wait_for_process(options.parent_process_id, LOCK_TIMEOUT)? {
        return Ok("Herdr Win maintenance: Deferred".to_string());
    }
    let install_root = files::full_path(&options.install_root)?;
    let _lifecycle = acquire_lifecycle_lock()?;
    if !files::path_exists(&install_root)? {
        return Ok("Herdr Win maintenance: Missing".to_string());
    }
    repair_launcher_publication(&install_root)?;
    remove_stale_staging(&install_root);
    if !matches!(
        classify_root(&install_root, false),
        Ok(RootKind::ManagedNative)
    ) {
        return Ok("Herdr Win maintenance: Deferred".to_string());
    }
    let _coordination =
        acquire_coordination(&ManagedInstall::new(install_root.clone()), LOCK_TIMEOUT)?;
    maintenance_locked(&install_root)?;
    Ok("Herdr Win maintenance: Complete".to_string())
}

fn install_layout(
    install_root: &Path,
    package_root: &Path,
    build_id: &BuildId,
    display_version: &str,
    numeric_version: &str,
    requested_manager: InstallManager,
    agent_skills_root: &Path,
    claude_skills_root: Option<&Path>,
    allow_convergence: bool,
) -> io::Result<String> {
    let package_root = files::full_path(package_root)?;
    let stage = package_root.join("payload");
    let launcher = package_root.join("app-launcher.exe");
    let helper = package_root.join("installer-helper.exe");
    let uninstaller = package_root.join("uninstall.exe");
    let skill = package_root.join("skill").join("SKILL.md");
    let skill_hashes = package_root.join("skill").join("managed-skill-hashes.txt");
    files::assert_regular_dir(&stage)?;
    for path in [&launcher, &helper, &uninstaller, &skill, &skill_hashes] {
        files::assert_regular_file(path)?;
    }
    let queried = files::query_launcher_build_id(&launcher, LOCK_TIMEOUT)?;
    if queried != *build_id {
        return Err(files::invalid_data(format!(
            "launcher build ID {} does not match runtime {}",
            queried.as_str(),
            build_id.as_str()
        )));
    }
    files::validate_version_identity(display_version, numeric_version)?;
    let known = skills::read_managed_skill_hashes(&skill_hashes, Some(&skill))?;
    remove_stale_staging(install_root);
    if unsupported_launcher_hop(install_root)? {
        return Err(files::incompatible_installation());
    }
    let mut kind = match classify_root(install_root, true) {
        Ok(value) => value,
        Err(err) if allow_convergence => {
            let _ = writeln!(
                io::stderr().lock(),
                "Warning: The registered current Herdr root could not use normal repair and will be rebuilt directly. {err}"
            );
            remove_current_root_for_convergence(install_root)?;
            RootKind::New
        }
        Err(err) => return Err(err),
    };
    if kind == RootKind::UninstallRetry {
        let retry_ownership = RetryOwnership::capture(install_root)?;
        let cleanup = (|| {
            let _ = uninstall_layout(
                install_root,
                agent_skills_root,
                &claude_skills_root
                    .into_iter()
                    .map(Path::to_path_buf)
                    .collect::<Vec<_>>(),
                &known,
                SkillDisposition::Keep,
                true,
                None,
                None,
                "herdr",
            )?;
            remove_uninstall_residual(install_root, None, "herdr", None)
        })();
        if let Err(original) = cleanup {
            return match retry_ownership.restore(install_root) {
                Ok(()) => Err(original),
                Err(restore) => Err(io::Error::other(format!(
                    "{original}; setup could not restore uninstall retry ownership: {restore}"
                ))),
            };
        }
        kind = RootKind::New;
    } else if kind == RootKind::UninstallResidual {
        remove_uninstall_residual(install_root, None, "herdr", None)?;
        kind = RootKind::New;
    }
    let effective_manager =
        if validate_package_manager_marker(&install_root.join("state").join("package-manager"))
            .unwrap_or(false)
        {
            InstallManager::WinGet
        } else {
            requested_manager
        };
    let status = match kind {
        RootKind::ManagedNative => install_upgrade(
            install_root,
            &stage,
            &launcher,
            &helper,
            &uninstaller,
            build_id,
            display_version,
            numeric_version,
            effective_manager,
        )?,
        RootKind::New => install_fresh(
            install_root,
            &stage,
            &launcher,
            &helper,
            &uninstaller,
            build_id,
            display_version,
            numeric_version,
            effective_manager,
        )?,
        RootKind::UninstallRetry | RootKind::UninstallResidual => unreachable!(),
    };
    let preserved =
        skills::install_skill_copies(&skill, agent_skills_root, claude_skills_root, &known)?;
    let mut output = if status == "Pending" {
        format!(
            "Herdr Win {}: Pending; staged until old sessions exit.",
            build_id.as_str()
        )
    } else {
        format!("Herdr Win {}: {status}", build_id.as_str())
    };
    for path in preserved {
        output.push_str(&format!(
            "\nWarning: Existing customized Herdr skill was preserved: {}",
            path.display()
        ));
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn install_fresh(
    install_root: &Path,
    stage: &Path,
    launcher: &Path,
    helper: &Path,
    uninstaller: &Path,
    build_id: &BuildId,
    display_version: &str,
    numeric_version: &str,
    manager: InstallManager,
) -> io::Result<&'static str> {
    let staging = new_staging("fresh", install_root)?;
    let root = staging.path.join("root");
    fs::create_dir(&root)?;
    fs::create_dir(root.join("bin"))?;
    fs::create_dir_all(root.join("bin").join("managed-install-v1"))?;
    fs::create_dir(root.join("runtime"))?;
    fs::create_dir(root.join("state"))?;
    fs::create_dir(root.join("state").join("leases"))?;
    files::copy_durable_file(launcher, &root.join("bin").join("herdr.exe"))?;
    files::write_durable(
        &root.join("bin").join("managed-install-v1").join("marker"),
        files::MANAGED_BIN_MARKER,
    )?;
    files::create_runtime_tree(
        &root.join("runtime").join(build_id.as_str()),
        stage,
        build_id,
    )?;
    files::write_durable(&root.join("state").join("launcher.lock"), &[])?;
    files::write_durable(
        &root.join("state").join("active"),
        files::pointer_text(build_id).as_bytes(),
    )?;
    files::copy_durable_file(helper, &root.join("state").join(files::NATIVE_HELPER_NAME))?;
    files::copy_durable_file(uninstaller, &root.join("uninstall.exe"))?;
    files::write_durable(
        &root.join("state").join("install.manifest"),
        files::install_manifest_text(
            &files::sha256(&root.join("bin").join("herdr.exe"))?,
            display_version,
            numeric_version,
        )?
        .as_bytes(),
    )?;
    set_package_manager_marker(&root.join("state"), manager)?;
    validate_managed_root(&root, false)?;
    if files::path_exists(install_root)? {
        return Err(files::invalid_data(
            "install root appeared before fresh publication",
        ));
    }
    fs::rename(&root, install_root)?;
    cleanup_staging(&staging);
    Ok("Activated")
}

#[allow(clippy::too_many_arguments)]
fn install_upgrade(
    install_root: &Path,
    stage: &Path,
    launcher: &Path,
    helper: &Path,
    uninstaller: &Path,
    build_id: &BuildId,
    display_version: &str,
    numeric_version: &str,
    manager: InstallManager,
) -> io::Result<&'static str> {
    let staging = new_staging("update", install_root)?;
    let staged_runtime = staging.path.join("runtime");
    files::create_runtime_tree(&staged_runtime, stage, build_id)?;
    let metadata = staging.path.join("metadata");
    fs::create_dir(&metadata)?;
    files::copy_durable_file(helper, &metadata.join(files::NATIVE_HELPER_NAME))?;
    files::copy_durable_file(uninstaller, &metadata.join("uninstall.exe"))?;
    files::write_durable(
        &metadata.join("pending"),
        files::pointer_text(build_id).as_bytes(),
    )?;
    let install = ManagedInstall::new(install_root.to_path_buf());
    let _coordination = acquire_coordination(&install, LOCK_TIMEOUT)?;
    validate_managed_root(install_root, true)?;
    files::write_durable(
        &metadata.join("install.manifest"),
        files::install_manifest_text(
            &files::sha256(&install_root.join("bin").join("herdr.exe"))?,
            display_version,
            numeric_version,
        )?
        .as_bytes(),
    )?;
    let runtime_destination = install_root.join("runtime").join(build_id.as_str());
    if files::path_exists(&runtime_destination)? {
        files::validate_runtime_directory(&runtime_destination, build_id)?;
        files::validate_runtime_directory(&staged_runtime, build_id)?;
        if fs::read(runtime_destination.join("runtime.manifest"))?
            != fs::read(staged_runtime.join("runtime.manifest"))?
        {
            return Err(files::invalid_data(format!(
                "existing runtime {} differs from staged payload",
                build_id.as_str()
            )));
        }
        files::remove_validated_directory(&staged_runtime)?;
    } else {
        fs::rename(&staged_runtime, &runtime_destination)?;
    }
    let state = install_root.join("state");
    files::publish_file(
        &metadata.join(files::NATIVE_HELPER_NAME),
        &state.join(files::NATIVE_HELPER_NAME),
        &staging.path,
    )?;
    files::publish_file(
        &metadata.join("uninstall.exe"),
        &install_root.join("uninstall.exe"),
        &staging.path,
    )?;
    set_pending_launcher(install_root, launcher, build_id)?;
    let active = install.read_required_active_pointer()?;
    let pending = install.pointer_path("pending");
    if active == *build_id {
        remove_file_if_exists(&pending)?;
        files::publish_file(
            &metadata.join("install.manifest"),
            &state.join("install.manifest"),
            &staging.path,
        )?;
        maintenance_locked(install_root)?;
        set_package_manager_marker(&state, manager)?;
        cleanup_staging(&staging);
        return Ok("AlreadyActive");
    }
    files::publish_file(&metadata.join("pending"), &pending, &staging.path)?;
    let leases = lease_status(&state.join("leases"))?;
    if !leases.active.is_empty() || !leases.ambiguous.is_empty() {
        files::publish_file(
            &metadata.join("install.manifest"),
            &state.join("install.manifest"),
            &staging.path,
        )?;
        maintenance_locked(install_root)?;
        set_package_manager_marker(&state, manager)?;
        cleanup_staging(&staging);
        return Ok("Pending");
    }
    remove_stale_leases(&leases)?;
    files::move_replace(&pending, &state.join("active"))?;
    if install.read_required_active_pointer()? != *build_id || files::path_exists(&pending)? {
        return Err(files::invalid_data(
            "pending activation did not publish expected active pointer",
        ));
    }
    files::publish_file(
        &metadata.join("install.manifest"),
        &state.join("install.manifest"),
        &staging.path,
    )?;
    maintenance_locked(install_root)?;
    set_package_manager_marker(&state, manager)?;
    cleanup_staging(&staging);
    Ok("Activated")
}

fn set_pending_launcher(
    install_root: &Path,
    candidate: &Path,
    build_id: &BuildId,
) -> io::Result<()> {
    if files::query_launcher_build_id(candidate, LOCK_TIMEOUT)? != *build_id {
        return Err(files::invalid_data("candidate launcher build ID mismatch"));
    }
    let state = install_root.join("state");
    let manifest = files::read_install_manifest(&state.join("install.manifest"))?;
    validate_managed_bin(&install_root.join("bin"), &manifest.bootstrap_sha256)?;
    let installed = install_root.join("bin").join("herdr.exe");
    let candidate_hash = files::sha256(candidate)?;
    if let Some(existing) = files::pending_launcher(&state)? {
        if existing.sha256 != candidate_hash {
            fs::remove_file(existing.path)?;
        }
    }
    if files::sha256(&installed)? == candidate_hash {
        if let Some(existing) = files::pending_launcher(&state)? {
            fs::remove_file(existing.path)?;
        }
        return Ok(());
    }
    if files::pending_launcher(&state)?.is_none() {
        files::copy_durable_file(
            candidate,
            &state.join(format!(
                "{PENDING_LAUNCHER_PREFIX}{candidate_hash}{PENDING_LAUNCHER_SUFFIX}"
            )),
        )?;
    }
    Ok(())
}

fn maintenance_locked(install_root: &Path) -> io::Result<()> {
    repair_launcher_publication(install_root)?;
    validate_managed_root(install_root, false)?;
    remove_inactive_runtimes(install_root)?;
    let _ = complete_launcher_update_locked(install_root)?;
    repair_launcher_publication(install_root)?;
    validate_managed_root(install_root, false)
}

fn complete_launcher_update_locked(install_root: &Path) -> io::Result<bool> {
    repair_launcher_publication(install_root)?;
    let state = install_root.join("state");
    let Some(pending) = files::pending_launcher(&state)? else {
        return Ok(false);
    };
    let leases = lease_status(&state.join("leases"))?;
    if !leases.active.is_empty() || !leases.ambiguous.is_empty() {
        return Ok(false);
    }
    let active = ManagedInstall::new(install_root.to_path_buf()).read_required_active_pointer()?;
    if files::query_launcher_build_id(&pending.path, LOCK_TIMEOUT)? != active {
        return Err(files::invalid_data(
            "pending launcher build ID does not match active runtime",
        ));
    }
    let launcher = install_root.join("bin").join("herdr.exe");
    let replacement = install_root
        .join("bin")
        .join(files::LAUNCHER_REPLACEMENT_NAME);
    remove_file_if_exists(&replacement)?;
    files::copy_durable_file(&pending.path, &replacement)?;
    let staging = new_staging("update", install_root)?;
    let backup = staging
        .path
        .join(format!("launcher.backup.{}", files::unique_hex()));
    if let Err(err) = files::replace_file(&launcher, &replacement, Some(&backup)) {
        remove_file_if_exists(&replacement)?;
        cleanup_staging(&staging);
        if err.kind() == io::ErrorKind::PermissionDenied {
            return Ok(false);
        }
        return Ok(false);
    }
    remove_file_if_exists(&backup)?;
    cleanup_staging(&staging);
    if files::sha256(&launcher)? != pending.sha256 {
        return Err(files::invalid_data(
            "published launcher does not match staged hash",
        ));
    }
    repair_launcher_publication(install_root)?;
    Ok(true)
}

fn repair_launcher_publication(install_root: &Path) -> io::Result<()> {
    if !files::path_exists(install_root)? {
        return Ok(());
    }
    files::assert_regular_dir(install_root)?;
    let state = install_root.join("state");
    let manifest_path = state.join("install.manifest");
    if !files::path_exists(&manifest_path)? {
        return Ok(());
    }
    let manifest = files::read_install_manifest(&manifest_path)?;
    let launcher = install_root.join("bin").join("herdr.exe");
    files::assert_regular_file(&launcher)?;
    let pending = files::pending_launcher(&state)?;
    let replacement = install_root
        .join("bin")
        .join(files::LAUNCHER_REPLACEMENT_NAME);
    if files::path_exists(&replacement)? {
        files::assert_regular_file(&replacement)?;
        if pending
            .as_ref()
            .map(|value| files::sha256(&replacement).map(|hash| hash != value.sha256))
            .transpose()?
            .unwrap_or(true)
        {
            return Err(files::invalid_data(
                "unrecognized managed launcher replacement file",
            ));
        }
        fs::remove_file(&replacement)?;
    }
    let installed_hash = files::sha256(&launcher)?;
    if installed_hash == manifest.bootstrap_sha256 {
        if let Some(pending) = pending {
            if pending.sha256 == installed_hash {
                fs::remove_file(pending.path)?;
            }
        }
        return Ok(());
    }
    let Some(pending) = pending else {
        return Err(files::invalid_data(
            "managed launcher hash matches neither manifest nor pending launcher",
        ));
    };
    if pending.sha256 != installed_hash {
        return Err(files::invalid_data(
            "managed launcher hash matches neither manifest nor pending launcher",
        ));
    }
    let active = ManagedInstall::new(install_root.to_path_buf()).read_required_active_pointer()?;
    if files::query_launcher_build_id(&pending.path, LOCK_TIMEOUT)? != active {
        return Err(files::invalid_data(
            "pending launcher build ID does not match active runtime",
        ));
    }
    let staging = new_staging("update", install_root)?;
    let replacement_manifest = staging.path.join("install.manifest");
    files::write_durable(
        &replacement_manifest,
        files::install_manifest_text(
            &installed_hash,
            &manifest.display_version,
            &manifest.numeric_version,
        )?
        .as_bytes(),
    )?;
    files::publish_file(&replacement_manifest, &manifest_path, &staging.path)?;
    cleanup_staging(&staging);
    fs::remove_file(pending.path)?;
    Ok(())
}

fn remove_inactive_runtimes(install_root: &Path) -> io::Result<()> {
    let install = ManagedInstall::new(install_root.to_path_buf());
    let active = install.read_required_active_pointer()?;
    let pending = install.read_pointer("pending")?;
    let processes = process_paths()?;
    for entry in fs::read_dir(install.runtime_dir())? {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| files::invalid_data("runtime directory name is not UTF-8"))?
            .to_string();
        let build = BuildId::parse(&name)?;
        if build == active || pending.as_ref() == Some(&build) {
            continue;
        }
        files::validate_runtime_directory(&entry.path(), &build)?;
        if processes
            .iter()
            .any(|(_, path)| files::path_within(path, &entry.path()).unwrap_or(false))
        {
            continue;
        }
        if install.try_open_exclusive_lease(&build)?.is_none() {
            continue;
        }
        let staging = new_staging("update", install_root)?;
        fs::rename(entry.path(), staging.path.join("runtime"))?;
        cleanup_staging(&staging);
        remove_file_if_exists(&install.lease_path(&build))?;
    }
    Ok(())
}

fn uninstall_layout(
    install_root: &Path,
    agent_root: &Path,
    claude_roots: &[PathBuf],
    known: &BTreeSet<String>,
    skill_disposition: SkillDisposition,
    allow_convergence: bool,
    quiet: Option<&QuietSession>,
    fault: Option<&str>,
    fault_prefix: &str,
) -> io::Result<(Vec<PathBuf>, Option<String>)> {
    remove_stale_staging(install_root);
    if !files::path_exists(install_root)? {
        let (preserved, warning) = skills::remove_skill_copies_best_effort(
            agent_root,
            claude_roots,
            known,
            skill_disposition,
        );
        return Ok((preserved, warning));
    }
    let kind = match classify_root(install_root, false) {
        Ok(value) => value,
        Err(err) if allow_convergence => {
            let _ = writeln!(
                io::stderr().lock(),
                "Warning: The registered current Herdr root could not use normal uninstall recovery and will be removed directly. {err}"
            );
            remove_current_root_for_convergence(install_root)?;
            let result = skills::remove_skill_copies_best_effort(
                agent_root,
                claude_roots,
                known,
                skill_disposition,
            );
            return Ok(result);
        }
        Err(err) => return Err(err),
    };
    if kind == RootKind::UninstallResidual {
        let result = skills::remove_skill_copies_best_effort(
            agent_root,
            claude_roots,
            known,
            skill_disposition,
        );
        return Ok(result);
    }
    if !matches!(kind, RootKind::ManagedNative | RootKind::UninstallRetry) {
        return Err(files::invalid_data(
            "only an exact managed root can be uninstalled",
        ));
    }
    if kind == RootKind::UninstallRetry {
        validate_uninstall_retry_root(install_root)?;
    } else {
        validate_managed_root(install_root, false)?;
        stop_installed_sessions(install_root)?;
    }
    let install = ManagedInstall::new(install_root.to_path_buf());
    let _coordination = acquire_coordination(&install, LOCK_TIMEOUT)?;
    let leases = if files::path_exists(&install.leases_dir())? {
        lease_status(&install.leases_dir())?
    } else {
        LeaseStatus {
            active: vec![],
            stale: vec![],
            ambiguous: vec![],
        }
    };
    if !leases.active.is_empty() || !leases.ambiguous.is_empty() {
        return Err(files::invalid_data(
            "Herdr is still active. Close all managed sessions before uninstalling.",
        ));
    }
    if process_paths()?.iter().any(|(pid, path)| {
        quiet.is_none_or(|quiet| *pid != quiet.process_id)
            && files::path_within(path, install_root).unwrap_or(false)
    }) {
        return Err(files::invalid_data(
            "a process from the managed Herdr install tree is still active",
        ));
    }
    let (preserved, warning) =
        skills::remove_skill_copies_best_effort(agent_root, claude_roots, known, skill_disposition);
    let state = install_root.join("state");
    let marker = state.join(UNINSTALL_PENDING_MARKER);
    if !files::path_exists(&marker)? {
        files::write_durable(&marker, files::UNINSTALL_MARKER)?;
    } else {
        files::assert_regular_file(&marker)?;
    }
    for name in ["bin", "runtime"] {
        stage_managed_directory_for_uninstall(install_root, name)?;
        if name == "bin" {
            inject_fault("after-bin-directory", fault, fault_prefix)?;
        }
    }
    if let Some(pending) = files::pending_launcher(&state)? {
        fs::remove_file(pending.path)?;
    }
    for name in ["active", "pending", "install.manifest", "package-manager"] {
        remove_file_if_exists(&state.join(name))?;
    }
    if files::path_exists(&state.join("leases"))? {
        files::remove_validated_directory(&state.join("leases"))?;
    }
    drop(_coordination);
    validate_uninstall_residual(install_root)?;
    Ok((preserved, warning))
}

fn stop_installed_sessions(install_root: &Path) -> io::Result<()> {
    let install = ManagedInstall::new(install_root.to_path_buf());
    let active = install.read_required_active_pointer()?;
    let runtime = install.payload_path(&active);
    files::assert_regular_file(&runtime)?;
    let lease = install.open_shared_lease(&active)?;
    let job = ChildProcessJob::new_kill_on_close()?;
    let mut command = Command::new(&runtime);
    command
        .arg(INSTALLER_STOP_SESSIONS_COMMAND)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    lease.configure_payload_child(&mut command);
    let mut child = command.spawn().map_err(|err| {
        files::contextual(
            err,
            installer_stop_retry_message(format!(
                "failed to start installed Herdr session shutdown command {}",
                runtime.display()
            )),
        )
    })?;
    if let Err(err) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(files::contextual(
            err,
            installer_stop_retry_message(
                "failed to contain installed Herdr session shutdown command",
            ),
        ));
    }

    let deadline = Instant::now() + INSTALLER_STOP_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(files::invalid_data(installer_stop_retry_message(format!(
                    "installed Herdr session shutdown command exited with {status}",
                ))))
            };
        }
        if Instant::now() >= deadline {
            let cleanup = job.terminate().and_then(|_| {
                job.wait_for_empty(
                    &mut child,
                    INSTALLER_STOP_CLEANUP_TIMEOUT,
                    "installed Herdr session shutdown process tree did not terminate within 5 seconds",
                )
            });
            return match cleanup {
                Ok(()) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    installer_stop_retry_message(
                        "installed Herdr session shutdown exceeded its 35 second deadline",
                    ),
                )),
                Err(err) => Err(files::contextual(
                    err,
                    installer_stop_retry_message(
                        "installed Herdr session shutdown timed out and cleanup failed",
                    ),
                )),
            };
        }
        thread::sleep(LOCK_RETRY);
    }
}

fn installer_stop_retry_message(reason: impl std::fmt::Display) -> String {
    format!("{reason}. {INSTALLER_STOP_RETRY_GUIDANCE}")
}

fn stage_managed_directory_for_uninstall(install_root: &Path, name: &str) -> io::Result<()> {
    let source = install_root.join(name);
    if !files::path_exists(&source)? {
        return Ok(());
    }
    files::assert_regular_dir(&source)?;
    let staging = new_staging("uninstall", install_root)?;
    if let Err(err) = fs::rename(&source, staging.path.join(name)) {
        cleanup_staging(&staging);
        return Err(files::contextual(
            err,
            format!("failed to stage managed {name} for uninstall"),
        ));
    }
    cleanup_staging(&staging);
    Ok(())
}

fn remove_current_root_for_convergence(install_root: &Path) -> io::Result<()> {
    if !files::path_exists(install_root)? {
        return Ok(());
    }
    files::assert_regular_dir(install_root)?;
    let _ = files::safe_tree_entries(install_root)?;
    let state = install_root.join("state");
    let state_exists = files::path_exists(&state)?;
    let coordination = if state_exists {
        files::assert_regular_dir(&state)?;
        let _ = files::pending_launcher(&state)?;
        Some(acquire_file_lock(
            &state.join("launcher.lock"),
            LOCK_TIMEOUT,
        )?)
    } else {
        None
    };
    if files::path_exists(&state.join("leases"))? {
        let leases = lease_status(&state.join("leases"))?;
        if !leases.active.is_empty() || !leases.ambiguous.is_empty() {
            return Err(files::invalid_data("Herdr is still active"));
        }
    }
    if process_paths()?
        .iter()
        .any(|(_, path)| files::path_within(path, install_root).unwrap_or(false))
    {
        return Err(files::invalid_data(
            "a process from the managed root is active",
        ));
    }
    for entry in fs::read_dir(install_root)? {
        let entry = entry?;
        if entry.file_name() != OsStr::new("state") {
            remove_convergence_entry(&entry.path())?;
        }
    }
    if state_exists {
        for entry in fs::read_dir(&state)? {
            let entry = entry?;
            if entry.file_name() != OsStr::new("launcher.lock") {
                remove_convergence_entry(&entry.path())?;
            }
        }
    }
    drop(coordination);
    if files::path_exists(&state)? {
        remove_file_if_exists(&state.join("launcher.lock"))?;
        files::remove_validated_directory(&state)?;
    }
    if files::path_exists(install_root)? {
        files::remove_validated_directory(install_root)?;
    }
    Ok(())
}

fn remove_convergence_entry(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if files::is_reparse(&metadata) {
        return Err(files::invalid_data(format!(
            "refusing a reparse point during current-root convergence: {}",
            path.display()
        )));
    }
    if metadata.is_dir() {
        files::remove_validated_directory(path)
    } else if metadata.is_file() {
        files::assert_regular_file(path)?;
        fs::remove_file(path)
    } else {
        Err(files::invalid_data(format!(
            "unrecognized current-root entry during convergence: {}",
            path.display()
        )))
    }
}

fn validate_managed_root(install_root: &Path, allow_missing_helper: bool) -> io::Result<()> {
    files::assert_regular_dir(install_root)?;
    assert_exact_root_names(install_root)?;
    let state = install_root.join("state");
    files::assert_regular_dir(&state)?;
    let allowed = [
        "active",
        "pending",
        "leases",
        "launcher.lock",
        files::NATIVE_HELPER_NAME,
        "install.manifest",
        "package-manager",
        "path-add.pending",
    ];
    for entry in fs::read_dir(&state)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        let pending = name_text.starts_with(PENDING_LAUNCHER_PREFIX)
            && name_text.ends_with(PENDING_LAUNCHER_SUFFIX);
        if (!allowed.iter().any(|allowed| name == OsStr::new(allowed)) && !pending)
            || entry.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(files::invalid_data(format!(
                "unrecognized managed state entry: {}",
                entry.path().display()
            )));
        }
    }
    if files::path_exists(&state.join(UNINSTALL_PENDING_MARKER))? {
        return Err(files::invalid_data("managed uninstall is incomplete"));
    }
    for path in [
        state.join("active"),
        state.join("launcher.lock"),
        state.join("install.manifest"),
    ] {
        files::assert_regular_file(&path)?;
    }
    files::assert_regular_dir(&state.join("leases"))?;
    let native = files::path_exists(&state.join(files::NATIVE_HELPER_NAME))?;
    if !(native || allow_missing_helper) {
        return Err(files::invalid_data(
            "managed root lacks native installer helper",
        ));
    }
    if native {
        files::assert_regular_file(&state.join(files::NATIVE_HELPER_NAME))?;
    }
    validate_leases_dir(&state.join("leases"))?;
    validate_package_manager_marker(&state.join("package-manager"))?;
    let _ = registry::path_add_pending_value_created(&state.join("path-add.pending"))?;
    let manifest = files::read_install_manifest(&state.join("install.manifest"))?;
    validate_managed_bin(&install_root.join("bin"), &manifest.bootstrap_sha256)?;
    let _ = files::pending_launcher(&state)?;
    let runtime_root = install_root.join("runtime");
    files::assert_regular_dir(&runtime_root)?;
    for entry in fs::read_dir(&runtime_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let build = BuildId::parse(
            name.to_str()
                .ok_or_else(|| files::invalid_data("runtime name is not UTF-8"))?,
        )?;
        files::validate_runtime_directory(&entry.path(), &build)?;
    }
    let install = ManagedInstall::new(install_root.to_path_buf());
    let active = install.read_required_active_pointer()?;
    files::validate_runtime_directory(&install.build_dir(&active), &active)?;
    if let Some(pending) = install.read_pointer("pending")? {
        files::validate_runtime_directory(&install.build_dir(&pending), &pending)?;
    }
    Ok(())
}

fn assert_exact_root_names(install_root: &Path) -> io::Result<()> {
    let allowed = ["bin", "runtime", "state", "uninstall.exe"];
    for entry in fs::read_dir(install_root)? {
        let entry = entry?;
        if !allowed
            .iter()
            .any(|allowed| entry.file_name() == OsStr::new(allowed))
            || entry.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(files::invalid_data(format!(
                "unrecognized managed root entry: {}",
                entry.path().display()
            )));
        }
    }
    for directory in ["bin", "runtime", "state"] {
        files::assert_regular_dir(&install_root.join(directory))?;
    }
    if files::path_exists(&install_root.join("uninstall.exe"))? {
        files::assert_regular_file(&install_root.join("uninstall.exe"))?;
    }
    Ok(())
}

fn validate_managed_bin(bin: &Path, expected_hash: &str) -> io::Result<()> {
    files::assert_regular_dir(bin)?;
    let names = files::sorted_names(bin)?;
    if names
        != [
            OsString::from("herdr.exe"),
            OsString::from("managed-install-v1"),
        ]
    {
        return Err(files::invalid_data("managed bin has unrecognized layout"));
    }
    let launcher = bin.join("herdr.exe");
    files::assert_regular_file(&launcher)?;
    if files::sha256(&launcher)? != expected_hash {
        return Err(files::invalid_data(
            "managed launcher hash differs from install manifest",
        ));
    }
    let sentinel = bin.join("managed-install-v1");
    files::assert_regular_dir(&sentinel)?;
    if files::sorted_names(&sentinel)? != [OsString::from("marker")]
        || fs::read(sentinel.join("marker"))? != files::MANAGED_BIN_MARKER
    {
        return Err(files::invalid_data("managed bin marker is invalid"));
    }
    Ok(())
}

fn validate_uninstall_retry_root(install_root: &Path) -> io::Result<()> {
    files::assert_regular_dir(install_root)?;
    for entry in fs::read_dir(install_root)? {
        let entry = entry?;
        let name = entry.file_name();
        match name.to_str() {
            Some("bin") | Some("runtime") | Some("state") => {
                files::assert_regular_dir(&entry.path())?
            }
            Some("uninstall.exe") => files::assert_regular_file(&entry.path())?,
            _ => {
                return Err(files::invalid_data(
                    "uninstall retry root contains unexpected content",
                ))
            }
        }
    }
    let state = install_root.join("state");
    files::assert_regular_dir(&state)?;
    let allowed_files = [
        "active",
        "pending",
        "launcher.lock",
        files::NATIVE_HELPER_NAME,
        "install.manifest",
        "package-manager",
        "path-add.pending",
        UNINSTALL_PENDING_MARKER,
    ];
    for entry in fs::read_dir(&state)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == OsStr::new("leases") {
            files::assert_regular_dir(&entry.path())?;
            continue;
        }
        let name_text = name.to_string_lossy();
        let pending_launcher = name_text.starts_with(PENDING_LAUNCHER_PREFIX)
            && name_text.ends_with(PENDING_LAUNCHER_SUFFIX);
        if !allowed_files
            .iter()
            .any(|allowed| name == OsStr::new(allowed))
            && !pending_launcher
        {
            return Err(files::invalid_data(
                "uninstall retry state contains unexpected content",
            ));
        }
        files::assert_regular_file(&entry.path())?;
    }
    for required in [
        files::NATIVE_HELPER_NAME,
        "launcher.lock",
        UNINSTALL_PENDING_MARKER,
    ] {
        files::assert_regular_file(&state.join(required))?;
    }
    if files::path_exists(&state.join("leases"))? {
        validate_leases_dir(&state.join("leases"))?;
    }
    validate_package_manager_marker(&state.join("package-manager"))?;
    let _ = registry::path_add_pending_value_created(&state.join("path-add.pending"))?;
    if files::path_exists(&state.join("install.manifest"))? {
        let _ = files::read_install_manifest(&state.join("install.manifest"))?;
    }
    let install = ManagedInstall::new(install_root.to_path_buf());
    for pointer in ["active", "pending"] {
        if files::path_exists(&state.join(pointer))? {
            let _ = install.read_pointer(pointer)?;
        }
    }
    let _ = files::pending_launcher(&state)?;
    if files::path_exists(&install_root.join("bin"))? {
        let manifest = files::read_install_manifest(&state.join("install.manifest"))?;
        validate_managed_bin(&install_root.join("bin"), &manifest.bootstrap_sha256)?;
    }
    let runtime = install_root.join("runtime");
    if files::path_exists(&runtime)? {
        files::assert_regular_dir(&runtime)?;
        for entry in fs::read_dir(runtime)? {
            let entry = entry?;
            let name = entry.file_name();
            let build = BuildId::parse(
                name.to_str()
                    .ok_or_else(|| files::invalid_data("runtime name is not UTF-8"))?,
            )?;
            files::validate_runtime_directory(&entry.path(), &build)?;
        }
    }
    Ok(())
}

fn validate_uninstall_residual(install_root: &Path) -> io::Result<()> {
    files::assert_regular_dir(install_root)?;
    for entry in fs::read_dir(install_root)? {
        let entry = entry?;
        if !["state", "uninstall.exe"]
            .iter()
            .any(|allowed| entry.file_name() == OsStr::new(allowed))
        {
            return Err(files::invalid_data(
                "uninstall residual contains unexpected root state",
            ));
        }
    }
    let state = install_root.join("state");
    files::assert_regular_dir(&state)?;
    let allowed = [
        files::NATIVE_HELPER_NAME,
        "launcher.lock",
        "path-add.pending",
        UNINSTALL_PENDING_MARKER,
    ];
    for entry in fs::read_dir(&state)? {
        let entry = entry?;
        if !allowed
            .iter()
            .any(|allowed| entry.file_name() == OsStr::new(allowed))
        {
            return Err(files::invalid_data(
                "uninstall residual contains unexpected state",
            ));
        }
        files::assert_regular_file(&entry.path())?;
    }
    for required in [
        files::NATIVE_HELPER_NAME,
        "launcher.lock",
        UNINSTALL_PENDING_MARKER,
    ] {
        files::assert_regular_file(&state.join(required))?;
    }
    let _ = registry::path_add_pending_value_created(&state.join("path-add.pending"))?;
    Ok(())
}

fn validate_uninstall_cleanup_root(install_root: &Path) -> io::Result<()> {
    if !files::path_exists(install_root)? {
        return Ok(());
    }
    files::assert_regular_dir(install_root)?;
    for entry in fs::read_dir(install_root)? {
        let entry = entry?;
        match entry.file_name().to_str() {
            Some("state") => files::assert_regular_dir(&entry.path())?,
            Some("uninstall.exe") => files::assert_regular_file(&entry.path())?,
            _ => {
                return Err(files::invalid_data(
                    "uninstall cleanup root contains unexpected content",
                ))
            }
        }
    }
    let state = install_root.join("state");
    if !files::path_exists(&state)? {
        return Ok(());
    }
    files::assert_regular_dir(&state)?;
    let allowed = [
        files::NATIVE_HELPER_NAME,
        "launcher.lock",
        "path-add.pending",
        UNINSTALL_PENDING_MARKER,
    ];
    for entry in fs::read_dir(&state)? {
        let entry = entry?;
        if !allowed
            .iter()
            .any(|allowed| entry.file_name() == OsStr::new(allowed))
        {
            return Err(files::invalid_data(
                "uninstall cleanup state contains unexpected content",
            ));
        }
        files::assert_regular_file(&entry.path())?;
    }
    let _ = registry::path_add_pending_value_created(&state.join("path-add.pending"))?;
    Ok(())
}

fn classify_root(install_root: &Path, allow_missing_helper: bool) -> io::Result<RootKind> {
    if !files::path_exists(install_root)? {
        return Ok(RootKind::New);
    }
    if files::path_exists(&install_root.join("state").join(UNINSTALL_PENDING_MARKER))? {
        validate_uninstall_retry_root(install_root)?;
        return Ok(RootKind::UninstallRetry);
    }
    if validate_managed_root(install_root, false).is_ok() {
        return Ok(RootKind::ManagedNative);
    }
    if allow_missing_helper
        && !files::path_exists(&install_root.join("state").join(files::NATIVE_HELPER_NAME))?
        && validate_managed_root(install_root, true).is_ok()
    {
        return Ok(RootKind::ManagedNative);
    }
    if validate_uninstall_cleanup_root(install_root).is_ok() {
        return Ok(RootKind::UninstallResidual);
    }
    Err(files::incompatible_installation())
}

fn unsupported_launcher_hop(install_root: &Path) -> io::Result<bool> {
    let runtime = install_root.join("runtime");
    if !files::path_exists(&runtime)? {
        return Ok(false);
    }
    files::assert_regular_dir(&runtime)?;
    for entry in fs::read_dir(runtime)? {
        let entry = entry?;
        if entry.metadata()?.is_dir()
            && files::path_exists(&entry.path().join("herdr-launcher.exe"))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn acquire_lifecycle_lock() -> io::Result<LifecycleMutex> {
    acquire_named_lifecycle_mutex(LIFECYCLE_MUTEX_NAME).map(|(guard, _)| guard)
}

fn acquire_named_lifecycle_mutex(name: &str) -> io::Result<(LifecycleMutex, bool)> {
    if name.is_empty() || name.encode_utf16().any(|value| value == 0) {
        return Err(files::invalid_data("lifecycle mutex name is invalid"));
    }
    let wide = OsStr::new(name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: the name is a valid NUL-terminated UTF-16 buffer and the default
    // security descriptor remains owned by the current process token.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    if handle.is_null() {
        return Err(files::contextual(
            io::Error::last_os_error(),
            "failed to create or open the Herdr installer lifecycle mutex",
        ));
    }
    // SAFETY: handle is a valid mutex handle. A zero timeout performs one
    // ownership attempt and never polls object existence or stale state.
    let wait = unsafe { WaitForSingleObject(handle, 0) };
    match wait {
        WAIT_OBJECT_0 | WAIT_ABANDONED => Ok((
            LifecycleMutex {
                handle,
                _same_thread: PhantomData,
            },
            wait == WAIT_ABANDONED,
        )),
        WAIT_TIMEOUT => {
            // SAFETY: ownership was not acquired, and this process owns the handle.
            unsafe { CloseHandle(handle) };
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "another Herdr setup, update, or uninstall is already running",
            ))
        }
        WAIT_FAILED => {
            let err = io::Error::last_os_error();
            // SAFETY: this process owns the handle even though the wait failed.
            unsafe { CloseHandle(handle) };
            Err(files::contextual(
                err,
                "failed to acquire the Herdr installer lifecycle mutex",
            ))
        }
        value => {
            // SAFETY: this process owns the handle for the unexpected wait result.
            unsafe { CloseHandle(handle) };
            Err(files::invalid_data(format!(
                "unexpected lifecycle mutex wait result 0x{value:08x}"
            )))
        }
    }
}

fn acquire_file_lock(path: &Path, timeout: Duration) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        if !files::path_exists(parent)? {
            fs::create_dir_all(parent)?;
        }
        files::assert_regular_dir(parent)?;
    }
    if files::path_exists(path)? {
        files::assert_regular_file(path)?;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        match options.open(path) {
            Ok(file) => {
                files::assert_regular_file(path)?;
                return Ok(file);
            }
            Err(_err) if Instant::now() < deadline => thread::sleep(LOCK_RETRY),
            Err(err) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out acquiring Herdr lock {}: {err}", path.display()),
                ))
            }
        }
    }
}

fn acquire_coordination(
    install: &ManagedInstall,
    timeout: Duration,
) -> io::Result<CoordinationLease> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(lease) = install.try_open_coordination_lease()? {
            return Ok(lease);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out acquiring coordination lock {}",
                    install.coordination_lock_path().display()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn new_staging(kind: &'static str, install_root: &Path) -> io::Result<Staging> {
    let parent = install_root
        .parent()
        .ok_or_else(|| files::invalid_data("install root has no parent"))?;
    if !files::path_exists(parent)? {
        fs::create_dir_all(parent)?;
    }
    files::assert_regular_dir(parent)?;
    let leaf = install_root
        .file_name()
        .ok_or_else(|| files::invalid_data("install root has no leaf"))?
        .to_string_lossy();
    let path = parent.join(format!("{leaf}.installer-{kind}.{}", files::unique_hex()));
    fs::create_dir(&path)?;
    Ok(Staging {
        path,
        kind,
        install_root: install_root.to_path_buf(),
    })
}

fn validate_staging(staging: &Staging) -> io::Result<()> {
    let parent = staging
        .install_root
        .parent()
        .ok_or_else(|| files::invalid_data("install root has no parent"))?;
    if staging.path.parent() != Some(parent) {
        return Err(files::invalid_data(
            "staging directory is not beside install root",
        ));
    }
    let leaf = staging
        .install_root
        .file_name()
        .ok_or_else(|| files::invalid_data("install root has no leaf"))?
        .to_string_lossy();
    let prefix = format!("{leaf}.installer-{}.", staging.kind);
    let name = staging
        .path
        .file_name()
        .ok_or_else(|| files::invalid_data("staging directory has no leaf"))?
        .to_string_lossy();
    let suffix = name
        .strip_prefix(&prefix)
        .ok_or_else(|| files::invalid_data("unrecognized staging name"))?;
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(files::invalid_data("unrecognized staging identifier"));
    }
    files::assert_regular_dir(&staging.path)?;
    let _ = files::safe_tree_entries(&staging.path)?;
    Ok(())
}

fn cleanup_staging(staging: &Staging) {
    let result =
        validate_staging(staging).and_then(|_| files::remove_validated_directory(&staging.path));
    if let Err(err) = result {
        let _ = writeln!(
            io::stderr().lock(),
            "Warning: Private installer staging was preserved and will not change the requested result: {}. {err}",
            staging.path.display()
        );
    }
}

fn remove_stale_staging(install_root: &Path) {
    let Some(parent) = install_root.parent() else {
        return;
    };
    let Some(leaf) = install_root.file_name().and_then(OsStr::to_str) else {
        return;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        for kind in ["fresh", "update", "uninstall"] {
            let prefix = format!("{leaf}.installer-{kind}.");
            if name.strip_prefix(&prefix).is_some_and(|suffix| {
                suffix.len() == 32
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            }) {
                cleanup_staging(&Staging {
                    path: entry.path(),
                    kind,
                    install_root: install_root.to_path_buf(),
                });
            }
        }
    }
}

fn validate_leases_dir(path: &Path) -> io::Result<()> {
    files::assert_regular_dir(path)?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(value) = name.to_str().and_then(|name| name.strip_suffix(".lease")) else {
            return Err(files::invalid_data("unrecognized lease entry"));
        };
        BuildId::parse(value)?;
        files::assert_regular_file(&entry.path())?;
    }
    Ok(())
}

fn lease_status(path: &Path) -> io::Result<LeaseStatus> {
    validate_leases_dir(path)?;
    let mut output = LeaseStatus {
        active: vec![],
        stale: vec![],
        ambiguous: vec![],
    };
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        match options.open(&path) {
            Ok(_) => output.stale.push(path),
            Err(err) if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                output.ambiguous.push(path)
            }
            Err(_) => output.active.push(path),
        }
    }
    Ok(output)
}

fn remove_stale_leases(status: &LeaseStatus) -> io::Result<()> {
    if !status.active.is_empty() || !status.ambiguous.is_empty() {
        return Err(files::invalid_data(
            "cannot remove active or ambiguous leases",
        ));
    }
    for path in &status.stale {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn process_paths() -> io::Result<Vec<(u32, PathBuf)>> {
    // SAFETY: snapshot has no pointer arguments and returns an owned handle.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let _snapshot = ProcessHandle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut output = Vec::new();
    // SAFETY: entry is initialized with the required structure size.
    let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while ok {
        if let Some(path) = process_path(entry.th32ProcessID) {
            output.push((entry.th32ProcessID, path));
        }
        // SAFETY: entry remains valid for the next snapshot record.
        ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) && err.raw_os_error() != Some(18) {
        return Err(err);
    }
    Ok(output)
}

fn process_path(pid: u32) -> Option<PathBuf> {
    // SAFETY: no inheritance and only query access are requested.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let _handle = ProcessHandle(handle);
    let mut buffer = vec![0u16; 32768];
    let mut size = buffer.len() as u32;
    // SAFETY: buffer and size are valid writable arguments.
    if unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size)
    } == 0
    {
        return None;
    }
    buffer.truncate(size as usize);
    Some(PathBuf::from(OsString::from_wide(&buffer)))
}

fn validate_package_manager_marker(path: &Path) -> io::Result<bool> {
    if !files::path_exists(path)? {
        return Ok(false);
    }
    files::assert_regular_file(path)?;
    if fs::read(path)? != WINGET_PACKAGE_MANAGER_RECORD {
        return Err(files::invalid_data("package-manager marker is invalid"));
    }
    Ok(true)
}

fn current_path_ownership(
    install_root: &Path,
    pending_marker: &Path,
) -> io::Result<registry::PathOwnership> {
    let arp = registry::arp_path_ownership(install_root)?;
    let pending_value_created = registry::path_add_pending_value_created(pending_marker)?;
    let pending_owned = pending_value_created.is_some()
        && registry::exact_user_path_entry_exists(&install_root.join("bin"))?;
    if !pending_owned {
        return Ok(arp.ownership);
    }
    let pending = registry::PathOwnership {
        owned: true,
        value_created: pending_value_created.unwrap_or(false),
    };
    if arp.ownership.owned && arp.value_created_present && arp.ownership != pending {
        return Err(files::invalid_data(
            "PATH ownership intent differs from Installed Apps ownership",
        ));
    }
    Ok(if arp.ownership.owned && arp.value_created_present {
        arp.ownership
    } else {
        pending
    })
}

fn set_package_manager_marker(state: &Path, manager: InstallManager) -> io::Result<()> {
    let path = state.join("package-manager");
    if files::path_exists(&path)? {
        validate_package_manager_marker(&path)?;
    } else if manager == InstallManager::WinGet {
        files::write_durable(&path, WINGET_PACKAGE_MANAGER_RECORD)?;
    }
    Ok(())
}

fn fault_marker(point: &str, prefix: &str) -> io::Result<PathBuf> {
    if prefix.is_empty()
        || prefix.len() > 32
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(files::invalid_data("invalid uninstall fault marker prefix"));
    }
    Ok(std::env::temp_dir().join(format!("{prefix}-uninstall-fault-{point}.once")))
}

fn inject_fault(point: &str, fault: Option<&str>, prefix: &str) -> io::Result<()> {
    let Some(selected) = fault else {
        return Ok(());
    };
    let terminate = selected.strip_prefix("terminate-") == Some(point);
    if selected != point && !terminate {
        return Ok(());
    }
    let marker = fault_marker(selected, prefix)?;
    if files::path_exists(&marker)? {
        files::assert_regular_file(&marker)?;
        return Ok(());
    }
    files::write_durable(&marker, &[])?;
    if terminate {
        std::process::exit(86);
    }
    Err(files::invalid_data(format!(
        "injected installer lifecycle fault after {point}"
    )))
}

fn remove_fault_marker(fault: Option<&str>, prefix: &str) -> io::Result<()> {
    if let Some(fault) = fault {
        remove_file_if_exists(&fault_marker(fault, prefix)?)?;
    }
    Ok(())
}

fn remove_uninstall_residual(
    install_root: &Path,
    fault: Option<&str>,
    prefix: &str,
    quiet: Option<&QuietSession>,
) -> io::Result<()> {
    if !files::path_exists(install_root)? {
        return Ok(());
    }
    validate_uninstall_cleanup_root(install_root)?;
    (|| {
        let state = install_root.join("state");
        if files::path_exists(&state)? {
            remove_file_if_exists(&state.join("path-add.pending"))?;
            validate_uninstall_cleanup_root(install_root)?;
            remove_file_if_exists(&state.join(UNINSTALL_PENDING_MARKER))?;
            remove_file_if_exists(&state.join("launcher.lock"))?;
            let helper = state.join(files::NATIVE_HELPER_NAME);
            if let Some(quiet) = quiet {
                files::assert_regular_file(&helper)?;
                if quiet.process_id == std::process::id() {
                    return Err(files::invalid_data(
                        "quiet-uninstall runner and embedded helper process identities collided",
                    ));
                }
                if files::path_exists(&quiet.moved_helper_path)? {
                    return Err(files::invalid_data(
                        "quiet-uninstall helper handoff path appeared during cleanup",
                    ));
                }
                fs::rename(&helper, &quiet.moved_helper_path)?;
            } else {
                remove_file_if_exists(&helper)?;
            }
            inject_fault("after-installer-helper", fault, prefix)?;
            validate_uninstall_cleanup_root(install_root)?;
            fs::remove_dir(&state)?;
            inject_fault("after-state-directory", fault, prefix)?;
        }
        remove_terminal_uninstall_files(install_root, fault, prefix)
    })()
}

fn remove_terminal_uninstall_files(
    install_root: &Path,
    fault: Option<&str>,
    prefix: &str,
) -> io::Result<()> {
    validate_uninstall_cleanup_root(install_root)?;
    let mut retry = None;
    let uninstaller = install_root.join("uninstall.exe");
    if files::path_exists(&uninstaller)? {
        files::assert_regular_file(&uninstaller)?;
        retry = Some((
            uninstaller.clone(),
            fs::read(&uninstaller)?,
            files::sha256(&uninstaller)?,
        ));
    }
    let result = (|| {
        remove_file_if_exists(&uninstaller)?;
        inject_fault("after-uninstaller", fault, prefix)?;
        fs::remove_dir(install_root)?;
        Ok(())
    })();
    if let Err(original) = result {
        if files::path_exists(install_root)? {
            files::assert_regular_dir(install_root)?;
            if let Some((path, bytes, hash)) = retry {
                if files::path_exists(&path)? {
                    files::assert_regular_file(&path)?;
                    if files::sha256(&path)? != hash {
                        return Err(files::invalid_data(format!(
                            "terminal uninstall failed ({original}) and retry state changed: {}",
                            path.display()
                        )));
                    }
                } else {
                    files::write_durable(&path, &bytes)?;
                }
                if files::sha256(&path)? != hash {
                    return Err(files::invalid_data(format!(
                        "restored uninstall retry state differs from original: {}",
                        path.display()
                    )));
                }
            }
        }
        return Err(original);
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    if files::path_exists(path)? {
        files::assert_regular_file(path)?;
        fs::remove_file(path)?;
    }
    Ok(())
}

fn delete_running_executable(path: &Path) -> io::Result<()> {
    files::assert_regular_file(path)?;
    let wide_path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let open = || {
        // SAFETY: wide_path is NUL-terminated and remains live for this synchronous call.
        let handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                DELETE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(ProcessHandle(handle))
        }
    };

    let stream = format!(":herdr-uninstall-{}", files::unique_hex())
        .encode_utf16()
        .collect::<Vec<_>>();
    let stream_bytes = stream
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| files::invalid_data("quiet-uninstall self-delete stream is too long"))?;
    let rename_size = offset_of!(FILE_RENAME_INFO, FileName)
        .checked_add(stream_bytes)
        .ok_or_else(|| files::invalid_data("quiet-uninstall rename buffer is too large"))?;
    let rename_size_u32 = u32::try_from(rename_size)
        .map_err(|_| files::invalid_data("quiet-uninstall rename buffer is too large"))?;
    let stream_bytes_u32 = u32::try_from(stream_bytes)
        .map_err(|_| files::invalid_data("quiet-uninstall self-delete stream is too long"))?;
    let rename_words = rename_size.div_ceil(size_of::<usize>());
    let mut rename = vec![0usize; rename_words];
    let rename_info = rename.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: rename is sized through FILE_RENAME_INFO::FileName plus the exact
    // UTF-16 byte length, and these writes stay within that allocation.
    unsafe {
        (*rename_info).FileNameLength = stream_bytes_u32;
        std::ptr::copy_nonoverlapping(
            stream.as_ptr(),
            (*rename_info).FileName.as_mut_ptr(),
            stream.len(),
        );
    }
    let first = open().map_err(|err| {
        files::contextual(
            err,
            "failed to open the quiet-uninstall runner image for renaming",
        )
    })?;
    // SAFETY: first owns a valid DELETE handle and rename contains the exact
    // FILE_RENAME_INFO buffer described above.
    if unsafe {
        SetFileInformationByHandle(
            first.0,
            FileRenameInfo,
            rename.as_ptr().cast(),
            rename_size_u32,
        )
    } == 0
    {
        return Err(files::contextual(
            io::Error::last_os_error(),
            "failed to rename the quiet-uninstall runner image stream",
        ));
    }
    drop(first);

    let second = open().map_err(|err| {
        files::contextual(
            err,
            "failed to reopen the quiet-uninstall runner image for deletion",
        )
    })?;
    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: second owns a valid DELETE handle and disposition is valid for
    // FileDispositionInfoEx for the duration of this synchronous call.
    if unsafe {
        SetFileInformationByHandle(
            second.0,
            FileDispositionInfoEx,
            (&disposition as *const FILE_DISPOSITION_INFO_EX).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } == 0
    {
        return Err(files::contextual(
            io::Error::last_os_error(),
            "failed to mark the quiet-uninstall runner image for deletion",
        ));
    }
    drop(second);
    if files::path_exists(path)? {
        return Err(files::invalid_data(
            "quiet-uninstall runner image remained after terminal deletion",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn test_mutex_name(label: &str) -> String {
        format!(
            r"Local\HerdrWinInstallerLifecycleTest-{label}-{}",
            files::unique_hex()
        )
    }

    fn open_unowned_mutex(name: &str) -> HANDLE {
        let wide = OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // SAFETY: the test supplies a valid NUL-terminated UTF-16 name.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
        assert!(!handle.is_null());
        handle
    }

    #[test]
    fn fault_marker_prefix_is_narrow() {
        assert!(fault_marker("after-uninstaller", "herdr-test").is_ok());
        assert!(fault_marker("after-uninstaller", "../escape").is_err());
    }

    #[test]
    fn quiet_uninstall_token_is_exact_lowercase_hex() {
        assert!(validate_quiet_token("0123456789abcdef0123456789abcdef").is_ok());
        for invalid in [
            "0123456789abcdef",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert!(validate_quiet_token(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn terminal_deletion_child_waits_when_selected() {
        let Some(ready) = std::env::var_os("HERDR_TERMINAL_DELETE_CHILD_READY") else {
            return;
        };
        fs::write(ready, b"ready").unwrap();
        thread::sleep(Duration::from_secs(10));
    }

    #[test]
    fn terminal_deletion_removes_a_running_executable_copy() {
        let root = std::env::temp_dir().join(format!(
            "herdr-quiet-runner-delete-test-{}",
            files::unique_hex()
        ));
        fs::create_dir(&root).unwrap();
        let copy = root.join("runner.exe");
        let ready = root.join("ready");
        fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        let mut child = Command::new(&copy)
            .args([
                "--exact",
                "installer_helper::installer_helper_lifecycle::tests::terminal_deletion_child_waits_when_selected",
                "--nocapture",
            ])
            .env("HERDR_TERMINAL_DELETE_CHILD_READY", &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let result = (|| -> io::Result<()> {
            let ready_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if ready.exists() {
                    break;
                }
                if let Some(status) = child.try_wait()? {
                    return Err(files::invalid_data(format!(
                        "terminal-deletion test child exited before readiness: {status}"
                    )));
                }
                if Instant::now() >= ready_deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "terminal-deletion test child did not become ready",
                    ));
                }
                thread::sleep(LOCK_RETRY);
            }
            delete_running_executable(&copy)?;
            if copy.exists() {
                return Err(files::invalid_data(
                    "running executable copy remained after terminal deletion",
                ));
            }
            Ok(())
        })();
        let _ = child.kill();
        let reap_deadline = Instant::now() + Duration::from_secs(5);
        let reaped = loop {
            match child.try_wait() {
                Ok(Some(_)) => break Ok(()),
                Ok(None) if Instant::now() < reap_deadline => thread::sleep(LOCK_RETRY),
                Ok(None) => {
                    break Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "terminal-deletion test child did not exit after termination",
                    ))
                }
                Err(err) => break Err(err),
            }
        };
        let _ = remove_file_if_exists(&ready);
        let _ = remove_file_if_exists(&copy);
        fs::remove_dir(root).unwrap();
        result.unwrap();
        reaped.unwrap();
    }

    #[test]
    fn lifecycle_mutex_acquires_an_existing_unowned_object() {
        let name = test_mutex_name("unowned");
        let existing = open_unowned_mutex(&name);
        let (guard, abandoned) = acquire_named_lifecycle_mutex(&name).unwrap();
        assert!(!abandoned);
        drop(guard);
        // SAFETY: this test owns the original non-null handle.
        unsafe { CloseHandle(existing) };
    }

    #[test]
    fn lifecycle_mutex_releases_for_the_next_owner() {
        let name = test_mutex_name("release");
        let (first, first_abandoned) = acquire_named_lifecycle_mutex(&name).unwrap();
        assert!(!first_abandoned);
        drop(first);
        let (second, second_abandoned) = acquire_named_lifecycle_mutex(&name).unwrap();
        assert!(!second_abandoned);
        drop(second);
    }

    #[test]
    fn lifecycle_mutex_name_is_stable_and_unversioned() {
        assert_eq!(LIFECYCLE_MUTEX_NAME, r"Local\HerdrWinInstallerLifecycle");
    }

    #[test]
    fn lifecycle_mutex_rejects_a_live_owner() {
        let name = test_mutex_name("live");
        let worker_name = name.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder = thread::spawn(move || {
            let (guard, abandoned) = acquire_named_lifecycle_mutex(&worker_name).unwrap();
            assert!(!abandoned);
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            drop(guard);
        });
        let ready = ready_rx.recv_timeout(Duration::from_secs(5));
        if ready.is_err() {
            let _ = release_tx.send(());
            let _ = holder.join();
            panic!("lifecycle mutex holder did not become ready");
        }
        let err = match acquire_named_lifecycle_mutex(&name) {
            Ok(_) => panic!("acquired lifecycle mutex while another thread owned it"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        release_tx.send(()).unwrap();
        holder.join().unwrap();
    }

    #[test]
    fn lifecycle_mutex_recovers_a_hard_terminated_owner() {
        let name = test_mutex_name("abandoned");
        let worker_name = name.clone();
        let original_handle = thread::spawn(move || {
            let (guard, abandoned) = acquire_named_lifecycle_mutex(&worker_name).unwrap();
            assert!(!abandoned);
            let handle = guard.handle as usize;
            std::mem::forget(guard);
            handle
        })
        .join()
        .unwrap();
        let (recovered, abandoned) = acquire_named_lifecycle_mutex(&name).unwrap();
        assert!(abandoned);
        drop(recovered);
        // SAFETY: the worker deliberately transferred its leaked handle value.
        unsafe { CloseHandle(original_handle as HANDLE) };
    }
}
