use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, Write},
    os::windows::process::CommandExt as _,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::System::{
    Console::{
        GetConsoleCP, SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT, PHANDLER_ROUTINE,
    },
    Threading::{CREATE_NO_WINDOW, DETACHED_PROCESS},
};

use crate::{
    managed_install::{
        BuildId, ManagedInstall, LAUNCHER_BUILD_ID_QUERY_ARG, PENDING_LAUNCHER_PREFIX,
        PENDING_LAUNCHER_SUFFIX, UNINSTALL_PENDING_MARKER,
    },
    windows_managed_install::{CoordinationLease, Runtime, SharedLease, MANAGED_LEASE_HANDLE_ENV},
};

const PENDING_POINTER: &str = "pending";
const COORDINATION_TIMEOUT: Duration = Duration::from_secs(5);
const COORDINATION_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const DEVELOPMENT_BUILD_ID: &str = "development";

pub(crate) fn run() -> io::Result<i32> {
    let current = std::env::current_exe().map_err(|err| {
        contextual(
            err,
            "failed to determine the managed Herdr launcher path".to_string(),
        )
    })?;
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if let Some(code) = maybe_run_build_id_query(&current, &args)? {
        return Ok(code);
    }
    let install = resolve_install(&current)?;
    run_launcher(&install, &args)
}

fn maybe_run_build_id_query(current: &Path, args: &[OsString]) -> io::Result<Option<i32>> {
    if !allows_build_id_query(current) || args != [OsString::from(LAUNCHER_BUILD_ID_QUERY_ARG)] {
        return Ok(None);
    }
    std::env::remove_var(MANAGED_LEASE_HANDLE_ENV);
    writeln!(io::stdout().lock(), "{}", compiled_build_id_label()?)?;
    Ok(Some(0))
}

fn allows_build_id_query(current: &Path) -> bool {
    let Some(name) = current.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    if name == "herdr-launcher.exe" || name == "app-launcher.exe" {
        return true;
    }
    let Some(hash) = name
        .strip_prefix(PENDING_LAUNCHER_PREFIX)
        .and_then(|name| name.strip_suffix(PENDING_LAUNCHER_SUFFIX))
    else {
        return false;
    };
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn compiled_build_id_label() -> io::Result<&'static str> {
    match option_env!("HERDR_BUILD_ID") {
        Some(value) if !value.is_empty() && value != DEVELOPMENT_BUILD_ID => {
            BuildId::parse(value)?;
            Ok(value)
        }
        _ => Ok(DEVELOPMENT_BUILD_ID),
    }
}

fn resolve_install(current: &Path) -> io::Result<ManagedInstall> {
    if current.file_name() != Some(OsStr::new("herdr.exe")) {
        return Err(invalid_data(format!(
            "managed launcher {} is not bin/herdr.exe",
            current.display()
        )));
    }
    let bin_dir = current.parent().ok_or_else(|| {
        invalid_data(format!(
            "managed launcher {} has no bin directory",
            current.display()
        ))
    })?;
    if bin_dir.file_name() != Some(OsStr::new("bin")) {
        return Err(invalid_data(format!(
            "managed launcher {} is not the direct bin/herdr.exe command",
            current.display()
        )));
    }
    let root = bin_dir.parent().ok_or_else(|| {
        invalid_data(format!(
            "managed launcher {} has no install root",
            current.display()
        ))
    })?;
    let install = ManagedInstall::new(root.to_path_buf());
    let expected = install.validate_managed_bin()?;
    if expected != current {
        return Err(invalid_data(format!(
            "managed launcher path is {}, expected {}",
            current.display(),
            expected.display()
        )));
    }
    Ok(install)
}

fn run_launcher(install: &ManagedInstall, args: &[OsString]) -> io::Result<i32> {
    let (mut child, lease, _console_handler) = spawn_payload(install, args)?;
    let status = child.wait().map_err(|err| {
        contextual(
            err,
            "failed while waiting for managed Herdr payload".to_string(),
        )
    })?;
    drop(lease);

    match prepare_post_exit_maintenance(install, COORDINATION_TIMEOUT) {
        Ok(true) => {
            if let Err(err) = spawn_post_exit_maintenance(install) {
                let _ = writeln!(
                    io::stderr().lock(),
                    "herdr launcher: payload exited, but maintenance could not start: {err}"
                );
            }
        }
        Ok(false) => {}
        Err(err) => {
            let _ = writeln!(
                io::stderr().lock(),
                "herdr launcher: payload exited, but maintenance preparation failed: {err}"
            );
        }
    }
    child_exit_code(status)
}

fn spawn_payload(
    install: &ManagedInstall,
    args: &[OsString],
) -> io::Result<(Child, SharedLease, ConsoleCtrlHandler)> {
    let _coordination = CoordinationGate::acquire(install, COORDINATION_TIMEOUT)?;
    install.validate_managed_bin()?;
    reject_pending_uninstall(install)?;
    let runtime = select_runtime_locked(install)?;
    let lease = install.open_shared_lease(&runtime.build_id)?;
    let console_handler = ConsoleCtrlHandler::install()?;
    let mut command = payload_command(&runtime.executable, args);
    lease.configure_payload_child(&mut command);
    let child = command.spawn().map_err(|err| {
        contextual(
            err,
            format!(
                "failed to launch managed Herdr payload {}",
                runtime.executable.display()
            ),
        )
    })?;

    // `_coordination` remains live until after CreateProcess succeeds. Rust's
    // Windows Command inherits the explicitly marked lease handle; the payload
    // validates and adopts that exact file before it can create descendants.
    Ok((child, lease, console_handler))
}

fn reject_pending_uninstall(install: &ManagedInstall) -> io::Result<()> {
    let marker = install.state_dir().join(UNINSTALL_PENDING_MARKER);
    match fs::symlink_metadata(&marker) {
        Ok(_) => Err(invalid_data(format!(
            "managed Herdr uninstall is pending at {}; retry uninstall before launching Herdr",
            marker.display()
        ))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(contextual(
            err,
            format!(
                "failed to inspect managed Herdr uninstall state {}",
                marker.display()
            ),
        )),
    }
}

fn payload_command(executable: &Path, args: &[OsString]) -> Command {
    let mut command = Command::new(executable);
    // Deliberately leave environment, cwd, standard handles, console, and
    // Rust's default handle inheritance untouched. The only creation flag
    // adjustment below carries an already-detached state into the payload.
    command.args(args);
    // SAFETY: `GetConsoleCP` has no arguments and only queries whether this
    // process is attached to a console.
    let detached = unsafe { GetConsoleCP() } == 0;
    if detached {
        // A console-subsystem child created by a detached launcher would
        // otherwise allocate a fresh console. Preserve the caller's detached
        // state through the single launcher hop.
        command.creation_flags(DETACHED_PROCESS);
    }
    command
}

fn child_exit_code(status: std::process::ExitStatus) -> io::Result<i32> {
    status.code().ok_or_else(|| {
        io::Error::other("managed Herdr child exited without a Windows process exit code")
    })
}

fn prepare_post_exit_maintenance(install: &ManagedInstall, timeout: Duration) -> io::Result<bool> {
    let _coordination = CoordinationGate::acquire(install, timeout)?;
    let _ = select_runtime_locked(install)?;
    maintenance_needed_locked(install)
}

fn maintenance_needed_locked(install: &ManagedInstall) -> io::Result<bool> {
    if install.read_pointer(PENDING_POINTER)?.is_some() {
        return Ok(true);
    }
    let mut runtimes = fs::read_dir(install.runtime_dir()).map_err(|err| {
        contextual(
            err,
            format!(
                "failed to inspect managed Herdr runtimes {}",
                install.runtime_dir().display()
            ),
        )
    })?;
    if runtimes.next().transpose()?.is_some() && runtimes.next().transpose()?.is_some() {
        return Ok(true);
    }
    for entry in fs::read_dir(install.state_dir()).map_err(|err| {
        contextual(
            err,
            format!(
                "failed to inspect managed Herdr state {}",
                install.state_dir().display()
            ),
        )
    })? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(PENDING_LAUNCHER_PREFIX) && name.ends_with(PENDING_LAUNCHER_SUFFIX) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn spawn_post_exit_maintenance(install: &ManagedInstall) -> io::Result<()> {
    let helper = install.validate_installer_helper()?;
    let mut command = Command::new(&helper);
    command
        .arg("complete-maintenance")
        .arg("--install-root")
        .arg(install.root())
        .arg("--parent-process-id")
        .arg(std::process::id().to_string())
        .env_remove(MANAGED_LEASE_HANDLE_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    command.spawn().map_err(|err| {
        contextual(
            err,
            format!(
                "failed to start managed Herdr maintenance helper {}",
                helper.display()
            ),
        )
    })?;
    Ok(())
}

/// Selects one runtime while the caller holds the coordination gate.
///
/// The old-build exclusive sharing probe and active pointer replacement happen
/// before this returns. A launching caller must open the selected inheritable
/// lease and create the child before releasing the gate.
fn select_runtime_locked(install: &ManagedInstall) -> io::Result<Runtime> {
    let active_id = install.read_required_active_pointer()?;
    let active_runtime = install.validate_runtime(&active_id)?;

    let Some(pending_id) = install.read_pointer(PENDING_POINTER)? else {
        return Ok(active_runtime);
    };
    let pending_runtime = install.validate_runtime(&pending_id)?;

    let Some(_exclusive_old_build) = install.try_open_exclusive_lease(&active_id)? else {
        return Ok(active_runtime);
    };
    install.replace_active_with_pending(&pending_id)?;
    Ok(pending_runtime)
}

#[derive(Debug)]
struct CoordinationGate {
    _lease: CoordinationLease,
}

impl CoordinationGate {
    fn acquire(install: &ManagedInstall, timeout: Duration) -> io::Result<Self> {
        let path = install.coordination_lock_path();
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(lease) = install.try_open_coordination_lease()? {
                return Ok(Self { _lease: lease });
            }

            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "timed out after {} ms acquiring managed Herdr launcher coordination gate {}",
                        timeout.as_millis(),
                        path.display()
                    ),
                ));
            }
            thread::sleep(COORDINATION_RETRY_INTERVAL.min(deadline - now));
        }
    }
}

struct ConsoleCtrlHandler {
    installed: bool,
}

impl ConsoleCtrlHandler {
    fn install() -> io::Result<Self> {
        let handler: PHANDLER_ROUTINE = Some(launcher_console_handler);
        // SAFETY: the handler has the required system ABI and static lifetime.
        if unsafe { SetConsoleCtrlHandler(handler, 1) } != 0 {
            return Ok(Self { installed: true });
        }
        // A detached server chain has no console and therefore no console
        // control event to intercept. Do not make that valid mode unlaunchable.
        // SAFETY: `GetConsoleCP` has no arguments and only queries attachment.
        if unsafe { GetConsoleCP() } == 0 {
            return Ok(Self { installed: false });
        }
        Err(contextual(
            io::Error::last_os_error(),
            "failed to install transparent Herdr launcher console handler".to_string(),
        ))
    }
}

impl Drop for ConsoleCtrlHandler {
    fn drop(&mut self) {
        if !self.installed {
            return;
        }
        let handler: PHANDLER_ROUTINE = Some(launcher_console_handler);
        // SAFETY: removes the same static handler registered by `install`.
        unsafe {
            SetConsoleCtrlHandler(handler, 0);
        }
    }
}

unsafe extern "system" fn launcher_console_handler(control_type: u32) -> i32 {
    i32::from(matches!(control_type, CTRL_C_EVENT | CTRL_BREAK_EVENT))
}

fn contextual(error: io::Error, context: String) -> io::Error {
    io::Error::new(error.kind(), format!("{context}: {error}"))
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        io::{BufRead, BufReader, Read},
        net::{TcpListener, TcpStream},
        os::windows::process::CommandExt as _,
        path::PathBuf,
        process::{ExitStatus, Stdio},
        sync::{
            atomic::{AtomicU64, Ordering},
            Mutex,
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        System::{
            Console::GetConsoleWindow,
            Threading::{
                OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
                PROCESS_TERMINATE,
            },
        },
    };

    use crate::{
        managed_install::{MANAGED_BIN_MARKER, POINTER_RECORD_HEADER, RUNTIME_RECORD_HEADER},
        windows_managed_install::{
            adopt_managed_runtime_lease_platform, managed_install_command_executable_platform,
        },
    };

    use super::*;

    const ACTIVE_POINTER: &str = "active";
    const OLD_BUILD: &str = "111111111111.aaaaaaaaaaaa";
    const NEW_BUILD: &str = "222222222222.bbbbbbbbbbbb";
    const CHILD_ENV: &str = "HERDR_LAUNCHER_INHERITANCE_TEST";
    const CHILD_CWD_ENV: &str = "HERDR_LAUNCHER_CWD_TEST";
    const OWNER_ROOT_ENV: &str = "HERDR_LAUNCHER_OWNER_ROOT";
    const OWNER_ADDR_ENV: &str = "HERDR_LAUNCHER_OWNER_ADDR";
    const DETACHED_ROOT_ENV: &str = "HERDR_LAUNCHER_DETACHED_ROOT";
    const ADOPTION_PROBE_ENV: &str = "HERDR_LAUNCHER_ADOPTION_PROBE";
    const LEASE_DESCENDANT_EXE: &str = "lease-descendant.exe";
    const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
    const TEST_POLL_INTERVAL: Duration = Duration::from_millis(10);

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
    static PROCESS_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TestTree {
        root: PathBuf,
    }

    impl TestTree {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos();
            let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "herdr-launcher-{label}-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("bin/managed-install-v1")).expect("create bin sentinel");
            fs::create_dir_all(root.join("runtime")).expect("create runtime");
            fs::create_dir_all(root.join("state")).expect("create state");
            fs::write(root.join("bin/herdr.exe"), b"bootstrap").expect("write bootstrap");
            fs::write(
                root.join("bin/managed-install-v1/marker"),
                MANAGED_BIN_MARKER,
            )
            .expect("write bin marker");
            Self { root }
        }

        fn install(&self) -> ManagedInstall {
            ManagedInstall::new(self.root.clone())
        }

        fn add_runtime(&self, build_id: &str) -> PathBuf {
            let dir = self.root.join("runtime").join(build_id);
            fs::create_dir(&dir).expect("create build runtime");
            fs::write(dir.join("herdr.exe"), b"payload").expect("write payload");
            fs::write(
                dir.join("runtime.ready"),
                format!("{RUNTIME_RECORD_HEADER}\nbuild_id={build_id}\n"),
            )
            .expect("write marker");
            dir
        }

        fn install_test_executable(&self, build_id: &str, name: &str) {
            fs::copy(
                std::env::current_exe().expect("test executable"),
                self.root.join("runtime").join(build_id).join(name),
            )
            .expect("install test executable");
        }

        fn write_pointer(&self, name: &str, build_id: &str) {
            fs::write(
                self.root.join("state").join(name),
                format!("{POINTER_RECORD_HEADER}\nbuild_id={build_id}\n"),
            )
            .expect("write pointer");
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    struct ProcessHandleGuard {
        handle: HANDLE,
        exited: bool,
    }

    impl ProcessHandleGuard {
        fn open(pid: u32) -> Self {
            // SAFETY: opens a real PID reported by the test child with only
            // synchronization and emergency cleanup rights.
            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
            assert!(
                !handle.is_null(),
                "open helper process {pid}: {}",
                io::Error::last_os_error()
            );
            Self {
                handle,
                exited: false,
            }
        }

        fn wait(&mut self, timeout: Duration) {
            let millis = u32::try_from(timeout.as_millis()).expect("bounded timeout");
            // SAFETY: the guard owns a valid process handle.
            let result = unsafe { WaitForSingleObject(self.handle, millis) };
            assert_eq!(result, WAIT_OBJECT_0, "helper process did not exit in time");
            self.exited = true;
        }

        fn terminate_and_wait(&mut self, timeout: Duration) {
            // SAFETY: the guard owns a live test-only process handle with
            // PROCESS_TERMINATE rights.
            assert_ne!(unsafe { TerminateProcess(self.handle, 1) }, 0);
            self.wait(timeout);
        }
    }

    impl Drop for ProcessHandleGuard {
        fn drop(&mut self) {
            // SAFETY: test-only emergency cleanup for a helper PID. Production
            // launcher code never terminates processes.
            unsafe {
                if !self.exited {
                    TerminateProcess(self.handle, 1);
                    WaitForSingleObject(self.handle, 5_000);
                }
                CloseHandle(self.handle);
            }
        }
    }

    fn harness_test_name(local_name: &str) -> String {
        let module = module_path!()
            .split_once("::")
            .map_or(module_path!(), |(_, module)| module);
        format!("{module}::{local_name}")
    }

    fn helper_command(local_name: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .arg("--exact")
            .arg(harness_test_name(local_name))
            .arg("--nocapture");
        command
    }

    fn spawn_active_test_payload(
        install: &ManagedInstall,
        args: &[OsString],
    ) -> (Child, SharedLease, ConsoleCtrlHandler) {
        spawn_payload(install, args).expect("spawn test payload")
    }

    fn wait_child_bounded(child: &mut Child, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait().expect("inspect helper child") {
                Some(status) => return status,
                None if Instant::now() < deadline => thread::sleep(TEST_POLL_INTERVAL),
                None => {
                    let _ = child.kill();
                    return child.wait().expect("reap timed-out helper child");
                }
            }
        }
    }

    fn kill_child_bounded(child: &mut Child) {
        child.kill().expect("terminate helper parent");
        let status = wait_child_bounded(child, HELPER_TIMEOUT);
        assert!(
            !status.success(),
            "terminated helper unexpectedly succeeded"
        );
    }

    fn accept_helper_streams(
        listener: &TcpListener,
        expected: usize,
    ) -> HashMap<String, (u32, TcpStream)> {
        listener
            .set_nonblocking(true)
            .expect("nonblocking helper listener");
        let deadline = Instant::now() + HELPER_TIMEOUT;
        let mut helpers = HashMap::new();
        while helpers.len() < expected && Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .expect("bound helper handshake");
                    let mut line = String::new();
                    BufReader::new(stream.try_clone().expect("clone helper stream"))
                        .read_line(&mut line)
                        .expect("read helper handshake");
                    let mut fields = line.split_whitespace();
                    let role = fields.next().expect("helper role").to_string();
                    let pid = fields
                        .next()
                        .expect("helper pid")
                        .parse::<u32>()
                        .expect("numeric helper pid");
                    assert!(fields.next().is_none(), "unexpected helper handshake");
                    helpers.insert(role, (pid, stream));
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(TEST_POLL_INTERVAL);
                }
                Err(err) => panic!("accept helper connection: {err}"),
            }
        }
        assert_eq!(helpers.len(), expected, "helpers did not become ready");
        helpers
    }

    #[test]
    fn launcher_requires_the_exact_managed_bin_layout() {
        let tree = TestTree::new("roles");
        let runtime_dir = tree.add_runtime(OLD_BUILD);
        tree.write_pointer(ACTIVE_POINTER, OLD_BUILD);

        let install = resolve_install(&tree.root.join("bin/herdr.exe")).expect("launcher role");
        assert_eq!(install.root(), tree.root);
        assert_eq!(
            managed_install_command_executable_platform(runtime_dir.join("herdr.exe"))
                .expect("stable command"),
            tree.root.join("bin/herdr.exe")
        );

        fs::write(tree.root.join("bin/managed-install-v1/marker"), b"wrong\n")
            .expect("corrupt bin marker");
        assert!(resolve_install(&tree.root.join("bin/herdr.exe")).is_err());
        assert!(
            managed_install_command_executable_platform(runtime_dir.join("herdr.exe")).is_err()
        );
    }

    #[test]
    fn build_id_query_is_available_only_from_valid_staged_launcher_names() {
        let _env = PROCESS_ENV_LOCK.lock().expect("process env lock");
        let args = [OsString::from(LAUNCHER_BUILD_ID_QUERY_ARG)];
        std::env::set_var(MANAGED_LEASE_HANDLE_ENV, "1234");
        let staged = PathBuf::from(r"C:\stage\herdr-launcher.exe");
        assert_eq!(
            maybe_run_build_id_query(&staged, &args).expect("staged build query"),
            Some(0)
        );
        assert!(std::env::var_os(MANAGED_LEASE_HANDLE_ENV).is_none());
        let embedded = PathBuf::from(r"C:\setup\app-launcher.exe");
        assert_eq!(
            maybe_run_build_id_query(&embedded, &args).expect("embedded build query"),
            Some(0)
        );
        let pending = PathBuf::from(format!(
            r"C:\Herdr\state\{PENDING_LAUNCHER_PREFIX}{}{PENDING_LAUNCHER_SUFFIX}",
            "a".repeat(64)
        ));
        assert_eq!(
            maybe_run_build_id_query(&pending, &args).expect("pending build query"),
            Some(0)
        );
        let malformed = PathBuf::from(r"C:\Herdr\state\launcher.pending-not-a-hash.exe");
        assert!(maybe_run_build_id_query(&malformed, &args)
            .expect("malformed pending query")
            .is_none());
        let installed = PathBuf::from(r"C:\Herdr\bin\herdr.exe");
        assert!(maybe_run_build_id_query(&installed, &args)
            .expect("installed query")
            .is_none());
    }

    #[test]
    fn runtime_validation_rejects_mismatched_marker_and_hard_link() {
        let tree = TestTree::new("runtime-validation");
        let runtime_dir = tree.add_runtime(OLD_BUILD);
        let install = tree.install();
        let build_id = BuildId::parse(OLD_BUILD).expect("build id");
        fs::write(
            install.runtime_marker_path(&build_id),
            format!("{RUNTIME_RECORD_HEADER}\nbuild_id={NEW_BUILD}\n"),
        )
        .expect("replace marker");
        assert!(install.validate_runtime(&build_id).is_err());

        fs::write(
            install.runtime_marker_path(&build_id),
            format!("{RUNTIME_RECORD_HEADER}\nbuild_id={OLD_BUILD}\n"),
        )
        .expect("restore marker");
        let payload = runtime_dir.join("herdr.exe");
        fs::hard_link(&payload, tree.root.join("payload-hard-link.exe")).expect("create hard link");
        assert!(install.validate_runtime(&build_id).is_err());
    }

    #[test]
    fn managed_payload_rejects_missing_and_wrong_runtime_leases() {
        let _env = PROCESS_ENV_LOCK.lock().expect("process env lock");
        let tree = TestTree::new("lease-rejection");
        tree.add_runtime(OLD_BUILD);
        tree.add_runtime(NEW_BUILD);
        tree.install_test_executable(OLD_BUILD, "herdr.exe");
        let install = tree.install();
        let old_id = BuildId::parse(OLD_BUILD).expect("old build id");
        let new_id = BuildId::parse(NEW_BUILD).expect("new build id");
        drop(
            install
                .try_open_exclusive_lease(&old_id)
                .expect("prepare old lease")
                .expect("exclusive old lease"),
        );

        let payload = tree.root.join("runtime").join(OLD_BUILD).join("herdr.exe");
        let launch_probe = |mode: &str| {
            let mut command = Command::new(&payload);
            command
                .arg("--exact")
                .arg(harness_test_name("lease_adoption_rejection_helper"))
                .arg("--nocapture")
                .env(ADOPTION_PROBE_ENV, mode)
                .env_remove(MANAGED_LEASE_HANDLE_ENV)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command
        };

        let mut missing = launch_probe("missing")
            .spawn()
            .expect("spawn missing lease probe");
        assert!(wait_child_bounded(&mut missing, HELPER_TIMEOUT).success());

        let wrong_lease = install
            .open_shared_lease(&new_id)
            .expect("open wrong-build lease");
        let mut wrong_command = launch_probe("wrong");
        wrong_lease.configure_payload_child(&mut wrong_command);
        let mut wrong = wrong_command.spawn().expect("spawn wrong lease probe");
        assert!(wait_child_bounded(&mut wrong, HELPER_TIMEOUT).success());
    }

    #[test]
    fn share_mode_leases_interoperate_and_coordination_is_bounded() {
        let _env = PROCESS_ENV_LOCK.lock().expect("process env lock");
        let tree = TestTree::new("sharing");
        tree.add_runtime(OLD_BUILD);
        let install = tree.install();
        let old_id = BuildId::parse(OLD_BUILD).expect("old build id");
        let first = install
            .open_shared_lease(&old_id)
            .expect("first shared lease");
        let second = install
            .open_shared_lease(&old_id)
            .expect("second shared lease");
        assert!(install
            .try_open_exclusive_lease(&old_id)
            .expect("exclusive lease probe")
            .is_none());
        drop(first);
        drop(second);
        assert!(install
            .try_open_exclusive_lease(&old_id)
            .expect("exclusive lease probe")
            .is_some());

        let gate = CoordinationGate::acquire(&install, Duration::from_millis(100))
            .expect("first coordination gate");
        let started = Instant::now();
        let error = CoordinationGate::acquire(&install, Duration::from_millis(80))
            .expect_err("second coordination gate should time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(gate);
    }

    #[test]
    fn pending_uninstall_blocks_payload_launch_before_runtime_spawn() {
        let tree = TestTree::new("pending-uninstall");
        tree.add_runtime(OLD_BUILD);
        tree.write_pointer(ACTIVE_POINTER, OLD_BUILD);
        fs::write(
            tree.root.join("state").join(UNINSTALL_PENDING_MARKER),
            b"herdr-uninstall-v1\n",
        )
        .expect("write pending uninstall marker");

        let error = match spawn_payload(&tree.install(), &[]) {
            Ok(_) => panic!("pending uninstall launched a managed payload"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("uninstall is pending"));
    }

    #[test]
    fn managed_payload_lease_survives_launcher_exit_but_not_payload_descendants() {
        let _env = PROCESS_ENV_LOCK.lock().expect("process env lock");
        let tree = TestTree::new("lease-lifetime");
        tree.add_runtime(OLD_BUILD);
        tree.add_runtime(NEW_BUILD);
        tree.install_test_executable(OLD_BUILD, "herdr.exe");
        tree.install_test_executable(OLD_BUILD, LEASE_DESCENDANT_EXE);
        tree.write_pointer(ACTIVE_POINTER, OLD_BUILD);
        let install = tree.install();
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind helper listener");
        let address = listener.local_addr().expect("helper listener address");

        let mut owner = helper_command("lease_owner_helper");
        owner
            .env(OWNER_ROOT_ENV, &tree.root)
            .env(OWNER_ADDR_ENV, address.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut owner = owner.spawn().expect("spawn lease owner helper");
        let mut helpers = accept_helper_streams(&listener, 3);
        let (_, _owner_stream) = helpers.remove("owner").expect("owner handshake");
        let (payload_pid, _payload_stream) = helpers.remove("payload").expect("payload handshake");
        let (descendant_pid, mut descendant_stream) =
            helpers.remove("descendant").expect("descendant handshake");
        let mut payload_guard = ProcessHandleGuard::open(payload_pid);
        let mut descendant_guard = ProcessHandleGuard::open(descendant_pid);

        kill_child_bounded(&mut owner);
        tree.write_pointer(PENDING_POINTER, NEW_BUILD);
        prepare_post_exit_maintenance(&install, Duration::from_secs(1))
            .expect("defer activation while inherited lease is live");
        assert_eq!(
            install
                .read_required_active_pointer()
                .expect("active pointer")
                .as_str(),
            OLD_BUILD
        );
        assert!(install
            .read_pointer(PENDING_POINTER)
            .expect("pending pointer")
            .is_some());

        payload_guard.terminate_and_wait(HELPER_TIMEOUT);
        prepare_post_exit_maintenance(&install, Duration::from_secs(1))
            .expect("activate after hard-killed payload while descendant remains alive");
        assert_eq!(
            install
                .read_required_active_pointer()
                .expect("active pointer")
                .as_str(),
            NEW_BUILD
        );
        assert!(install
            .read_pointer(PENDING_POINTER)
            .expect("pending pointer")
            .is_none());

        descendant_stream
            .write_all(b"x")
            .expect("release payload descendant");
        descendant_guard.wait(HELPER_TIMEOUT);
    }

    #[test]
    fn launcher_forwards_arguments_environment_cwd_and_exit_code() {
        let _env = PROCESS_ENV_LOCK.lock().expect("process env lock");
        let tree = TestTree::new("process-contract");
        tree.add_runtime(OLD_BUILD);
        tree.install_test_executable(OLD_BUILD, "herdr.exe");
        tree.write_pointer(ACTIVE_POINTER, OLD_BUILD);
        let args = vec![
            OsString::from("--exact"),
            OsString::from(harness_test_name("payload_process_helper")),
            OsString::from("--nocapture"),
        ];
        std::env::set_var(CHILD_ENV, "inherited");
        std::env::set_var(
            CHILD_CWD_ENV,
            std::env::current_dir().expect("current directory"),
        );
        let launched = spawn_active_test_payload(&tree.install(), &args);
        std::env::remove_var(CHILD_ENV);
        std::env::remove_var(CHILD_CWD_ENV);
        let (mut child, lease, _console_handler) = launched;
        let status = wait_child_bounded(&mut child, HELPER_TIMEOUT);
        drop(lease);
        assert_eq!(status.code(), Some(37));
    }

    #[test]
    fn detached_launcher_preserves_headless_payload_behavior() {
        let _env = PROCESS_ENV_LOCK.lock().expect("process env lock");
        let tree = TestTree::new("detached");
        tree.add_runtime(OLD_BUILD);
        tree.install_test_executable(OLD_BUILD, "herdr.exe");
        tree.write_pointer(ACTIVE_POINTER, OLD_BUILD);
        let mut command = helper_command("detached_launcher_helper");
        command
            .env(DETACHED_ROOT_ENV, &tree.root)
            .creation_flags(DETACHED_PROCESS)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut helper = command.spawn().expect("spawn detached launcher helper");
        let status = wait_child_bounded(&mut helper, HELPER_TIMEOUT);
        assert_eq!(status.code(), Some(41));
    }

    #[test]
    fn payload_process_helper() {
        if std::env::current_exe()
            .expect("helper executable")
            .file_name()
            != Some(OsStr::new("herdr.exe"))
        {
            return;
        }
        let _lease = adopt_managed_runtime_lease_platform().expect("adopt process payload lease");
        assert!(std::env::var_os(MANAGED_LEASE_HANDLE_ENV).is_none());
        assert_eq!(std::env::var(CHILD_ENV).as_deref(), Ok("inherited"));
        assert_eq!(
            std::env::current_dir().expect("helper current directory"),
            PathBuf::from(std::env::var_os(CHILD_CWD_ENV).expect("inherited cwd"))
        );
        assert_eq!(
            std::env::args_os().skip(1).collect::<Vec<_>>(),
            [
                OsString::from("--exact"),
                OsString::from(harness_test_name("payload_process_helper")),
                OsString::from("--nocapture")
            ]
        );
        std::process::exit(37);
    }

    #[test]
    fn lease_adoption_rejection_helper() {
        if std::env::current_exe()
            .expect("lease rejection executable")
            .file_name()
            != Some(OsStr::new("herdr.exe"))
            || std::env::var_os(ADOPTION_PROBE_ENV).is_none()
        {
            return;
        }
        let error = adopt_managed_runtime_lease_platform()
            .expect_err("managed payload accepted an invalid runtime lease");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn lease_owner_helper() {
        let Some(root) = std::env::var_os(OWNER_ROOT_ENV) else {
            return;
        };
        let address = std::env::var(OWNER_ADDR_ENV).expect("owner helper address");
        let install = ManagedInstall::new(PathBuf::from(root));
        let args = vec![
            OsString::from("--exact"),
            OsString::from(harness_test_name("lease_payload_helper")),
            OsString::from("--nocapture"),
        ];
        let (child, _lease, _console_handler) = spawn_active_test_payload(&install, &args);
        let mut stream = TcpStream::connect(address).expect("connect owner helper");
        writeln!(stream, "owner {}", std::process::id()).expect("owner handshake");
        stream.flush().expect("flush owner handshake");
        let mut release = [0_u8; 1];
        stream.read_exact(&mut release).expect("owner release");
        drop(child);
    }

    #[test]
    fn lease_payload_helper() {
        if std::env::current_exe()
            .expect("lease payload executable")
            .file_name()
            != Some(OsStr::new("herdr.exe"))
        {
            return;
        }
        let _lease = adopt_managed_runtime_lease_platform().expect("adopt managed payload lease");
        assert!(std::env::var_os(MANAGED_LEASE_HANDLE_ENV).is_none());

        let address = std::env::var(OWNER_ADDR_ENV).expect("lease payload address");
        let descendant = std::env::current_exe()
            .expect("lease payload executable")
            .parent()
            .expect("lease payload build directory")
            .join(LEASE_DESCENDANT_EXE);
        let mut child = Command::new(descendant);
        child
            .arg("--exact")
            .arg(harness_test_name("lease_descendant_helper"))
            .arg("--nocapture")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = child.spawn().expect("spawn payload descendant");

        let mut stream = TcpStream::connect(address).expect("connect lease payload");
        writeln!(stream, "payload {}", std::process::id()).expect("payload handshake");
        stream.flush().expect("flush child handshake");
        let mut release = [0_u8; 1];
        stream.read_exact(&mut release).expect("child release");
        drop(child);
    }

    #[test]
    fn lease_descendant_helper() {
        if std::env::current_exe()
            .expect("lease descendant executable")
            .file_name()
            != Some(OsStr::new(LEASE_DESCENDANT_EXE))
        {
            return;
        }
        assert!(std::env::var_os(MANAGED_LEASE_HANDLE_ENV).is_none());
        let address = std::env::var(OWNER_ADDR_ENV).expect("lease descendant address");
        let mut stream = TcpStream::connect(address).expect("connect lease descendant");
        writeln!(stream, "descendant {}", std::process::id()).expect("descendant handshake");
        stream.flush().expect("flush descendant handshake");
        let mut release = [0_u8; 1];
        stream.read_exact(&mut release).expect("descendant release");
    }

    #[test]
    fn detached_launcher_helper() {
        let Some(root) = std::env::var_os(DETACHED_ROOT_ENV) else {
            return;
        };
        let install = ManagedInstall::new(PathBuf::from(root));
        let args = vec![
            OsString::from("--exact"),
            OsString::from(harness_test_name("detached_payload_helper")),
            OsString::from("--nocapture"),
        ];
        let (mut child, lease, _console_handler) = spawn_active_test_payload(&install, &args);
        let status = wait_child_bounded(&mut child, Duration::from_secs(5));
        drop(lease);
        std::process::exit(status.code().expect("detached payload exit code"));
    }

    #[test]
    fn detached_payload_helper() {
        if std::env::current_exe()
            .expect("detached payload executable")
            .file_name()
            != Some(OsStr::new("herdr.exe"))
        {
            return;
        }
        let _lease = adopt_managed_runtime_lease_platform().expect("adopt detached payload lease");
        assert!(std::env::var_os(MANAGED_LEASE_HANDLE_ENV).is_none());
        assert!(unsafe { GetConsoleWindow() }.is_null());
        assert_eq!(unsafe { GetConsoleCP() }, 0);
        std::process::exit(41);
    }
}
