//! Remote thin-client launcher over SSH command stdio.

use super::{args::*, process::wait_with_output_timeout, restart_policy::*, shell_quote};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

#[cfg(unix)]
use interprocess::local_socket::traits::Listener as _;
#[cfg(all(test, unix))]
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::ListenerNonblockingMode;
use interprocess::TryClone as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const BRIDGE_ACCEPT_POLL: Duration = Duration::from_millis(50);
const BRIDGE_IO_POLL: Duration = Duration::from_millis(1);
const BRIDGE_SOCKET_PERMISSION_MODE: u32 = 0o600;
const REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
const NONINTERACTIVE_SSH_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const NONINTERACTIVE_SSH_STDERR_LIMIT: usize = 16 * 1024;
const BRIDGE_FAILURE_REPORT_TIMEOUT: Duration = Duration::from_secs(1);
const REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CURRENT_PROTOCOL: u32 = crate::protocol::PROTOCOL_VERSION;
const REMOTE_BINARY_ENV_VAR: &str = "HERDR_REMOTE_BINARY";
pub(super) const REMOTE_OUTPUT_READY_MARKER: &str = "herdr-remote-output-ready:1";
const SSH_CONTROL_SOCKET_NAME: &str = "ctl";
const WINDOWS_POWERSHELL_EXECUTABLE: &str = "powershell.exe";

fn preview_update_manifest_url() -> &'static str {
    crate::distribution::PREVIEW_MANIFEST_URL
}
pub(crate) fn run_remote(remote: RemoteLaunch) -> io::Result<()> {
    let session_name = crate::session::active_name()
        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string());
    let local_socket = local_forward_socket_path(&remote.target, &session_name);
    let program = std::env::args()
        .next()
        .unwrap_or_else(|| "herdr".to_string());
    let reattach_command = reattach_command(
        &program,
        &remote.target,
        &session_name,
        remote.keybindings,
        remote.live_handoff,
    );
    let manage_ssh_config = crate::config::Config::load()
        .config
        .remote
        .manage_ssh_config;
    let require_surface_interest = crate::client::endpoint::EndpointCatalog::load()
        .map(|catalog| catalog.contains_enabled_target_session(&remote.target, &session_name))
        .unwrap_or(false);
    let interactive_progress = remote_progress_enabled(
        remote.json,
        io::stdin().is_terminal(),
        io::stderr().is_terminal(),
    );
    let remote_ssh = RemoteSsh::new(
        remote.target.clone(),
        manage_ssh_config,
        session_name.clone(),
        interactive_progress,
    );
    remote_ssh.progress(format_args!(
        "Connecting to {} and checking remote Herdr...",
        remote.target
    ));
    let override_binary = remote_binary_override_path()?;
    let detected = detect_remote_host(
        &remote_ssh,
        override_binary.as_deref(),
        require_surface_interest,
        remote.provision,
    )?;
    if remote.provision {
        let result = provision_remote(&remote_ssh, detected, remote.yes, override_binary)?;
        print_remote_provision_result(&result, remote.json)?;
        return Ok(());
    }
    let remote_herdr = prepare_remote_attachment(
        &remote_ssh,
        detected,
        remote.live_handoff,
        remote.yes,
        override_binary,
        require_surface_interest,
    )?;
    let remote_command = remote_bridge_command(&remote_herdr, &session_name, false)?;

    remote_ssh.progress(format_args!(
        "Opening the remote session on {}; starting its Herdr server if needed...",
        remote.target
    ));
    let _bridge = SshStdioBridge::start(
        remote.target,
        remote_command,
        local_socket.clone(),
        remote_ssh.options(),
        false,
    )?;

    run_client_process(&local_socket, &reattach_command, remote.keybindings)
}

fn remote_progress_enabled(json: bool, stdin_terminal: bool, stderr_terminal: bool) -> bool {
    !json && stdin_terminal && stderr_terminal
}

pub(crate) fn check_saved_ssh(target: &str, session: &str) -> io::Result<()> {
    super::validate_remote_target(target)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    crate::session::validate_name(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let ssh = RemoteSsh::new_noninteractive(target.to_owned(), session.to_owned());
    find_installed_remote_herdr(&ssh).map(|_| ())
}

fn prepare_saved_ssh_with(ssh: &RemoteSsh) -> io::Result<RemoteHerdr> {
    let override_binary = remote_binary_override_path()?;
    let detected = detect_remote_host(ssh, override_binary.as_deref(), true, false)?;
    let remote_herdr =
        prepare_remote_attachment(ssh, detected, false, false, override_binary, true)?;

    // The bridge already owns daemon startup. EOF closes only this temporary attachment,
    // leaving the named server running even when no local TUI is open yet.
    let output = ssh.user_shell_output(&remote_bridge_command(
        &remote_herdr,
        &ssh.session_name,
        false,
    )?)?;
    if !output.status.success() {
        return Err(command_failed("remote server startup failed", &output));
    }
    match remote_server_status(ssh, &remote_herdr, true)? {
        RemoteServerStatus::Running {
            endpoint_protocol_generation,
            surface_interest,
            health_check,
            detached_server_daemon,
            ..
        } if remote_server_restart_reason(
            endpoint_protocol_generation,
            detached_server_daemon,
            true,
            surface_interest,
            health_check,
        )
        .is_none() =>
        {
            Ok(remote_herdr)
        }
        _ => Err(io::Error::other(
            "remote server is not ready for saved machines",
        )),
    }
}

fn prepare_remote_attachment(
    ssh: &RemoteSsh,
    detected: DetectedRemoteHost,
    live_handoff: bool,
    yes: bool,
    override_binary: Option<PathBuf>,
    require_surface_interest: bool,
) -> io::Result<RemoteHerdr> {
    let DetectedRemoteHost {
        host,
        windows_herdr,
    } = detected;
    let (prepared, known_status, windows) = match host {
        RemoteHostPlatform::Unix(platform) => (
            prepare_remote_herdr(
                ssh,
                platform,
                live_handoff,
                yes,
                override_binary,
                require_surface_interest,
                false,
            )?,
            None,
            false,
        ),
        RemoteHostPlatform::Windows {
            platform,
            user_profile,
            ssh_shell,
        } => {
            if live_handoff {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "live handoff is not supported for Windows remote hosts",
                ));
            }
            super::windows::validate_streaming_shell(&ssh_shell)?;
            match windows_herdr {
                Some(detected)
                    if can_reuse_detected_windows_herdr(
                        &detected,
                        override_binary.as_deref(),
                        require_surface_interest,
                        false,
                    ) =>
                {
                    (
                        PreparedRemoteHerdr {
                            remote_herdr: detected.remote_herdr,
                            installed_or_replaced: false,
                            stop_after_install_approved: false,
                        },
                        Some(detected.server_status),
                        true,
                    )
                }
                detected => (
                    prepare_remote_windows_herdr(
                        ssh,
                        platform,
                        &user_profile,
                        ssh_shell,
                        yes,
                        detected,
                        override_binary,
                    )?,
                    Some(RemoteServerStatus::NotRunning),
                    true,
                ),
            }
        }
    };
    let known_status = match known_status {
        Some(
            status @ RemoteServerStatus::Running {
                endpoint_protocol_generation:
                    Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION),
                ..
            },
        ) if require_surface_interest => Some(
            status.with_endpoint_negotiation(&probe_remote_endpoint(ssh, &prepared.remote_herdr)?),
        ),
        status => status,
    };
    ensure_remote_server_ready(
        ssh,
        &prepared.remote_herdr,
        prepared.stop_after_install_approved || yes,
        live_handoff,
        known_status,
        windows,
        require_surface_interest,
    )?;
    Ok(prepared.remote_herdr)
}

pub(crate) struct SavedSshSetup {
    ssh: RemoteSsh,
    candidates: Vec<RemoteHerdr>,
}

impl SavedSshSetup {
    pub(crate) fn connect(target: &str) -> io::Result<Self> {
        super::validate_remote_target(target)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let manage = crate::config::Config::load()
            .config
            .remote
            .manage_ssh_config;
        let ssh = RemoteSsh::new(
            target.to_owned(),
            manage,
            crate::session::DEFAULT_SESSION_NAME.to_owned(),
            true,
        );
        let detected = detect_remote_host(&ssh, None, false, false)?;
        let candidates = match detected.host {
            RemoteHostPlatform::Unix(platform) => {
                let remote_herdr = RemoteHerdr::for_platform(platform);
                remote_binary_candidates(&ssh, &remote_herdr)?
            }
            RemoteHostPlatform::Windows { ssh_shell, .. } => {
                super::windows::validate_streaming_shell(&ssh_shell)?;
                detected
                    .windows_herdr
                    .map(|detected| detected.remote_herdr)
                    .into_iter()
                    .collect()
            }
        };
        Ok(Self { ssh, candidates })
    }

    pub(crate) fn running_sessions(&self) -> io::Result<Vec<String>> {
        // A fresh host must still reach the normal installation approval.
        if self.candidates.is_empty() {
            return Ok(Vec::new());
        }
        let mut failure = String::new();
        for candidate in &self.candidates {
            let output = match candidate.shell {
                RemoteShell::Posix => self
                    .ssh
                    .sh_output(&format!("{} session list --json", candidate.shell_path))?,
                RemoteShell::WindowsPowerShell => self.ssh.windows_herdr_output(
                    candidate,
                    &["session".into(), "list".into(), "--json".into()],
                )?,
            };
            if output.status.code() == Some(255) {
                return Err(command_failed("remote SSH connection failed", &output));
            }
            if !output.status.success() {
                failure = command_failed("remote session query failed", &output).to_string();
                continue;
            }
            match serde_json::from_slice::<RemoteSessionListJson>(&output.stdout) {
                Ok(list) => {
                    return Ok(list
                        .sessions
                        .into_iter()
                        .filter(|session| {
                            session.running && crate::session::validate_name(&session.name).is_ok()
                        })
                        .map(|session| session.name)
                        .collect())
                }
                Err(error) => failure = format!("invalid remote session list: {error}"),
            }
        }
        Err(io::Error::other(format!(
            "could not discover remote Herdr sessions: {failure}; specify --remote-session to continue"
        )))
    }

    pub(crate) fn prepare(
        mut self,
        session_name: &str,
    ) -> io::Result<Option<crate::client::endpoint::SshMachineMetadata>> {
        crate::session::validate_name(session_name)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        self.ssh.session_name = session_name.to_owned();
        // Session discovery may have inspected the default session. Prepare
        // through the existing owner only after selecting the actual session.
        let remote_herdr = prepare_saved_ssh_with(&self.ssh)?;
        Ok(remote_herdr.machine_metadata())
    }
}

#[derive(Deserialize)]
struct RemoteSessionListJson {
    sessions: Vec<RemoteSessionJson>,
}

#[derive(Deserialize)]
struct RemoteSessionJson {
    name: String,
    running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemotePlatform {
    os: &'static str,
    arch: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteHostPlatform {
    Unix(RemotePlatform),
    Windows {
        platform: RemotePlatform,
        user_profile: String,
        ssh_shell: super::windows::WindowsSshShell,
    },
}

struct DetectedRemoteHost {
    host: RemoteHostPlatform,
    windows_herdr: Option<DetectedWindowsHerdr>,
}

struct DetectedWindowsHerdr {
    remote_herdr: RemoteHerdr,
    server_status: RemoteServerStatus,
    matches_current: bool,
}

impl RemotePlatform {
    fn from_uname(os: &str, arch: &str) -> Option<Self> {
        let os = match os.trim() {
            "Linux" => "linux",
            "Darwin" => "macos",
            _ => return None,
        };
        let arch = match arch.trim() {
            "x86_64" | "amd64" => "x86_64",
            "aarch64" | "arm64" => "aarch64",
            _ => return None,
        };
        Some(Self { os, arch })
    }

    fn windows(arch: &str) -> Option<Self> {
        let arch = match arch.trim().to_ascii_uppercase().as_str() {
            "AMD64" | "X86_64" => "x86_64",
            "ARM64" | "AARCH64" => "aarch64",
            _ => return None,
        };
        Some(Self {
            os: "windows",
            arch,
        })
    }

    fn local() -> Self {
        let os = if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "unknown"
        };

        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "unknown"
        };

        Self { os, arch }
    }

    fn asset_key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

#[derive(Debug, Clone)]
pub(super) struct RemoteHerdr {
    install_suffix: String,
    shell_path: String,
    platform: RemotePlatform,
    shell: RemoteShell,
    remote_sidecar: bool,
    payload_sha256: Option<String>,
    ssh_shell: Option<super::windows::WindowsSshShell>,
    client: Option<RemoteClientStatusJson>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteShell {
    Posix,
    WindowsPowerShell,
}

impl RemoteHerdr {
    pub(super) fn machine_metadata(&self) -> Option<crate::client::endpoint::SshMachineMetadata> {
        let executable = self.client.as_ref()?.binary.as_ref()?.clone();
        let windows = match self.shell {
            RemoteShell::Posix => None,
            RemoteShell::WindowsPowerShell => {
                let shell = self.ssh_shell.as_ref()?;
                super::windows::validate_streaming_shell(shell).ok()?;
                Some(crate::client::endpoint::WindowsSshMetadata {
                    shell: shell.clone(),
                    sidecar: self.remote_sidecar,
                })
            }
        };
        let metadata = crate::client::endpoint::SshMachineMetadata {
            os: self.platform.os.to_owned(),
            executable,
            windows,
        };
        metadata.is_valid().then_some(metadata)
    }

    fn for_platform(platform: RemotePlatform) -> Self {
        let install_suffix = ".local/bin/herdr".to_string();
        let shell_path = format!("\"$HOME/{install_suffix}\"");
        Self {
            install_suffix,
            shell_path,
            platform,
            shell: RemoteShell::Posix,
            remote_sidecar: false,
            payload_sha256: None,
            ssh_shell: None,
            client: None,
        }
    }

    fn for_windows(
        platform: RemotePlatform,
        user_profile: &str,
        payload_sha256: Option<String>,
        ssh_shell: super::windows::WindowsSshShell,
    ) -> Self {
        let install_suffix = ".herdr\\remote\\herdr.exe".to_string();
        let shell_path = format!(
            "{}\\{}",
            user_profile.trim_end_matches(['\\', '/']),
            install_suffix
        );
        Self {
            install_suffix,
            shell_path,
            platform,
            shell: RemoteShell::WindowsPowerShell,
            remote_sidecar: true,
            payload_sha256,
            ssh_shell: Some(ssh_shell),
            client: None,
        }
    }

    fn with_shell_path(mut self, shell_path: String) -> Self {
        self.shell_path = shell_path;
        self
    }

    fn with_posix_path(self, path: &str) -> Self {
        self.with_shell_path(shell_quote(path))
    }

    fn into_path_candidate(mut self) -> Self {
        self.remote_sidecar = false;
        self.payload_sha256 = None;
        self
    }
}

fn posix_remote_output_command(command: &str) -> String {
    format!("printf '\n%s\n' '{REMOTE_OUTPUT_READY_MARKER}'\n{command}")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RemoteAssetRef {
    Url(String),
    Object {
        url: String,
        sha256: Option<String>,
        format: Option<String>,
    },
}

impl RemoteAssetRef {
    fn url(&self) -> &str {
        match self {
            Self::Url(url) => url,
            Self::Object { url, .. } => url,
        }
    }

    fn sha256(&self) -> Option<&str> {
        match self {
            Self::Url(_) => None,
            Self::Object { sha256, .. } => {
                sha256.as_deref().filter(|value| !value.trim().is_empty())
            }
        }
    }

    fn format(&self) -> Option<&str> {
        match self {
            Self::Url(_) => None,
            Self::Object { format, .. } => {
                format.as_deref().filter(|value| !value.trim().is_empty())
            }
        }
    }
}

#[derive(Deserialize)]
struct RemotePreviewManifest {
    prerelease: bool,
    build_id: String,
    protocol: u32,
    assets: BTreeMap<String, RemoteAssetRef>,
    #[serde(default)]
    builds: BTreeMap<String, RemotePreviewBuildMetadata>,
}

#[derive(Deserialize)]
struct RemotePreviewBuildMetadata {
    protocol: u32,
    assets: BTreeMap<String, RemoteAssetRef>,
}

fn current_version() -> String {
    crate::build_info::version()
}

fn current_channel() -> &'static str {
    crate::build_info::channel()
}

struct InstallSource {
    path: PathBuf,
    temporary_dir: Option<PathBuf>,
    kind: InstallSourceKind,
    sha256: Option<String>,
    executable_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallSourceKind {
    Executable,
    WindowsZip,
}

struct RemoteReleaseAsset {
    url: String,
    sha256: Option<String>,
    format: Option<String>,
}

struct TemporaryWindowsScript {
    path: PathBuf,
}

impl TemporaryWindowsScript {
    fn create(name: &str, script: &str) -> io::Result<Self> {
        let temporary = Self {
            path: std::env::temp_dir().join(name),
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary.path)?;
        file.write_all(&super::windows::powershell_script_file_bytes(script))?;
        file.flush()?;
        Ok(temporary)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryWindowsScript {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            tracing::debug!(path = %self.path.display(), %err, "could not remove temporary Windows remote script");
        }
    }
}

pub(super) struct PreparedRemoteHerdr {
    pub(super) remote_herdr: RemoteHerdr,
    installed_or_replaced: bool,
    stop_after_install_approved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RemoteBinaryOutcome {
    AlreadyMatching,
    Installed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RemoteServerOutcome {
    Started,
    Reloaded,
    Restarted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteProvisionServerAction {
    Start,
    Reload,
    Restart,
}

#[derive(Debug, Serialize)]
struct RemoteProvisionResult {
    target: String,
    platform: String,
    binary: String,
    binary_outcome: RemoteBinaryOutcome,
    server_outcome: RemoteServerOutcome,
    version: String,
    protocol: u32,
}

#[derive(Clone)]
pub(super) struct ManagedSshOptions {
    config_path: PathBuf,
    control_path: Option<PathBuf>,
    // Bridge workers may launch SSH after the helper that created this config
    // has gone away. The last options owner removes only the temporary config.
    _directory: Arc<ManagedSshConfigDirectory>,
}

struct ManagedSshConfig {
    options: ManagedSshOptions,
}

struct ManagedSshConfigDirectory(PathBuf);

impl Drop for ManagedSshConfigDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Classify only SSH authentication diagnostics, not transport failures or
/// unknown/changed host keys. This does not imply permission to prompt.
pub(crate) fn ssh_error_requires_authentication(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    if message.contains("host key verification failed")
        || message.contains("remote host identification has changed")
    {
        return false;
    }
    (message.contains("permission denied")
        && ["(publickey", "(keyboard-interactive", "(password"]
            .iter()
            .any(|method| message.contains(method)))
        || (message.contains("signing failed")
            && (message.contains("sign_and_send_pubkey") || message.contains("agent")))
}

/// Keep this owner alive until the child has exited: OpenSSH reads its temporary
/// config after spawn. Dropping it never stops the shared authenticated master.
pub(crate) struct SshAuthenticationCommand {
    pub(crate) command: Command,
    _config: ManagedSshConfig,
}

pub(crate) fn ssh_authentication_command(target: &str) -> io::Result<SshAuthenticationCommand> {
    if target.is_empty() || target.starts_with('-') || target.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SSH target",
        ));
    }
    if !crate::platform::remote_ssh_config_paths().multiplexing {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "interactive SSH recovery requires Unix OpenSSH multiplexing; authenticate outside Herdr on this platform"));
    }
    if !crate::config::Config::load()
        .config
        .remote
        .manage_ssh_config
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "interactive SSH recovery requires remote.manage_ssh_config=true",
        ));
    }
    let config = write_managed_ssh_config(target)?;
    Ok(authentication_command_with_config(target, config))
}

fn authentication_command_with_config(
    target: &str,
    config: ManagedSshConfig,
) -> SshAuthenticationCommand {
    let mut command = Command::new("ssh");
    apply_managed_ssh_options(&mut command, Some(&config.options));
    command
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env_remove("SSH_ASKPASS")
        .arg("-o")
        .arg("BatchMode=no")
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg("NumberOfPasswordPrompts=3")
        .arg("-T")
        .arg(target)
        .arg("exit");
    SshAuthenticationCommand {
        command,
        _config: config,
    }
}

pub(super) struct RemoteSsh {
    target: String,
    session_name: String,
    managed_config: Option<ManagedSshConfig>,
    noninteractive: bool,
    interactive_progress: bool,
}

impl RemoteSsh {
    fn new(
        target: String,
        manage_ssh_config: bool,
        session_name: String,
        interactive_progress: bool,
    ) -> Self {
        let managed_config = if manage_ssh_config {
            write_managed_ssh_config(&target)
                .inspect_err(|err| {
                    tracing::debug!(%err, "could not write managed ssh config; using plain ssh");
                })
                .ok()
        } else {
            None
        };

        Self {
            target,
            session_name,
            managed_config,
            noninteractive: false,
            interactive_progress,
        }
    }

    pub(super) fn new_noninteractive(target: String, session_name: String) -> Self {
        let manage = crate::platform::remote_ssh_config_paths().multiplexing
            && crate::config::Config::load()
                .config
                .remote
                .manage_ssh_config;
        let mut ssh = Self::new(target, manage, session_name, false);
        ssh.noninteractive = true;
        ssh
    }

    fn target(&self) -> &str {
        &self.target
    }

    fn progress(&self, message: impl std::fmt::Display) {
        if self.interactive_progress {
            eprintln!("{message}");
        }
    }

    fn destination(&self) -> String {
        format!("{} (session {})", self.target, self.session_name)
    }

    pub(super) fn options(&self) -> Option<&ManagedSshOptions> {
        self.managed_config.as_ref().map(|config| &config.options)
    }

    fn command(&self) -> Command {
        let mut command = self.base_command();
        if self.noninteractive {
            apply_noninteractive_ssh_options(&mut command);
        }
        command.arg("-T").arg(&self.target);
        command
    }

    fn base_command(&self) -> Command {
        let mut command = Command::new("ssh");
        apply_managed_ssh_options(&mut command, self.options());
        command
    }

    fn scp_command(&self) -> Command {
        let mut command = Command::new("scp");
        command.arg("-O");
        apply_managed_scp_options(&mut command, self.options());
        command
    }

    fn sh_output(&self, script: &str) -> io::Result<Output> {
        let script = posix_remote_output_command(script);
        let mut child = self
            .command()
            .arg("/bin/sh -s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if !self.noninteractive {
            return normalize_remote_output(output_with_forwarded_stderr(
                child,
                Some(script.as_bytes()),
                io::stderr(),
            )?);
        }

        let write_result = if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(script.as_bytes())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh bootstrap stdin missing",
            ))
        };
        let output = wait_with_output_timeout(child, NONINTERACTIVE_SSH_COMMAND_TIMEOUT)?;
        write_result?;
        normalize_remote_output(output)
    }

    fn user_shell_output(&self, remote_command: &str) -> io::Result<Output> {
        let mut command = self.command();
        command
            // Windows OpenSSH can still read the console with stdin redirected to NUL.
            .arg("-n")
            .arg(remote_command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if self.noninteractive {
            wait_with_output_timeout(command.spawn()?, NONINTERACTIVE_SSH_COMMAND_TIMEOUT)
        } else {
            output_with_forwarded_stderr(command.spawn()?, None, io::stderr())
        }
    }

    fn framed_user_shell_output(&self, remote_command: &str) -> io::Result<Output> {
        normalize_remote_output(self.user_shell_output(remote_command)?)
    }

    fn posix_user_shell_output(&self, remote_command: &str) -> io::Result<Output> {
        self.framed_user_shell_output(&posix_remote_output_command(remote_command))
    }

    fn powershell_output(&self, script: &str) -> io::Result<Output> {
        self.user_shell_output(&super::windows::powershell_script_command(script))
    }

    fn powershell_command_output(&self, command: &str) -> io::Result<Output> {
        self.user_shell_output(command)
    }

    fn powershell_script_output(
        &self,
        remote_herdr: &RemoteHerdr,
        script: &str,
    ) -> io::Result<Output> {
        let script_name = windows_bootstrap_script_name()?;
        let temporary = TemporaryWindowsScript::create(&script_name, script)?;
        let destination = windows_scp_destination(&self.target, &script_name);
        let transfer = self
            .scp_command()
            .arg(temporary.path())
            .arg(&destination)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!("failed to start Windows remote bootstrap transfer: {err}"),
                )
            })?;
        drop(temporary);
        if !transfer.status.success() {
            return Err(command_failed(
                "Windows remote bootstrap transfer failed",
                &transfer,
            ));
        }

        let remote_path = windows_remote_home_path(remote_herdr, &script_name)?;
        self.powershell_command_output(&super::windows::powershell_script_file_command(
            &remote_path,
        ))
    }

    fn windows_herdr_output(
        &self,
        remote_herdr: &RemoteHerdr,
        arguments: &[String],
    ) -> io::Result<Output> {
        let executable = (remote_herdr.shell == RemoteShell::WindowsPowerShell)
            .then_some(remote_herdr.shell_path.as_str());
        self.powershell_command_output(&super::windows::powershell_herdr_command(
            executable,
            arguments,
            remote_herdr.remote_sidecar,
        ))
    }

    fn install_herdr(&self, remote_herdr: &RemoteHerdr, source_path: &Path) -> io::Result<()> {
        let output = self.sh_output(&remote_install_prepare_script(remote_herdr))?;
        if !output.status.success() {
            return Err(command_failed("remote install preparation failed", &output));
        }
        let (tmp_path, dest_path) = parse_remote_install_paths(&output.stdout)?;

        let mut child = self
            .command()
            .arg(remote_install_stream_command(&tmp_path))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| {
                io::Error::new(err.kind(), format!("failed to start ssh install: {err}"))
            })?;

        let mut source = File::open(source_path)?;
        let copy_result = if let Some(mut stdin) = child.stdin.take() {
            io::copy(&mut source, &mut stdin).map(|_| ())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh install stdin missing",
            ))
        };
        let status = child.wait()?;
        copy_result?;

        if status.success() {
            let output = self.sh_output(&remote_install_commit_script(&tmp_path, &dest_path))?;
            if output.status.success() {
                Ok(())
            } else {
                Err(command_failed("remote install commit failed", &output))
            }
        } else {
            Err(io::Error::other(format!(
                "remote install exited with {status}"
            )))
        }
    }

    fn install_windows_payload(
        &self,
        remote_herdr: &RemoteHerdr,
        source_path: &Path,
        expected_sha256: &str,
        stop_remote: Option<&RemoteHerdr>,
    ) -> io::Result<RemoteClientStatusJson> {
        let archive_name = windows_payload_archive_name()?;
        let temporary_archive = windows_payload_archive_path(remote_herdr, &archive_name)?;
        self.progress(format_args!(
            "Preparing a temporary Herdr install on {}...",
            self.target
        ));
        let output =
            self.powershell_output(&windows_install_prepare_script(remote_herdr, &archive_name))?;
        if !output.status.success() {
            return Err(command_failed(
                "Windows remote install preparation failed",
                &output,
            ));
        }

        let destination = windows_scp_destination(&self.target, &format!(".herdr/{archive_name}"));
        self.progress(format_args!(
            "Transferring Herdr {} to {}...",
            current_version(),
            self.target
        ));
        let transfer = self
            .scp_command()
            .arg(source_path)
            .arg(&destination)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!("failed to start Windows remote ZIP transfer: {err}"),
                )
            })?;
        if !transfer.success() {
            let _ =
                self.powershell_output(&windows_install_cleanup_archive_script(&temporary_archive));
            return Err(io::Error::other(format!(
                "Windows remote ZIP transfer to {destination} exited with {transfer}"
            )));
        }

        let action = if stop_remote.is_some() {
            "Validating the package, stopping the Herdr server, activating, and verifying"
        } else {
            "Validating the package, activating, and verifying"
        };
        self.progress(format_args!("{action} on {}...", self.target));
        let output = self.powershell_script_output(
            remote_herdr,
            &windows_install_script(
                remote_herdr,
                &temporary_archive,
                expected_sha256,
                stop_remote,
                &self.session_name,
            ),
        )?;
        if !output.status.success() {
            return Err(command_failed(
                "Windows remote payload installation failed",
                &output,
            ));
        }
        let client = parse_client_status_json(&String::from_utf8_lossy(&output.stdout))
            .filter(RemoteClientStatusJson::matches_deployment_identity)
            .ok_or_else(|| {
                io::Error::other(
                    "Windows activation did not report the verified deployment identity",
                )
            })?;
        Ok(client)
    }
}

fn normalize_remote_output(mut output: Output) -> io::Result<Output> {
    normalize_remote_stdout(&mut output.stdout, output.status.success())?;
    Ok(output)
}

fn normalize_remote_stdout(stdout: &mut Vec<u8>, command_succeeded: bool) -> io::Result<()> {
    let consumed = {
        let mut reader = io::Cursor::new(stdout.as_slice());
        match discard_remote_output_preamble(&mut reader) {
            Ok(()) => reader.position() as usize,
            Err(_) if !command_succeeded => return Ok(()),
            Err(err) => return Err(err),
        }
    };
    stdout.drain(..consumed);
    Ok(())
}

fn remote_install_prepare_script(remote_herdr: &RemoteHerdr) -> String {
    format!(
        r#"set -eu
dest="$HOME/{install_suffix}"
dir="${{dest%/*}}"
mkdir -p "$dir"
tmp="${{dest}}.tmp.$$"
printf '%s\0%s\0' "$tmp" "$dest"
"#,
        install_suffix = remote_herdr.install_suffix
    )
}

fn parse_remote_install_paths(stdout: &[u8]) -> io::Result<(String, String)> {
    let mut parts = stdout.split(|byte| *byte == 0);
    let tmp_path = parts.next().unwrap_or_default();
    let dest_path = parts.next().unwrap_or_default();
    if tmp_path.is_empty() || dest_path.is_empty() {
        return Err(io::Error::other(
            "remote install preparation did not return destination paths",
        ));
    }
    let tmp_path = String::from_utf8(tmp_path.to_vec()).map_err(|err| {
        io::Error::other(format!(
            "remote install temporary path is not valid UTF-8: {err}"
        ))
    })?;
    let dest_path = String::from_utf8(dest_path.to_vec()).map_err(|err| {
        io::Error::other(format!(
            "remote install destination path is not valid UTF-8: {err}"
        ))
    })?;
    Ok((tmp_path, dest_path))
}

fn remote_install_stream_command(tmp_path: &str) -> String {
    format!("tee {}", shell_quote(tmp_path))
}

fn remote_install_commit_script(tmp_path: &str, dest_path: &str) -> String {
    format!(
        "set -eu\nchmod 755 {tmp_path}\nmv {tmp_path} {dest_path}\n",
        tmp_path = shell_quote(tmp_path),
        dest_path = shell_quote(dest_path)
    )
}

fn windows_sidecar_root(remote_herdr: &RemoteHerdr) -> String {
    remote_herdr
        .shell_path
        .strip_suffix("\\herdr.exe")
        .unwrap_or(&remote_herdr.shell_path)
        .to_string()
}

fn windows_payload_archive_name() -> io::Result<String> {
    Ok(format!("payload-{}.zip", windows_transfer_id()?))
}

fn windows_bootstrap_script_name() -> io::Result<String> {
    Ok(format!("bootstrap-{}.ps1", windows_transfer_id()?))
}

fn windows_transfer_id() -> io::Result<String> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|err| io::Error::other(format!("system clock precedes Unix epoch: {err}")))?
        .as_nanos();
    Ok(format!("{}-{timestamp:x}", std::process::id()))
}

fn windows_payload_archive_path(
    remote_herdr: &RemoteHerdr,
    archive_name: &str,
) -> io::Result<String> {
    let root = windows_sidecar_root(remote_herdr);
    let (parent, _) = root.rsplit_once('\\').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Windows remote sidecar root has no parent: {root}"),
        )
    })?;
    Ok(format!("{parent}\\{archive_name}"))
}

fn windows_remote_home_path(remote_herdr: &RemoteHerdr, name: &str) -> io::Result<String> {
    let suffix = format!("\\{}", remote_herdr.install_suffix);
    let home = remote_herdr
        .shell_path
        .strip_suffix(&suffix)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Windows remote executable is outside its detected user profile: {}",
                    remote_herdr.shell_path
                ),
            )
        })?;
    Ok(format!("{home}\\{name}"))
}

fn windows_scp_destination(target: &str, relative_path: &str) -> String {
    if let Some(authority) = target.strip_prefix("ssh://") {
        format!("scp://{}/{relative_path}", authority.trim_end_matches('/'))
    } else {
        format!("{target}:{relative_path}")
    }
}

fn windows_install_prepare_script(remote_herdr: &RemoteHerdr, archive_name: &str) -> String {
    let root = windows_sidecar_root(remote_herdr);
    format!(
        "$ErrorActionPreference = 'Stop'\n$destination = {}\n$archiveName = {}\n$parent = Split-Path -Parent $destination\n[IO.Directory]::CreateDirectory($parent) | Out-Null\nif ($archiveName -cnotmatch '^payload-[0-9]+-[0-9a-f]+\\.zip$') {{ throw 'invalid remote payload archive name' }}\n$archive = Join-Path $parent $archiveName\nif ([IO.File]::Exists($archive)) {{ throw \"remote payload archive already exists: $archive\" }}\n",
        super::windows::powershell_quote(&root),
        super::windows::powershell_quote(archive_name),
    )
}

fn windows_install_cleanup_archive_script(archive_path: &str) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'\n$archive = {}\nif ([IO.File]::Exists($archive)) {{ [IO.File]::Delete($archive) }}\n",
        super::windows::powershell_quote(archive_path)
    )
}

fn windows_install_script(
    remote_herdr: &RemoteHerdr,
    archive_path: &str,
    expected_sha256: &str,
    stop_remote: Option<&RemoteHerdr>,
    session_name: &str,
) -> String {
    let root = windows_sidecar_root(remote_herdr);
    let session = session_name;
    let (existing_herdr, existing_sidecar) = stop_remote
        .map(|remote| (remote.shell_path.as_str(), remote.remote_sidecar))
        .unwrap_or(("", false));
    super::windows::powershell_bootstrap_script(&format!(
        "$stage = Invoke-HerdrRemoteStageInstall -Archive {} -Destination {} -ExpectedSha256 {} -ExpectedRuntimeVersion {} -ExpectedProtocol {} -SessionName {}\ntry {{\nInvoke-HerdrRemoteActivateInstall -Stage $stage -Destination {} -ExistingHerdr {} -ExistingSidecar ${} -SessionName {} -ExpectedRuntimeVersion {} -ExpectedProtocol {}\n}} catch {{\nif ($null -ne $stage) {{ Remove-HerdrRemoteStage -Stage $stage -Destination {} }}\nthrow\n}}",
        super::windows::powershell_quote(archive_path),
        super::windows::powershell_quote(&root),
        super::windows::powershell_quote(expected_sha256),
        super::windows::powershell_quote(&current_version()),
        CURRENT_PROTOCOL,
        super::windows::powershell_quote(&session),
        super::windows::powershell_quote(&root),
        super::windows::powershell_quote(existing_herdr),
        if existing_sidecar { "true" } else { "false" },
        super::windows::powershell_quote(&session),
        super::windows::powershell_quote(&current_version()),
        CURRENT_PROTOCOL,
        super::windows::powershell_quote(&root),
    ))
}

// Interactive authentication instructions must be visible before SSH exits.
// Background probes retain their capture-only timeout path to avoid disturbing the TUI.
fn output_with_forwarded_stderr(
    mut child: Child,
    stdin: Option<&[u8]>,
    mut destination: impl io::Write + Send + 'static,
) -> io::Result<Output> {
    let mut child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh command stderr missing"))?;
    let stderr_relay = thread::spawn(move || -> io::Result<Vec<u8>> {
        let mut captured = Vec::new();
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let read = child_stderr.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            captured.extend_from_slice(&buffer[..read]);
            if destination.write_all(&buffer[..read]).is_ok() {
                let _ = destination.flush();
            }
        }
        Ok(captured)
    });

    let write_result = if let Some(bytes) = stdin {
        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(bytes)
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh bootstrap stdin missing",
            ))
        }
    } else {
        Ok(())
    };
    let output_result = child.wait_with_output();
    let stderr_result = stderr_relay
        .join()
        .map_err(|_| io::Error::other("ssh stderr relay panicked"))?;
    let mut output = output_result?;
    write_result?;
    output.stderr = stderr_result?;
    Ok(output)
}

fn apply_noninteractive_ssh_options(command: &mut Command) {
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("NumberOfPasswordPrompts=0")
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("-o")
        .arg("ConnectionAttempts=1")
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=4");
}

fn apply_managed_ssh_options(command: &mut Command, options: Option<&ManagedSshOptions>) {
    // Compress the first connection too: multiplexed bridges inherit the master's transport.
    command.arg("-C");
    let Some(options) = options else {
        return;
    };

    command.arg("-F").arg(&options.config_path);
    if let Some(control_path) = &options.control_path {
        // User ControlPaths may be shared across isolated Herdr configs (or
        // explicitly disabled). Managed auth must use our scoped transport;
        // never stop or unlink a master belonging to the user's SSH setup.
        command
            .arg("-S")
            .arg(control_path)
            .arg("-o")
            .arg("ControlMaster=auto")
            .arg("-o")
            .arg("ControlPersist=600");
    }
}

fn apply_managed_scp_options(command: &mut Command, options: Option<&ManagedSshOptions>) {
    command.arg("-C");
    let Some(options) = options else {
        return;
    };

    command.arg("-F").arg(&options.config_path);
    if let Some(control_path) = &options.control_path {
        // User ControlPaths may be shared across isolated Herdr configs (or
        // explicitly disabled). Managed auth must use our scoped transport;
        // never stop or unlink a master belonging to the user's SSH setup.
        command
            .arg("-o")
            .arg(format!(
                "ControlPath={}",
                ssh_config_quote(&control_path.to_string_lossy())
            ))
            .arg("-o")
            .arg("ControlMaster=auto")
            .arg("-o")
            .arg("ControlPersist=600");
    }
}

impl InstallSource {
    fn persistent(path: PathBuf) -> Self {
        Self {
            path,
            temporary_dir: None,
            kind: InstallSourceKind::Executable,
            sha256: None,
            executable_sha256: None,
        }
    }

    fn temporary(path: PathBuf, temporary_dir: PathBuf) -> Self {
        Self {
            path,
            temporary_dir: Some(temporary_dir),
            kind: InstallSourceKind::Executable,
            sha256: None,
            executable_sha256: None,
        }
    }

    fn windows_zip(path: PathBuf, temporary_dir: Option<PathBuf>, sha256: String) -> Self {
        Self {
            path,
            temporary_dir,
            kind: InstallSourceKind::WindowsZip,
            sha256: Some(sha256),
            executable_sha256: None,
        }
    }

    fn local_windows_zip(
        path: PathBuf,
        temporary_dir: PathBuf,
        sha256: String,
        executable_sha256: String,
    ) -> Self {
        Self {
            path,
            temporary_dir: Some(temporary_dir),
            kind: InstallSourceKind::WindowsZip,
            sha256: Some(sha256),
            executable_sha256: Some(executable_sha256),
        }
    }

    fn cleanup(&self) {
        if let Some(dir) = &self.temporary_dir {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

fn prepare_remote_herdr(
    ssh: &RemoteSsh,
    platform: RemotePlatform,
    live_handoff_enabled: bool,
    yes: bool,
    override_binary: Option<PathBuf>,
    require_surface_interest: bool,
    exact_identity: bool,
) -> io::Result<PreparedRemoteHerdr> {
    let mut remote_herdr = RemoteHerdr::for_platform(platform);
    let remote_binary_candidates = remote_binary_candidates(ssh, &remote_herdr)?;

    if override_binary.is_none() {
        for mut candidate in remote_binary_candidates
            .iter()
            .chain(std::iter::once(&remote_herdr))
            .cloned()
        {
            if let Some(client) = remote_client_status(ssh, &candidate)? {
                if client.supports_endpoint_requirement(require_surface_interest)
                    && (!exact_identity || client.matches_deployment_identity())
                {
                    candidate.client = Some(client);
                    return Ok(PreparedRemoteHerdr {
                        remote_herdr: candidate,
                        installed_or_replaced: false,
                        stop_after_install_approved: false,
                    });
                }
            }
        }
    }

    let mut stop_after_install_approved = false;
    if let Some(status_probe_herdr) = remote_binary_candidates.first().or_else(|| {
        remote_binary_exists(ssh, &remote_herdr)
            .ok()
            .and_then(|exists| exists.then_some(&remote_herdr))
    }) {
        stop_after_install_approved = if yes {
            matches!(
                remote_server_status(ssh, status_probe_herdr, require_surface_interest)?,
                RemoteServerStatus::Running { .. }
            )
        } else {
            confirm_remote_install_with_running_server(
                ssh,
                status_probe_herdr,
                live_handoff_enabled,
                require_surface_interest,
            )?
        };
    }
    confirm_remote_install(
        ssh.target(),
        &remote_herdr,
        &install_source_description(&remote_herdr.platform, override_binary.as_deref()),
        yes || stop_after_install_approved,
    )?;
    ssh.progress(format_args!(
        "Preparing Herdr {} for {}...",
        current_version(),
        ssh.target()
    ));
    let source = resolve_install_source(&remote_herdr.platform, override_binary)?;
    ssh.progress(format_args!(
        "Transferring and installing Herdr {} on {}...",
        current_version(),
        ssh.target()
    ));
    let install_result = ssh.install_herdr(&remote_herdr, &source.path);
    source.cleanup();
    install_result?;

    let client = remote_client_status(ssh, &remote_herdr)?
        .ok_or_else(|| io::Error::other("installed remote binary did not report its identity"))?;
    if !client.supports_endpoint_requirement(require_surface_interest)
        || (exact_identity && !client.matches_deployment_identity())
    {
        return Err(io::Error::other(format!(
            "installed remote herdr at {}, but it does not satisfy the selected deployment identity and endpoint requirement",
            remote_herdr.shell_path
        )));
    }
    remote_herdr.client = Some(client);
    warn_if_remote_bin_not_on_path(ssh)?;
    ssh.progress(format_args!(
        "Herdr {} is installed and verified on {}.",
        current_version(),
        ssh.target()
    ));

    Ok(PreparedRemoteHerdr {
        remote_herdr,
        installed_or_replaced: true,
        stop_after_install_approved,
    })
}

fn prepare_remote_windows_herdr(
    ssh: &RemoteSsh,
    platform: RemotePlatform,
    user_profile: &str,
    ssh_shell: super::windows::WindowsSshShell,
    yes: bool,
    detected: Option<DetectedWindowsHerdr>,
    override_payload: Option<PathBuf>,
) -> io::Result<PreparedRemoteHerdr> {
    let (expected_executable_sha256, _) = if override_payload.is_some() {
        (None, false)
    } else {
        local_windows_attach_identity()?
    };
    let mut managed = RemoteHerdr::for_windows(
        platform.clone(),
        user_profile,
        expected_executable_sha256,
        ssh_shell,
    );
    let stop_before_activation = match detected.as_ref() {
        Some(detected) => approve_windows_replacement(&detected.server_status, yes, || {
            confirm_remote_provision_restart(ssh.target(), &detected.server_status)
        })?,
        None => false,
    };
    confirm_remote_install(
        ssh.target(),
        &managed,
        &install_source_description(&managed.platform, override_payload.as_deref()),
        yes || stop_before_activation,
    )?;

    ssh.progress(format_args!(
        "Preparing Herdr {} for {}...",
        current_version(),
        ssh.target()
    ));
    let source = resolve_windows_install_source(&platform, override_payload)?;
    if source.kind != InstallSourceKind::WindowsZip {
        source.cleanup();
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows remote installation source is not a portable ZIP",
        ));
    }
    let expected_sha256 = source.sha256.as_deref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows remote portable payload is missing its verified SHA-256 identity",
        )
    })?;
    if let Some(expected_executable_sha256) = managed.payload_sha256.as_deref() {
        if source.executable_sha256.as_deref() != Some(expected_executable_sha256) {
            source.cleanup();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "local Windows portable payload executable identity does not match the running client",
            ));
        }
    }
    let stop_remote = if stop_before_activation {
        Some(
            &detected
                .as_ref()
                .ok_or_else(|| {
                    io::Error::other("remote server stop was approved without a status binary")
                })?
                .remote_herdr,
        )
    } else {
        None
    };
    let install_result =
        ssh.install_windows_payload(&managed, &source.path, expected_sha256, stop_remote);
    source.cleanup();
    managed.client = Some(install_result?);
    ssh.progress(format_args!(
        "Herdr {} is installed and verified on {}.",
        current_version(),
        ssh.target()
    ));

    Ok(PreparedRemoteHerdr {
        remote_herdr: managed,
        installed_or_replaced: true,
        stop_after_install_approved: stop_before_activation,
    })
}

fn can_reuse_detected_windows_herdr(
    detected: &DetectedWindowsHerdr,
    override_payload: Option<&Path>,
    require_surface_interest: bool,
    exact_identity: bool,
) -> bool {
    override_payload.is_none()
        && (!exact_identity || detected.matches_current)
        && detected.remote_herdr.client.as_ref().is_some_and(|client| {
            client.supports_endpoint_requirement(require_surface_interest)
                && client.endpoint_capabilities.iter().any(|capability| {
                    capability == crate::protocol::endpoint::WINDOWS_REMOTE_HOST_CAPABILITY
                })
        })
}

fn approve_windows_replacement(
    status: &RemoteServerStatus,
    yes: bool,
    confirm: impl FnOnce() -> io::Result<bool>,
) -> io::Result<bool> {
    if *status == RemoteServerStatus::NotRunning {
        return Ok(false);
    }
    if !yes && !confirm()? {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Windows payload activation requires approval to stop its running server",
        ));
    }
    Ok(true)
}

pub(super) fn find_installed_remote_herdr(ssh: &RemoteSsh) -> io::Result<RemoteHerdr> {
    let detected = detect_remote_host(ssh, None, true, false)?;
    let platform = match detected.host {
        RemoteHostPlatform::Unix(platform) => platform,
        RemoteHostPlatform::Windows { ssh_shell, .. } => {
            super::windows::validate_streaming_shell(&ssh_shell)?;
            if let Some(detected) = detected.windows_herdr {
                if can_reuse_detected_windows_herdr(&detected, None, true, false) {
                    require_saved_server_ready(
                        ssh,
                        &detected.remote_herdr,
                        detected.server_status,
                    )?;
                    return Ok(detected.remote_herdr);
                }
            }
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows Herdr is not ready for this saved machine; set it up interactively",
            ));
        }
    };
    let remote_herdr = RemoteHerdr::for_platform(platform);
    let candidates = remote_binary_candidates(ssh, &remote_herdr)?;
    for mut candidate in candidates.into_iter().chain(std::iter::once(remote_herdr)) {
        if let Some(client) = remote_client_status(ssh, &candidate)? {
            if client.supports_endpoint_requirement(true) {
                candidate.client = Some(client);
                let status = remote_server_status(ssh, &candidate, false)?;
                require_saved_server_ready(ssh, &candidate, status)?;
                return Ok(candidate);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "matching Herdr is not ready on {}; run `herdr --remote {}` interactively to install or update it",
            ssh.target(),
            ssh.target()
        ),
    ))
}

fn require_saved_server_ready(
    ssh: &RemoteSsh,
    remote: &RemoteHerdr,
    status: RemoteServerStatus,
) -> io::Result<()> {
    let RemoteServerStatus::Running {
        endpoint_protocol_generation,
        detached_server_daemon,
        ..
    } = status
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "saved Herdr server is not running; start it explicitly",
        ));
    };
    if endpoint_protocol_generation != Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION)
        || !detached_server_daemon
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "saved Herdr server needs an interactive update",
        ));
    }
    let negotiation = probe_remote_endpoint(ssh, remote)?;
    if !negotiation.supports_surface_interest() || !negotiation.supports_health_check() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "saved Herdr server lacks endpoint lifecycle support",
        ));
    }
    Ok(())
}

pub(super) fn discover_remote_api_metadata(
    ssh: &RemoteSsh,
    session: &str,
) -> io::Result<crate::client::endpoint::SshMachineMetadata> {
    let detected = detect_remote_host(ssh, None, false, false)?;
    let metadata = match detected.host {
        RemoteHostPlatform::Unix(platform) => {
            let output = ssh.framed_user_shell_output(&posix_remote_api_discovery_command(
                &platform, session,
            ))?;
            if !output.status.success() {
                return Err(command_failed("remote binary discovery failed", &output));
            }
            crate::client::endpoint::SshMachineMetadata {
                os: platform.os.to_owned(),
                executable: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
                windows: None,
            }
        }
        RemoteHostPlatform::Windows { ssh_shell, .. } => {
            super::windows::validate_streaming_shell(&ssh_shell)?;
            let remote_herdr =
                detected
                    .windows_herdr
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::Unsupported,
                    "Windows Herdr is not installed for this machine; set it up interactively")
                    })?
                    .remote_herdr;
            let command = remote_api_bridge_command(&remote_herdr, session, true)?;
            let output = ssh.framed_user_shell_output(&command)?;
            if output.status.code() == Some(255) {
                return Err(command_failed("remote SSH connection failed", &output));
            }
            if !output.status.success()
                || String::from_utf8_lossy(&output.stdout).trim() != "herdr-api-bridge-v1"
            {
                return Err(io::Error::new(io::ErrorKind::Unsupported,
                    "remote Herdr does not support machine API forwarding; update Herdr on this machine"));
            }
            crate::client::endpoint::SshMachineMetadata {
                os: "windows".into(),
                executable: remote_herdr.shell_path,
                windows: Some(crate::client::endpoint::WindowsSshMetadata {
                    shell: ssh_shell,
                    sidecar: remote_herdr.remote_sidecar,
                }),
            }
        }
    };
    if !metadata.is_valid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid remote Herdr executable metadata",
        ));
    }
    Ok(metadata)
}

fn detect_remote_platform(ssh: &RemoteSsh) -> io::Result<RemotePlatform> {
    let output = ssh.sh_output("uname -s\nuname -m\n")?;
    if !output.status.success() {
        return Err(command_failed("remote platform detection failed", &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let os = lines.next().unwrap_or_default();
    let arch = lines.next().unwrap_or_default();
    RemotePlatform::from_uname(os, arch).ok_or_else(|| {
        io::Error::other(format!(
            "unsupported remote platform: {} {}",
            os.trim(),
            arch.trim()
        ))
    })
}

fn detect_remote_host(
    ssh: &RemoteSsh,
    override_binary: Option<&Path>,
    require_surface_interest: bool,
    exact_identity: bool,
) -> io::Result<DetectedRemoteHost> {
    if let Some(detected) = detect_remote_windows_attach(
        ssh,
        override_binary.is_some(),
        require_surface_interest,
        exact_identity,
    )? {
        return Ok(detected);
    }
    detect_remote_platform(ssh).map(|platform| DetectedRemoteHost {
        host: RemoteHostPlatform::Unix(platform),
        windows_herdr: None,
    })
}

fn detect_remote_windows_attach(
    ssh: &RemoteSsh,
    override_present: bool,
    require_surface_interest: bool,
    exact_identity: bool,
) -> io::Result<Option<DetectedRemoteHost>> {
    let (expected_payload_sha256, allow_path_candidate) = if override_present || !exact_identity {
        (None, true)
    } else {
        local_windows_attach_identity()?
    };
    let command = super::windows::powershell_attach_probe_command(
        &current_version(),
        CURRENT_PROTOCOL,
        expected_payload_sha256.as_deref(),
        allow_path_candidate,
        Some(&ssh.session_name),
        require_surface_interest,
        exact_identity,
    );
    // Classify remote probe diagnostics before displaying them. SSH's own stderr
    // stays live for authentication and connection errors on the local process.
    let mut output = ssh.powershell_command_output(&format!("{command} 2>&1"))?;
    if !output.status.success() {
        output.stderr.extend_from_slice(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if remote_command_missing(&stderr, WINDOWS_POWERSHELL_EXECUTABLE) {
            return Ok(None);
        }
        return Err(command_failed(
            "Windows remote attach probe failed",
            &output,
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let probe: WindowsAttachProbeJson = serde_json::from_str(stdout.trim()).map_err(|err| {
        io::Error::other(format!(
            "could not parse Windows remote attach probe JSON from `{}`: {err}",
            stdout.trim()
        ))
    })?;
    if !probe.os.eq_ignore_ascii_case("Windows_NT") {
        return Err(io::Error::other(format!(
            "Windows remote attach probe reported unsupported OS {}",
            probe.os
        )));
    }
    let platform = RemotePlatform::windows(&probe.arch).ok_or_else(|| {
        io::Error::other(format!(
            "unsupported Windows remote architecture: {}",
            probe.arch
        ))
    })?;
    if !valid_windows_user_profile(&probe.user_profile) {
        return Err(io::Error::other(format!(
            "Windows remote attach probe reported invalid user profile {}",
            probe.user_profile
        )));
    }

    let ssh_shell = super::windows::WindowsSshShell::from_default_shell(&probe.default_shell);
    let mut windows_herdr = None;
    if let Some(candidate) = probe.candidate {
        if candidate.matches_current
            && (candidate.client.version.as_deref() != Some(current_version().as_str())
                || candidate.client.protocol != Some(CURRENT_PROTOCOL))
        {
            return Err(io::Error::other(
                "Windows remote attach probe reported inconsistent Herdr identity",
            ));
        }
        let managed = RemoteHerdr::for_windows(
            platform.clone(),
            &probe.user_profile,
            candidate
                .matches_current
                .then_some(expected_payload_sha256)
                .flatten(),
            ssh_shell.clone(),
        );
        let mut remote_herdr = if candidate.sidecar {
            if !remote_binary_paths_match(&candidate.path, &managed.shell_path) {
                return Err(io::Error::other(
                    "Windows remote attach probe reported an unexpected sidecar path",
                ));
            }
            managed
        } else {
            if !allow_path_candidate {
                return Err(io::Error::other(
                    "Windows remote attach probe selected PATH for a local development build",
                ));
            }
            managed
                .with_shell_path(candidate.path)
                .into_path_candidate()
        };
        remote_herdr.client = Some(candidate.client);
        windows_herdr = Some(DetectedWindowsHerdr {
            remote_herdr,
            server_status: remote_server_status_from_json(candidate.server),
            matches_current: candidate.matches_current,
        });
    }

    Ok(Some(DetectedRemoteHost {
        host: RemoteHostPlatform::Windows {
            platform,
            user_profile: probe.user_profile,
            ssh_shell,
        },
        windows_herdr,
    }))
}

#[cfg(windows)]
fn local_windows_attach_identity() -> io::Result<(Option<String>, bool)> {
    if option_env!("HERDR_RELEASE_VERSION").is_some() {
        return Ok((None, true));
    }
    let executable = std::env::current_exe()?;
    if crate::update::is_package_manager_managed_exe_path(&executable) {
        return Ok((None, true));
    }
    file_sha256(&executable).map(|sha256| (Some(sha256), false))
}

#[cfg(not(windows))]
fn local_windows_attach_identity() -> io::Result<(Option<String>, bool)> {
    Ok((None, true))
}

fn remote_command_missing(stderr: &str, executable: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains(&executable.to_ascii_lowercase())
        && (stderr.contains("not recognized")
            || stderr.contains("not found")
            || stderr.contains("no such file"))
}

fn valid_windows_user_profile(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
        && !path.contains(['\r', '\n', '\0'])
}

fn remote_binary_candidates(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Vec<RemoteHerdr>> {
    let mut candidates = Vec::new();

    if let Some(path_candidate) = remote_binary_on_path_any(ssh, remote_herdr)? {
        push_if_new_remote_binary_candidate(&mut candidates, path_candidate);
    }

    let output = ssh.sh_output(&known_remote_binary_candidate_script(
        &remote_herdr.platform,
    ))?;
    if !output.status.success() {
        return Err(command_failed("remote binary discovery failed", &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for candidate in remote_herdrs_from_path_discovery(remote_herdr, &stdout) {
        push_if_new_remote_binary_candidate(&mut candidates, candidate);
    }

    Ok(candidates)
}

fn push_if_new_remote_binary_candidate(candidates: &mut Vec<RemoteHerdr>, candidate: RemoteHerdr) {
    if !candidates
        .iter()
        .any(|existing| existing.shell_path == candidate.shell_path)
    {
        candidates.push(candidate);
    }
}

fn known_remote_binary_candidate_script(platform: &RemotePlatform) -> String {
    let mut script = String::from(
        r#"home=${HOME:-}
user=${USER:-}
version="#,
    );
    script.push_str(&shell_quote(&current_version()));
    script.push_str(
        r#"
emit() {
    path=$1
    if [ -n "$path" ] && [ -x "$path" ]; then
        printf '%s\n' "$path"
    fi
}
if [ -n "$home" ]; then
    emit "$home/.local/bin/herdr"
fi
"#,
    );
    if platform.os == "macos" {
        script.push_str(
            r#"    emit "/opt/homebrew/bin/herdr"
    emit "/usr/local/bin/herdr"
"#,
        );
    } else if platform.os == "linux" {
        script.push_str(
            r#"    emit "/home/linuxbrew/.linuxbrew/bin/herdr"
"#,
        );
    }
    script.push_str(
        r#"if [ -n "$home" ]; then
    emit "$home/.local/share/mise/installs/herdr/$version/bin/herdr"
    emit "$home/.local/share/mise/installs/herdr/$version/herdr"
    emit "$home/.local/share/mise/installs/github-ogulcancelik-herdr/$version/herdr"
    emit "$home/.nix-profile/bin/herdr"
fi
if [ -n "$user" ]; then
    emit "/etc/profiles/per-user/$user/bin/herdr"
fi
emit "/nix/var/nix/profiles/default/bin/herdr"
emit "/run/current-system/sw/bin/herdr"
"#,
    );

    script
}

fn remote_binary_on_path_any(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Option<RemoteHerdr>> {
    let output = ssh.posix_user_shell_output("command -v herdr")?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(candidate) = remote_herdr_from_path_discovery(remote_herdr, &stdout) {
            return Ok(Some(candidate));
        }
    }

    // Non-POSIX login shells such as xonsh reject `command -v`; retry through
    // /bin/sh while retaining the login-shell probe for shell-initialized PATHs.
    let output = ssh.sh_output("command -v herdr\n")?;
    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(remote_herdr_from_path_discovery(remote_herdr, &stdout))
}

fn remote_herdrs_from_path_discovery(remote_herdr: &RemoteHerdr, stdout: &str) -> Vec<RemoteHerdr> {
    stdout
        .lines()
        .filter_map(|path| remote_herdr_from_path(remote_herdr, path))
        .collect()
}

fn remote_herdr_from_path_discovery(
    remote_herdr: &RemoteHerdr,
    stdout: &str,
) -> Option<RemoteHerdr> {
    stdout
        .lines()
        .find_map(|path| remote_herdr_from_path(remote_herdr, path))
}

fn remote_herdr_from_path(remote_herdr: &RemoteHerdr, path: &str) -> Option<RemoteHerdr> {
    let path = path.trim();
    if !path.starts_with('/') {
        return None;
    }
    if is_mise_shim_path(path) {
        return None;
    }
    Some(remote_herdr.clone().with_posix_path(path))
}

fn is_mise_shim_path(path: &str) -> bool {
    path.ends_with("/mise/shims/herdr")
}

fn remote_client_status(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Option<RemoteClientStatusJson>> {
    let output = match remote_herdr.shell {
        RemoteShell::Posix => ssh.sh_output(&format!(
            "test -x {0} && {0} status client --json",
            remote_herdr.shell_path
        ))?,
        RemoteShell::WindowsPowerShell => ssh.windows_herdr_output(
            remote_herdr,
            &["status".into(), "client".into(), "--json".into()],
        )?,
    };
    if !output.status.success() {
        if output.status.code() == Some(255) {
            return Err(command_failed("remote SSH connection failed", &output));
        }
        return Ok(None);
    }
    Ok(parse_client_status_json(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn remote_binary_exists(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<bool> {
    match remote_herdr.shell {
        RemoteShell::Posix => {
            let command = format!("test -x {}", remote_herdr.shell_path);
            Ok(ssh.sh_output(&command)?.status.success())
        }
        RemoteShell::WindowsPowerShell => {
            let script = format!(
                "if ([IO.File]::Exists({})) {{ exit 0 }} else {{ exit 1 }}",
                super::windows::powershell_quote(&remote_herdr.shell_path)
            );
            Ok(ssh.powershell_output(&script)?.status.success())
        }
    }
}

fn remote_binary_override_path() -> io::Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(REMOTE_BINARY_ENV_VAR) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{REMOTE_BINARY_ENV_VAR} must not be empty"),
        ));
    }

    let path = PathBuf::from(value);
    let metadata = fs::metadata(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to inspect {REMOTE_BINARY_ENV_VAR} path {}: {err}",
                path.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{REMOTE_BINARY_ENV_VAR} path is not a file: {}",
                path.display()
            ),
        ));
    }

    Ok(Some(path))
}

fn install_source_description(platform: &RemotePlatform, override_binary: Option<&Path>) -> String {
    install_source_description_for(
        platform,
        override_binary,
        local_binary_can_seed_remote(platform),
    )
}

fn install_source_description_for(
    platform: &RemotePlatform,
    override_binary: Option<&Path>,
    local_binary_can_seed_remote: bool,
) -> String {
    if let Some(path) = override_binary {
        return format!("{REMOTE_BINARY_ENV_VAR} ({})", path.display());
    }

    if local_binary_can_seed_remote {
        "the current local herdr binary".to_string()
    } else {
        format!(
            "the {} {} asset for {}",
            current_version(),
            current_channel(),
            platform.asset_key()
        )
    }
}

fn resolve_install_source(
    platform: &RemotePlatform,
    override_binary: Option<PathBuf>,
) -> io::Result<InstallSource> {
    if let Some(path) = override_binary {
        return Ok(InstallSource::persistent(path));
    }

    if *platform == RemotePlatform::local() {
        let path = std::env::current_exe()?;
        if !crate::update::is_package_manager_managed_exe_path(&path) {
            return Ok(InstallSource::persistent(path));
        }
    }

    download_release_asset(platform)
}

fn resolve_windows_install_source(
    platform: &RemotePlatform,
    override_payload: Option<PathBuf>,
) -> io::Result<InstallSource> {
    if let Some(path) = override_payload {
        validate_windows_zip_path(&path)?;
        let sha256 = file_sha256(&path)?;
        return Ok(InstallSource::windows_zip(path, None, sha256));
    }
    if local_binary_compatible_with_remote(platform) {
        let path = std::env::current_exe()?;
        if !crate::update::is_package_manager_managed_exe_path(&path) {
            return package_local_windows_runtime(&path);
        }
    }
    download_release_asset(&RemotePlatform {
        os: "windows",
        arch: "x86_64",
    })
}

fn validate_windows_zip_path(path: &Path) -> io::Result<()> {
    let mut file = File::open(path)?;
    let mut signature = [0_u8; 4];
    std::io::Read::read_exact(&mut file, &mut signature).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("Windows remote payload ZIP is unreadable: {err}"),
        )
    })?;
    if !matches!(signature, [b'P', b'K', 3, 4] | [b'P', b'K', 5, 6]) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{REMOTE_BINARY_ENV_VAR} must identify a complete Windows portable ZIP, not a loose executable"
            ),
        ));
    }
    Ok(())
}

fn file_sha256(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

const WINDOWS_PORTABLE_RUNTIME_REQUIRED_FILES: &[&str] = &[
    "LICENSE.txt",
    "conpty/herdr-conpty.json",
    "conpty/conpty.dll",
    "conpty/x64/OpenConsole.exe",
    "conpty/arm64/OpenConsole.exe",
    "THIRD-PARTY-NOTICES/Microsoft.Windows.Console.ConPTY-LICENSE.txt",
    "THIRD-PARTY-NOTICES/Microsoft.Windows.Console.ConPTY-NOTICE.md",
];

fn local_windows_runtime_root(executable: &Path) -> io::Result<&Path> {
    let root = executable.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "local Windows Herdr executable has no parent directory",
        )
    })?;
    for relative in WINDOWS_PORTABLE_RUNTIME_REQUIRED_FILES {
        let path = root.join(relative.replace('/', "\\"));
        if !path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "local Windows Herdr runtime is missing {relative}; run from a complete portable or managed runtime, or set {REMOTE_BINARY_ENV_VAR} to a complete portable ZIP"
                ),
            ));
        }
    }
    Ok(root)
}

#[cfg(windows)]
fn run_local_windows_payload_packager(
    source_root: &Path,
    stage: &Path,
    archive: &Path,
) -> io::Result<()> {
    let job = crate::platform::ChildProcessJob::new_kill_on_close()?;
    let encoded_script = super::windows::local_payload_package_encoded_script();
    let mut child = Command::new(WINDOWS_POWERSHELL_EXECUTABLE)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded_script,
        ])
        .env("HERDR_LOCAL_PAYLOAD_SOURCE", source_root)
        .env("HERDR_LOCAL_PAYLOAD_STAGE", stage)
        .env("HERDR_LOCAL_PAYLOAD_ARCHIVE", archive)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("failed to start local Windows payload packager: {err}"),
            )
        })?;
    if let Err(err) = job.assign(&child) {
        let _ = child.kill();
        let _ = crate::platform::wait_child_bounded(&mut child, Duration::from_secs(5));
        return Err(io::Error::new(
            err.kind(),
            format!("failed to contain local Windows payload packager: {err}"),
        ));
    }
    let status = match crate::platform::wait_child_bounded(&mut child, Duration::from_secs(120)) {
        Ok(Some(status)) => status,
        Ok(None) => {
            return match job.terminate_and_wait(&mut child, Duration::from_secs(5)) {
                Ok(()) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "local Windows payload packaging exceeded 120 seconds",
                )),
                Err(err) => Err(io::Error::other(format!(
                    "local Windows payload packaging timed out and cleanup failed: {err}"
                ))),
            };
        }
        Err(wait_err) => {
            return match job.terminate_and_wait(&mut child, Duration::from_secs(5)) {
                Ok(()) => Err(io::Error::new(
                    wait_err.kind(),
                    format!("failed to wait for local Windows payload packager: {wait_err}"),
                )),
                Err(cleanup_err) => Err(io::Error::other(format!(
                    "failed to wait for local Windows payload packager ({wait_err}); cleanup also failed: {cleanup_err}"
                ))),
            };
        }
    };
    if !status.success() {
        return Err(io::Error::other(format!(
            "local Windows payload packager exited with {status}"
        )));
    }
    validate_windows_zip_path(archive)
}

#[cfg(windows)]
fn package_local_windows_runtime(executable: &Path) -> io::Result<InstallSource> {
    let source_root = local_windows_runtime_root(executable)?;
    let executable_sha256 = file_sha256(executable)?;
    let runtime_executable = source_root.join("herdr.exe");
    if file_sha256(&runtime_executable)? != executable_sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "local Windows portable runtime herdr.exe does not match the running executable",
        ));
    }
    let temporary_dir = private_download_dir("windows-local-payload")?;
    let stage = temporary_dir.join("stage");
    let archive = temporary_dir.join("payload.zip");
    let result = (|| {
        run_local_windows_payload_packager(source_root, &stage, &archive)?;
        let packaged_executable_sha256 = file_sha256(&stage.join("herdr.exe"))?;
        if packaged_executable_sha256 != executable_sha256 {
            return Err(io::Error::other(
                "local Windows remote executable changed while packaging",
            ));
        }
        let archive_sha256 = file_sha256(&archive)?;
        Ok(InstallSource::local_windows_zip(
            archive.clone(),
            temporary_dir.clone(),
            archive_sha256,
            executable_sha256,
        ))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary_dir);
    }
    result
}

#[cfg(not(windows))]
fn package_local_windows_runtime(_executable: &Path) -> io::Result<InstallSource> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "a local Windows runtime can only be packaged by a Windows client",
    ))
}

fn local_binary_can_seed_remote(platform: &RemotePlatform) -> bool {
    if !local_binary_compatible_with_remote(platform) {
        return false;
    }

    std::env::current_exe()
        .map(|path| {
            !crate::update::is_package_manager_managed_exe_path(&path)
                && (platform.os != "windows" || local_windows_runtime_root(&path).is_ok())
        })
        .unwrap_or(false)
}

fn local_binary_compatible_with_remote(platform: &RemotePlatform) -> bool {
    let local = RemotePlatform::local();
    *platform == local
        || (local.os == "windows"
            && local.arch == "x86_64"
            && platform.os == "windows"
            && platform.arch == "aarch64")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteServerStatus {
    Running {
        version: Option<String>,
        protocol: Option<u32>,
        binary: Option<String>,
        endpoint_protocol_generation: Option<u32>,
        surface_interest: bool,
        health_check: bool,
        live_handoff: bool,
        detached_server_daemon: bool,
    },
    NotRunning,
}

impl RemoteServerStatus {
    fn with_endpoint_negotiation(
        mut self,
        negotiation: &crate::client::endpoint::EndpointNegotiation,
    ) -> Self {
        if let Self::Running {
            surface_interest,
            health_check,
            ..
        } = &mut self
        {
            *surface_interest = negotiation.supports_surface_interest();
            *health_check = negotiation.supports_health_check();
        }
        self
    }
}

fn ensure_remote_server_ready(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    stop_after_install_approved: bool,
    live_handoff_enabled: bool,
    known_status: Option<RemoteServerStatus>,
    restart_required: bool,
    require_surface_interest: bool,
) -> io::Result<()> {
    ssh.progress(format_args!(
        "Checking the Herdr server on {}...",
        ssh.target()
    ));
    let status = match known_status {
        Some(status) => status,
        None => remote_server_status(ssh, remote_herdr, require_surface_interest)?,
    };
    let RemoteServerStatus::Running {
        version,
        endpoint_protocol_generation,
        surface_interest,
        health_check,
        live_handoff,
        detached_server_daemon,
        ..
    } = status
    else {
        return Ok(());
    };

    let Some(reason) = remote_server_restart_reason(
        endpoint_protocol_generation,
        detached_server_daemon,
        require_surface_interest,
        surface_interest,
        health_check,
    ) else {
        return Ok(());
    };

    if live_handoff_enabled && live_handoff {
        ssh.progress(format_args!(
            "Handing the Herdr server on {} to the new binary...",
            ssh.target()
        ));
        match live_handoff_remote_server(ssh, remote_herdr) {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("remote live handoff failed: {err}");
                eprintln!("falling back to remote server restart.");
            }
        }
    }

    if stop_after_install_approved {
        stop_remote_server(ssh, remote_herdr)?;
        return Ok(());
    }

    if confirm_remote_server_stop(&ssh.destination(), version.as_deref(), reason)? {
        stop_remote_server(ssh, remote_herdr)?;
    } else if restart_required {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "the Windows remote server must restart before this client can attach",
        ));
    }
    Ok(())
}

fn confirm_remote_install_with_running_server(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    live_handoff_enabled: bool,
    require_surface_interest: bool,
) -> io::Result<bool> {
    let target = ssh.destination();
    let status = match remote_server_status(ssh, remote_herdr, require_surface_interest) {
        Ok(status) => status,
        Err(err) => {
            if !io::stdin().is_terminal() {
                return Err(io::Error::other(format!(
                    "could not inspect the running remote herdr server on {target} before installing: {err}; run from an interactive terminal to approve updating the remote binary"
                )));
            }
            eprintln!(
                "could not inspect the running remote herdr server on {target} before installing: {err}"
            );
            eprint!("continue installing the remote herdr binary? [y/N] ");
            io::stderr().flush()?;

            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            let answer = answer.trim().to_ascii_lowercase();
            if answer != "y" && answer != "yes" {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "remote herdr install cancelled",
                ));
            }
            return Ok(false);
        }
    };
    confirm_remote_install_with_server_status(
        &target,
        &status,
        live_handoff_enabled,
        require_surface_interest,
    )
}

fn confirm_remote_install_with_server_status(
    target: &str,
    status: &RemoteServerStatus,
    live_handoff_enabled: bool,
    require_surface_interest: bool,
) -> io::Result<bool> {
    let RemoteServerStatus::Running {
        version,
        endpoint_protocol_generation,
        surface_interest,
        health_check,
        live_handoff,
        detached_server_daemon,
        ..
    } = status
    else {
        return Ok(false);
    };
    let plan = remote_install_running_server_plan(
        *endpoint_protocol_generation,
        *detached_server_daemon,
        *surface_interest,
        *health_check,
        *live_handoff,
        live_handoff_enabled,
        require_surface_interest,
    );

    if plan == RemoteInstallRunningServerPlan::KeepRunning {
        if io::stdin().is_terminal() {
            eprintln!("remote herdr server on {target} is already compatible:");
            eprintln!("  server: v{}", version_label(version.as_deref()));
            eprintln!(
                "Herdr will install {} without stopping the running remote server.",
                current_version()
            );
        }
        return Ok(false);
    }

    if !io::stdin().is_terminal() {
        match plan {
            RemoteInstallRunningServerPlan::LiveHandoff => return Ok(false),
            RemoteInstallRunningServerPlan::StopRequired(_) => {
                return Err(io::Error::other(format!(
                    "remote herdr server on {target} is running v{}; run from an interactive terminal to approve stopping it for the update",
                    version_label(version.as_deref())
                )));
            }
            RemoteInstallRunningServerPlan::KeepRunning => return Ok(false),
        }
    }

    if plan == RemoteInstallRunningServerPlan::LiveHandoff {
        eprintln!("remote herdr server on {target} is currently running:");
        eprintln!("  server: v{}", version_label(version.as_deref()));
        eprintln!(
            "Herdr will install {} and hand off live pane processes to the prepared server.",
            current_version()
        );
        return Ok(false);
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!("  server: v{}", version_label(version.as_deref()));
    eprintln!(
        "To complete the remote update, Herdr must stop the running remote server after installing."
    );
    eprintln!(
        "This stops active remote pane processes, including shells, agents, dev servers, and tests."
    );
    eprintln!();
    eprint!(
        "Install {} and stop the remote server now? [y/N] ",
        current_version()
    );
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer != "y" && answer != "yes" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr install cancelled",
        ));
    }

    Ok(true)
}

fn provision_remote(
    ssh: &RemoteSsh,
    detected: DetectedRemoteHost,
    yes: bool,
    override_binary: Option<PathBuf>,
) -> io::Result<RemoteProvisionResult> {
    if !yes && !io::stdin().is_terminal() {
        return Err(io::Error::other(
            "non-interactive remote provisioning requires --yes",
        ));
    }
    // Fail before remote installation when the transferable local configuration is invalid.
    let client_config = crate::config::provision::export()?;
    let DetectedRemoteHost {
        host,
        windows_herdr,
    } = detected;
    let (platform, prepared, known_server_status, config_validated) = match host {
        RemoteHostPlatform::Unix(platform) => {
            let prepared = prepare_remote_herdr(
                ssh,
                platform.clone(),
                false,
                yes,
                override_binary,
                false,
                true,
            )?;
            (platform, prepared, None, false)
        }
        RemoteHostPlatform::Windows {
            platform,
            user_profile,
            ssh_shell,
        } => {
            let (prepared, known_server_status, config_validated) = match windows_herdr {
                Some(detected)
                    if can_reuse_detected_windows_herdr(
                        &detected,
                        override_binary.as_deref(),
                        false,
                        true,
                    ) =>
                {
                    (
                        PreparedRemoteHerdr {
                            remote_herdr: detected.remote_herdr,
                            installed_or_replaced: false,
                            stop_after_install_approved: false,
                        },
                        Some(detected.server_status),
                        false,
                    )
                }
                detected => (
                    prepare_remote_windows_herdr(
                        ssh,
                        platform.clone(),
                        &user_profile,
                        ssh_shell,
                        yes,
                        detected,
                        override_binary,
                    )?,
                    Some(RemoteServerStatus::NotRunning),
                    true,
                ),
            };
            (platform, prepared, known_server_status, config_validated)
        }
    };
    let client = prepared.remote_herdr.client.as_ref()
        .filter(|client| client.matches_deployment_identity())
        .ok_or_else(|| io::Error::other("provisioning requires the exact resolved deployment identity before server activation"))?;
    let version = client
        .version
        .clone()
        .ok_or_else(|| io::Error::other("selected runtime version is missing"))?;
    let protocol = client
        .protocol
        .ok_or_else(|| io::Error::other("selected runtime protocol is missing"))?;
    let binary = client
        .binary
        .clone()
        .ok_or_else(|| io::Error::other("selected runtime executable is missing"))?;
    if let Some(config) = &client_config {
        deploy_remote_config(ssh, &prepared.remote_herdr, config)?;
    }
    if !config_validated || client_config.is_some() {
        validate_remote_config(ssh, &prepared.remote_herdr)?;
    }
    let status = match known_server_status {
        Some(status) => status,
        None => remote_server_status(ssh, &prepared.remote_herdr, false)?,
    };
    let stopped_for_install = prepared.stop_after_install_approved;
    let server_outcome = activate_provisioned_remote(
        ssh,
        &prepared.remote_herdr,
        prepared.installed_or_replaced,
        yes,
        status,
    )?;
    let server_outcome = match (stopped_for_install, server_outcome) {
        (true, RemoteServerOutcome::Started) => RemoteServerOutcome::Restarted,
        (_, outcome) => outcome,
    };

    Ok(RemoteProvisionResult {
        target: ssh.target().to_string(),
        platform: platform.asset_key(),
        binary,
        binary_outcome: if prepared.installed_or_replaced {
            RemoteBinaryOutcome::Installed
        } else {
            RemoteBinaryOutcome::AlreadyMatching
        },
        server_outcome,
        version,
        protocol,
    })
}

fn deploy_remote_config(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    config: &str,
) -> io::Result<()> {
    ssh.progress(format_args!(
        "Applying client configuration on {} (machine-local settings stay unchanged)...",
        ssh.target()
    ));
    let command = match remote_herdr.shell {
        RemoteShell::Posix => format!("{} config provision-import", remote_herdr.shell_path),
        RemoteShell::WindowsPowerShell => super::windows::powershell_config_import_command(
            &remote_herdr.shell_path,
            remote_herdr.remote_sidecar,
        ),
    };
    let mut child = ssh
        .command()
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdin = child.stdin.take();
    let bytes = config.as_bytes().to_vec();
    let writer = thread::spawn(move || match stdin {
        Some(mut stdin) => stdin.write_all(&bytes),
        None => Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "configuration transfer stdin missing",
        )),
    });
    // The existing bounded runner closes the SSH process on timeout, also releasing stdin.
    let output = wait_with_output_timeout(child, NONINTERACTIVE_SSH_COMMAND_TIMEOUT);
    let write_result = writer
        .join()
        .map_err(|_| io::Error::other("configuration transfer failed"))?;
    let output = output?;
    if !output.status.success() {
        return Err(command_failed(
            "remote configuration transfer failed",
            &output,
        ));
    }
    write_result
}

fn validate_remote_config(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    ssh.progress(format_args!(
        "Checking the Herdr configuration on {}...",
        ssh.target()
    ));
    let output = remote_herdr_output(ssh, remote_herdr, &["config", "check"])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failed(
            "remote Herdr configuration is invalid",
            &output,
        ))
    }
}

fn activate_provisioned_remote(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    binary_changed: bool,
    yes: bool,
    status: RemoteServerStatus,
) -> io::Result<RemoteServerOutcome> {
    let binary_matches = if binary_changed {
        false
    } else if let RemoteServerStatus::Running {
        binary: Some(running_binary),
        ..
    } = &status
    {
        let selected_binary = remote_client_binary(remote_herdr)?;
        remote_binary_paths_match_for(remote_herdr.shell, running_binary, &selected_binary)
    } else {
        false
    };
    match remote_provision_server_action(&status, binary_changed, binary_matches) {
        RemoteProvisionServerAction::Start => {
            start_remote_server(ssh, remote_herdr)?;
            Ok(RemoteServerOutcome::Started)
        }
        RemoteProvisionServerAction::Reload => {
            reload_remote_config(ssh, remote_herdr)?;
            Ok(RemoteServerOutcome::Reloaded)
        }
        RemoteProvisionServerAction::Restart => {
            if !yes && !confirm_remote_provision_restart(ssh.target(), &status)? {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "remote Herdr server restart cancelled",
                ));
            }
            stop_remote_server(ssh, remote_herdr)?;
            start_remote_server(ssh, remote_herdr)?;
            Ok(RemoteServerOutcome::Restarted)
        }
    }
}

fn remote_provision_server_action(
    status: &RemoteServerStatus,
    binary_changed: bool,
    binary_matches: bool,
) -> RemoteProvisionServerAction {
    match status {
        RemoteServerStatus::NotRunning => RemoteProvisionServerAction::Start,
        RemoteServerStatus::Running {
            version,
            protocol: Some(CURRENT_PROTOCOL),
            binary: Some(binary),
            detached_server_daemon: true,
            ..
        } if !binary_changed
            && version.as_deref() == Some(current_version().as_str())
            && binary_matches
            && !binary.is_empty() =>
        {
            RemoteProvisionServerAction::Reload
        }
        RemoteServerStatus::Running { .. } => RemoteProvisionServerAction::Restart,
    }
}

fn remote_client_binary(remote_herdr: &RemoteHerdr) -> io::Result<&str> {
    remote_herdr
        .client
        .as_ref()
        .and_then(|client| client.binary.as_deref())
        .ok_or_else(|| {
            io::Error::other("remote client status did not report its executable identity")
        })
}

fn remote_binary_paths_match_for(shell: RemoteShell, running: &str, selected: &str) -> bool {
    match shell {
        RemoteShell::WindowsPowerShell => remote_binary_paths_match(running, selected),
        RemoteShell::Posix => running == selected,
    }
}

fn confirm_remote_provision_restart(target: &str, status: &RemoteServerStatus) -> io::Result<bool> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "remote Herdr server on {target} must restart to activate the provisioned binary; rerun with --yes to approve stopping its pane processes"
        )));
    }
    let version = match status {
        RemoteServerStatus::Running { version, .. } => version_label(version.as_deref()),
        RemoteServerStatus::NotRunning => "not running",
    };
    eprintln!("remote Herdr server on {target} is running v{version}.");
    eprintln!("restarting saves the Herdr session but stops its active pane processes.");
    eprint!("restart the remote server now? [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[derive(Debug, Deserialize)]
struct RemoteConfigReloadJson {
    status: RemoteConfigReloadStatus,
    #[serde(default)]
    diagnostics: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RemoteConfigReloadStatus {
    Applied,
    Partial,
    Failed,
}

fn reload_remote_config(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    ssh.progress(format_args!(
        "Applying the Herdr configuration on {}...",
        ssh.target()
    ));
    let output = remote_herdr_output(ssh, remote_herdr, &["server", "reload-config", "--json"])?;
    if !output.status.success() {
        return Err(command_failed(
            "remote server config reload failed",
            &output,
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: RemoteConfigReloadJson = serde_json::from_str(stdout.trim()).map_err(|err| {
        io::Error::other(format!(
            "could not parse remote config reload JSON from `{}`: {err}",
            stdout.trim()
        ))
    })?;
    if result.status == RemoteConfigReloadStatus::Applied {
        ssh.progress(format_args!(
            "The Herdr configuration is active on {}.",
            ssh.target()
        ));
        return Ok(());
    }
    Err(io::Error::other(format!(
        "remote server config reload was {:?}: {}",
        result.status,
        result.diagnostics.join("; ")
    )))
}

fn start_remote_server(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    ssh.progress(format_args!(
        "Starting the Herdr server on {}...",
        ssh.target()
    ));
    let output = remote_herdr_output(ssh, remote_herdr, &["server", "start"])?;
    if !output.status.success() {
        return Err(command_failed("remote server start failed", &output));
    }
    let status = remote_server_status(ssh, remote_herdr, false)?;
    let selected_binary = remote_client_binary(remote_herdr)?;
    match status {
        RemoteServerStatus::Running {
            version,
            protocol: Some(CURRENT_PROTOCOL),
            binary: Some(running_binary),
            detached_server_daemon: true,
            ..
        } if version.as_deref() == Some(current_version().as_str())
            && remote_binary_paths_match_for(
                remote_herdr.shell,
                &running_binary,
                &selected_binary,
            ) =>
        {
            ssh.progress(format_args!(
                "The Herdr server is running on {}.",
                ssh.target()
            ));
            Ok(())
        }
        _ => Err(io::Error::other(
            "remote server did not report the provisioned binary, version, and protocol after start",
        )),
    }
}

fn print_remote_provision_result(result: &RemoteProvisionResult, json: bool) -> io::Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(result).map_err(io::Error::other)?
        );
    } else {
        println!("remote provision: ok");
        println!("target: {}", result.target);
        println!("platform: {}", result.platform);
        println!("binary: {}", result.binary);
        println!("binary outcome: {:?}", result.binary_outcome);
        println!("server outcome: {:?}", result.server_outcome);
        println!("version: {}", result.version);
        println!("protocol: {}", result.protocol);
    }
    Ok(())
}

fn remote_server_status(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    require_surface_interest: bool,
) -> io::Result<RemoteServerStatus> {
    let output = remote_herdr_output(ssh, remote_herdr, &["status", "server", "--json"])?;
    if !output.status.success() {
        return Err(command_failed("remote server status failed", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let status = parse_remote_server_status_json(stdout.trim())?;
    if require_surface_interest
        && matches!(
            status,
            RemoteServerStatus::Running {
                endpoint_protocol_generation: Some(
                    crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION
                ),
                surface_interest: true,
                health_check: true,
                ..
            }
        )
    {
        // Older status helpers omit newer capabilities. Ask the live endpoint rather than
        // assuming that the installed binary and the running daemon support the same features.
        let negotiation = probe_remote_endpoint(ssh, remote_herdr)?;
        return Ok(status.with_endpoint_negotiation(&negotiation));
    }
    Ok(status)
}

fn probe_remote_endpoint(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<crate::client::endpoint::EndpointNegotiation> {
    let path = local_forward_socket_path(ssh.target(), &ssh.session_name);
    let bridge = SshStdioBridge::start(
        ssh.target.clone(),
        remote_bridge_command(remote_herdr, &ssh.session_name, true)?,
        path.clone(),
        ssh.options(),
        true,
    )?;
    let mut stream = crate::ipc::connect_local_stream(&path)?;
    // Use the saved client's noninteractive path. This metadata-only attachment never
    // acquires a surface or sends pane input.
    match crate::client::probe_endpoint_negotiation(&mut stream) {
        Ok(negotiation) => Ok(negotiation),
        Err(probe_error) => Err(bridge.reported_failure().unwrap_or(probe_error)),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RemoteClientStatusJson {
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    protocol: Option<u32>,
    #[serde(default)]
    endpoint_protocol_generation: Option<u32>,
    #[serde(default)]
    endpoint_capabilities: Vec<String>,
    #[serde(default)]
    remote_bridge_idle_timeout: bool,
}

#[derive(Debug, Deserialize)]
struct WindowsAttachProbeJson {
    os: String,
    arch: String,
    user_profile: String,
    default_shell: String,
    candidate: Option<WindowsAttachSelectionJson>,
}

#[derive(Debug, Deserialize)]
struct WindowsAttachSelectionJson {
    path: String,
    sidecar: bool,
    matches_current: bool,
    client: RemoteClientStatusJson,
    server: RemoteServerStatusJson,
}

impl RemoteClientStatusJson {
    fn matches_deployment_identity(&self) -> bool {
        self.version.as_deref() == Some(current_version().as_str())
            && self.protocol == Some(CURRENT_PROTOCOL)
    }
    fn supports_endpoint_requirement(&self, require_surface_interest: bool) -> bool {
        self.endpoint_protocol_generation
            == Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION)
            && (!require_surface_interest
                || [
                    crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY,
                    crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY,
                    crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY,
                    crate::protocol::endpoint::REMOTE_CONNECT_ONLY_CAPABILITY,
                ]
                .iter()
                .all(|required| {
                    self.endpoint_capabilities
                        .iter()
                        .any(|capability| capability == required)
                }))
    }
}

#[derive(Debug, Deserialize)]
struct RemoteServerStatusJson {
    running: bool,
    version: Option<String>,
    protocol: Option<u32>,
    #[serde(default)]
    binary: Option<String>,
    capabilities: Option<RemoteServerCapabilitiesJson>,
}

#[derive(Debug, Deserialize)]
struct RemoteServerCapabilitiesJson {
    live_handoff: bool,
    #[serde(default)]
    detached_server_daemon: bool,
    #[serde(default)]
    endpoint_protocol_generation: Option<u32>,
    #[serde(default)]
    surface_interest: bool,
    #[serde(default)]
    health_check: bool,
}

fn parse_client_status_json(status: &str) -> Option<RemoteClientStatusJson> {
    status
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<RemoteClientStatusJson>(line).ok())
        .find(|status| {
            status.version.is_some()
                || status.protocol.is_some()
                || status.endpoint_protocol_generation.is_some()
                || !status.endpoint_capabilities.is_empty()
        })
}

fn parse_remote_server_status_json(status: &str) -> io::Result<RemoteServerStatus> {
    let parsed: RemoteServerStatusJson = serde_json::from_str(status).map_err(|err| {
        io::Error::other(format!(
            "could not parse remote server status JSON from `{status}`: {err}"
        ))
    })?;
    Ok(remote_server_status_from_json(parsed))
}

fn remote_server_status_from_json(parsed: RemoteServerStatusJson) -> RemoteServerStatus {
    if !parsed.running {
        return RemoteServerStatus::NotRunning;
    }

    let capabilities = parsed.capabilities;

    RemoteServerStatus::Running {
        version: parsed.version,
        protocol: parsed.protocol,
        binary: parsed.binary,
        endpoint_protocol_generation: capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.endpoint_protocol_generation),
        surface_interest: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.surface_interest),
        health_check: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.health_check),
        live_handoff: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.live_handoff),
        detached_server_daemon: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.detached_server_daemon),
    }
}

fn confirm_remote_server_stop(
    target: &str,
    version: Option<&str>,
    reason: RemoteServerRestartReason,
) -> io::Result<bool> {
    let required_upgrade = matches!(
        reason,
        RemoteServerRestartReason::EndpointProtocol
            | RemoteServerRestartReason::SurfaceInterest
            | RemoteServerRestartReason::HealthCheck
    );
    if !io::stdin().is_terminal() {
        if required_upgrade {
            return Err(io::Error::other(format!(
                "remote herdr server on {target} needs one final update before this client can attach; run from an interactive terminal to approve updating it"
            )));
        }

        eprintln!(
            "remote herdr server on {target} is still running v{}; it will use {} after it restarts.",
            version_label(version),
            current_version()
        );
        return Ok(false);
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!("  server: v{}", version_label(version));
    eprintln!("  prepared binary: {}", current_version());
    eprintln!();

    match reason {
        RemoteServerRestartReason::EndpointProtocol => {
            eprintln!(
                "the remote server predates Herdr's stable endpoint protocol and must update before this client can attach."
            );
        }
        RemoteServerRestartReason::SurfaceInterest => {
            eprintln!(
                "the remote server must restart before it can join saved SSH endpoint federation."
            );
        }
        RemoteServerRestartReason::HealthCheck => {
            eprintln!("the remote server must restart to enable saved SSH endpoint health checks.");
        }
        RemoteServerRestartReason::DaemonDetach => {
            eprintln!(
                "the remote server was started by a herdr build that may not survive SSH connection loss. restart it so network drops disconnect only this client."
            );
        }
    }

    eprintln!(
        "This stops active remote pane processes, including shells, agents, dev servers, and tests."
    );
    let prompt = if required_upgrade {
        "stop and update the remote server, then continue attaching? [y/N] "
    } else {
        "restart the remote server now? [y/N] "
    };
    eprint!("{prompt}");
    io::stderr().flush()?;

    if read_remote_confirmation(&mut io::stdin().lock(), false)? {
        return Ok(true);
    }
    if required_upgrade {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr server stop cancelled",
        ));
    }

    Ok(false)
}

fn live_handoff_remote_server(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let status = remote_client_status(ssh, remote_herdr)?.ok_or_else(|| {
        io::Error::other("could not inspect the prepared remote herdr binary before live handoff")
    })?;
    let protocol = status.protocol.ok_or_else(|| {
        io::Error::other("prepared remote herdr did not report its private protocol")
    })?;
    let version = status
        .version
        .filter(|version| !version.is_empty())
        .ok_or_else(|| io::Error::other("prepared remote herdr did not report its version"))?;
    let command = format!(
        "{} --import-exe {} --expected-protocol {} --expected-version {}",
        remote_session_command(remote_herdr, &ssh.session_name, "server live-handoff"),
        remote_herdr.shell_path,
        protocol,
        shell_quote(&version),
    );
    let output = ssh.sh_output(&command)?;
    if !output.status.success() {
        return Err(command_failed("remote server live handoff failed", &output));
    }

    ssh.progress(format_args!(
        "The Herdr server on {} is using the new binary. Reconnecting...",
        ssh.target()
    ));
    Ok(())
}

fn stop_remote_server(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    ssh.progress(format_args!(
        "Stopping the Herdr server on {}...",
        ssh.target()
    ));
    let output = remote_herdr_output(ssh, remote_herdr, &["server", "stop"])?;
    if !output.status.success() {
        return Err(command_failed("remote server stop failed", &output));
    }
    wait_for_remote_server_shutdown(ssh, remote_herdr)?;

    ssh.progress(format_args!(
        "The Herdr server is stopped on {}.",
        ssh.target()
    ));
    Ok(())
}

fn wait_for_remote_server_shutdown(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let deadline = Instant::now() + REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT;
    loop {
        if remote_server_status(ssh, remote_herdr, false)? == RemoteServerStatus::NotRunning {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "shutdown was requested, but the old remote herdr server on {target} is still responding after {} seconds",
                    REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT.as_secs(),
                    target = ssh.target()
                ),
            ));
        }
        thread::sleep(REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL);
    }
}

fn version_label(version: Option<&str>) -> &str {
    version.unwrap_or("unknown")
}

fn warn_if_remote_bin_not_on_path(ssh: &RemoteSsh) -> io::Result<()> {
    let output = ssh.posix_user_shell_output("command -v herdr")?;
    if output.status.success()
        && remote_shell_resolves_managed_install(&String::from_utf8_lossy(&output.stdout))
    {
        return Ok(());
    }

    eprintln!(
        "herdr: installed remote binary to ~/.local/bin/herdr, but the remote shell does not resolve `herdr` to that path"
    );
    Ok(())
}

fn remote_shell_resolves_managed_install(stdout: &str) -> bool {
    stdout
        .lines()
        .next()
        .map(str::trim)
        .is_some_and(|path| path.ends_with("/.local/bin/herdr"))
}

fn download_release_asset(platform: &RemotePlatform) -> io::Result<InstallSource> {
    let asset_key = platform.asset_key();
    let asset = remote_release_asset(&asset_key)?;

    let dir = private_download_dir(&asset_key)?;
    let path = dir.join("herdr.tmp");
    let status = crate::noninteractive_process::curl_command(&asset.url)
        .args(["--max-time", "120", "-o"])
        .arg(&path)
        .status()
        .map_err(|err| io::Error::new(err.kind(), format!("download failed: {err}")))?;
    if !status.success() {
        let _ = fs::remove_dir_all(&dir);
        return Err(io::Error::other("download failed"));
    }
    let windows_zip = platform.os == "windows";
    if windows_zip && asset.sha256.is_none() {
        let _ = fs::remove_dir_all(&dir);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows remote portable asset is missing its SHA-256 digest",
        ));
    }
    if windows_zip && asset.format.as_deref() != Some("zip") {
        let _ = fs::remove_dir_all(&dir);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows remote portable asset must declare ZIP format",
        ));
    }
    if let Some(expected) = &asset.sha256 {
        if let Err(err) = crate::checksum::verify_sha256(&path, expected) {
            let _ = fs::remove_dir_all(&dir);
            return Err(io::Error::new(
                err.kind(),
                format!("downloaded remote asset checksum verification failed: {err}"),
            ));
        }
    }

    if windows_zip {
        validate_windows_zip_path(&path)?;
        let sha256 = asset.sha256.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows remote portable asset is missing its SHA-256 digest",
            )
        })?;
        Ok(InstallSource::windows_zip(path, Some(dir), sha256))
    } else {
        Ok(InstallSource::temporary(path, dir))
    }
}

fn fetch_remote_manifest(url: &str) -> io::Result<Vec<u8>> {
    let output = crate::noninteractive_process::curl_command(url)
        .args([
            "-H",
            "Cache-Control: no-cache",
            "--retry",
            "3",
            "--connect-timeout",
            "10",
            "--max-time",
            "20",
        ])
        .output()
        .map_err(|err| io::Error::new(err.kind(), format!("curl failed: {err}")))?;
    if !output.status.success() {
        return Err(command_failed("failed to fetch update manifest", &output));
    }
    Ok(output.stdout)
}

fn remote_asset_info(asset: &RemoteAssetRef) -> RemoteReleaseAsset {
    RemoteReleaseAsset {
        url: asset.url().to_string(),
        sha256: asset.sha256().map(str::to_string),
        format: asset.format().map(str::to_string),
    }
}

fn preview_assets_for_build<'a>(
    manifest: &'a RemotePreviewManifest,
    build_id: &str,
) -> io::Result<(u32, &'a BTreeMap<String, RemoteAssetRef>)> {
    if manifest.prerelease {
        return Err(io::Error::other(
            "update manifest is marked as a GitHub prerelease",
        ));
    }
    if manifest.build_id == build_id {
        return Ok((manifest.protocol, &manifest.assets));
    }
    let build = manifest.builds.get(build_id).ok_or_else(|| {
        io::Error::other(format!(
            "preview manifest no longer includes build {build_id}; run `herdr update` locally or set {REMOTE_BINARY_ENV_VAR}=target/release/herdr"
        ))
    })?;
    Ok((build.protocol, &build.assets))
}

fn remote_release_asset(asset_key: &str) -> io::Result<RemoteReleaseAsset> {
    let build_id = crate::build_info::build_id().ok_or_else(|| {
        io::Error::other("Herdr Extended client has no build id; set HERDR_REMOTE_BINARY or install Herdr on the remote manually")
    })?;
    let manifest_bytes = fetch_remote_manifest(preview_update_manifest_url())?;
    let manifest: RemotePreviewManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|err| io::Error::other(format!("failed to parse preview manifest JSON: {err}")))?;
    let (protocol, assets) = preview_assets_for_build(&manifest, build_id)?;
    if protocol != CURRENT_PROTOCOL {
        return Err(io::Error::other(format!(
            "preview manifest has build {build_id} protocol {protocol}, but this client needs protocol {CURRENT_PROTOCOL}; set {REMOTE_BINARY_ENV_VAR}=target/release/herdr or install a matching Herdr on the remote host manually"
        )));
    }
    assets.get(asset_key).map(remote_asset_info).ok_or_else(|| {
        io::Error::other(format!(
            "no {asset_key} binary in the preview manifest for build {build_id}"
        ))
    })
}

fn private_download_dir(asset_key: &str) -> io::Result<PathBuf> {
    let base = crate::platform::remote_private_temp_base();
    fs::create_dir_all(&base)?;
    for attempt in 0..100 {
        let dir = base.join(format!(
            "herdr-remote-{}-{}-{attempt}",
            std::process::id(),
            asset_key
        ));
        match crate::platform::create_remote_private_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to create private herdr remote download directory",
    ))
}

fn read_remote_confirmation(reader: &mut impl io::BufRead, default: bool) -> io::Result<bool> {
    let mut answer = String::new();
    if reader.read_line(&mut answer)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote setup cancelled",
        ));
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        "" => Ok(default),
        _ => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote setup cancelled: expected yes or no",
        )),
    }
}

fn confirm_remote_install(
    target: &str,
    remote_herdr: &RemoteHerdr,
    source_description: &str,
    yes: bool,
) -> io::Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "matching remote herdr {} is not installed at {}; run from an interactive terminal to approve installation",
            current_version(),
            remote_herdr.shell_path
        )));
    }

    eprintln!(
        "matching herdr {} is not installed on {target} for {}.",
        current_version(),
        remote_herdr.platform.asset_key()
    );
    eprint!(
        "Install {} to {}? [Y/n] ",
        source_description, remote_herdr.shell_path
    );
    io::stderr().flush()?;

    if !read_remote_confirmation(&mut io::stdin().lock(), true)? {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr installation cancelled",
        ));
    }

    Ok(())
}

fn remote_session_command(remote_herdr: &RemoteHerdr, session_name: &str, args: &str) -> String {
    format!(
        "{} --session {} {args}",
        remote_herdr.shell_path,
        shell_quote(session_name)
    )
}

pub(super) fn remote_bridge_command(
    remote_herdr: &RemoteHerdr,
    session_name: &str,
    connect_only: bool,
) -> io::Result<String> {
    if connect_only
        && !remote_herdr.client.as_ref().is_some_and(|client| {
            client
                .endpoint_capabilities
                .iter()
                .any(|cap| cap == crate::protocol::endpoint::REMOTE_CONNECT_ONLY_CAPABILITY)
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "remote binary lacks connect-only bridge support; update it interactively",
        ));
    }
    match remote_herdr.shell {
        RemoteShell::Posix => {
            let mut args = String::from("remote-client-bridge");
            if connect_only {
                args.push_str(" --connect-only");
                if remote_herdr
                    .client
                    .as_ref()
                    .is_some_and(|client| client.remote_bridge_idle_timeout)
                {
                    args.push_str(" --idle-timeout-v1");
                }
            }
            Ok(posix_remote_output_command(&format!(
                "exec {}",
                remote_session_command(remote_herdr, session_name, &args)
            )))
        }
        RemoteShell::WindowsPowerShell => {
            let mut arguments = vec![
                "--session".to_string(),
                session_name.to_string(),
                "remote-client-bridge".to_string(),
            ];
            if connect_only {
                arguments.push("--connect-only".into());
            }
            super::windows::streaming_herdr_command(
                &remote_herdr.shell_path,
                &arguments,
                remote_herdr.remote_sidecar,
                remote_herdr.ssh_shell.as_ref().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Windows remote bridge is missing the OpenSSH default shell",
                    )
                })?,
            )
        }
    }
}

fn remote_herdr_output(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    arguments: &[&str],
) -> io::Result<Output> {
    let scoped_arguments = scoped_remote_arguments(Some(&ssh.session_name), arguments);
    match remote_herdr.shell {
        RemoteShell::Posix => {
            let command = std::iter::once(remote_herdr.shell_path.clone())
                .chain(
                    scoped_arguments
                        .iter()
                        .map(|argument| shell_quote(argument)),
                )
                .collect::<Vec<_>>()
                .join(" ");
            ssh.sh_output(&command)
        }
        RemoteShell::WindowsPowerShell => ssh.windows_herdr_output(remote_herdr, &scoped_arguments),
    }
}

fn scoped_remote_arguments(session_name: Option<&str>, arguments: &[&str]) -> Vec<String> {
    let mut scoped = Vec::new();
    if let Some(session_name) = session_name {
        scoped.push("--session".to_string());
        scoped.push(session_name.to_string());
    }
    scoped.extend(arguments.iter().map(|argument| argument.to_string()));
    scoped
}

fn remote_binary_paths_match(running: &str, selected: &str) -> bool {
    normalize_windows_binary_path(running)
        .eq_ignore_ascii_case(&normalize_windows_binary_path(selected))
}

fn normalize_windows_binary_path(path: &str) -> String {
    let normalized = path.trim().replace('/', "\\");
    normalized
        .strip_prefix(r"\\?\")
        .unwrap_or(&normalized)
        .to_string()
}

fn posix_remote_api_discovery_command(platform: &RemotePlatform, session: &str) -> String {
    let script = format!(
        r#"set -f
candidates=$(
command -v herdr
{discovery}
)
IFS='
'
for candidate in $candidates; do
    case "$candidate" in
        */mise/shims/herdr) continue ;;
        /*) ;;
        *) continue ;;
    esac
    [ -x "$candidate" ] || continue
    if capability=$("$candidate" --session {session} remote-api-bridge --check </dev/null 2>/dev/null) && [ "$capability" = herdr-api-bridge-v1 ]; then
        printf '%s\n' "$candidate"
        exit 0
    fi
done
printf '%s\n' 'remote Herdr does not support machine API forwarding; update Herdr on this machine' >&2
exit 2"#,
        discovery = known_remote_binary_candidate_script(platform),
        session = shell_quote(session),
    );
    format!(
        "/bin/sh -c {}",
        shell_quote(&posix_remote_output_command(&script))
    )
}

pub(super) const STALE_API_METADATA: &str = "herdr-machine-metadata-stale-v1";

pub(super) fn cached_remote_api_command(
    metadata: &crate::client::endpoint::SshMachineMetadata,
    session: &str,
) -> io::Result<String> {
    if !metadata.is_valid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid cached SSH executable metadata",
        ));
    }
    if metadata.os == "windows" {
        let windows = metadata.windows.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "cached Windows SSH shell is missing",
            )
        })?;
        let arguments = scoped_remote_arguments(Some(session), &["remote-api-bridge"]);
        return super::windows::checked_api_bridge_command(
            &metadata.executable,
            &arguments,
            windows.sidecar,
            &windows.shell,
        );
    }
    let path = shell_quote(&metadata.executable);
    let session = shell_quote(session);
    let script = format!(
        "if capability=$({path} --session {session} remote-api-bridge --check </dev/null 2>/dev/null) && [ \"$capability\" = herdr-api-bridge-v1 ]; then\n{}\nelse\n    printf '%s\\n' '{STALE_API_METADATA}' >&2\n    exit 78\nfi",
        posix_remote_output_command(&format!("exec {path} --session {session} remote-api-bridge")),
    );
    Ok(format!("/bin/sh -c {}", shell_quote(&script)))
}

pub(super) fn remote_api_bridge_command(
    remote_herdr: &RemoteHerdr,
    session_name: &str,
    check: bool,
) -> io::Result<String> {
    let mut args = scoped_remote_arguments(Some(session_name), &["remote-api-bridge"]);
    if check {
        args.push("--check".into());
    }
    match remote_herdr.shell {
        RemoteShell::Posix => {
            let command = std::iter::once(remote_herdr.shell_path.clone())
                .chain(args.iter().map(|arg| shell_quote(arg)))
                .collect::<Vec<_>>()
                .join(" ");
            Ok(posix_remote_output_command(&format!("exec {command}")))
        }
        RemoteShell::WindowsPowerShell => {
            let shell = remote_herdr.ssh_shell.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Windows remote bridge is missing its validated shell",
                )
            })?;
            super::windows::streaming_herdr_command(
                &remote_herdr.shell_path,
                &args,
                remote_herdr.remote_sidecar,
                shell,
            )
        }
    }
}
fn reattach_command(
    program: &str,
    target: &str,
    session_name: &str,
    keybindings: RemoteKeybindings,
    live_handoff: bool,
) -> String {
    let program = crate::platform::remote_reattach_program(program);
    let target = crate::platform::remote_reattach_argument(target);
    let mut command = format!("{program} --remote {target}");
    if keybindings != RemoteKeybindings::Local {
        command.push_str(" --remote-keybindings ");
        command.push_str(keybindings.as_str());
    }
    if live_handoff {
        command.push_str(" --handoff");
    }
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&crate::platform::remote_reattach_argument(session_name));
    }
    command
}

fn command_failed(context: &str, output: &Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        io::Error::other(format!("{context}: {}", output.status))
    } else {
        io::Error::other(format!("{context}: {stderr}"))
    }
}

pub(super) struct SshStdioBridge {
    local_socket: PathBuf,
    socket_identity: crate::ipc::SocketFileIdentity,
    should_stop: Arc<AtomicBool>,
    failure_rx: mpsc::Receiver<io::Error>,
    thread: Option<JoinHandle<()>>,
}

impl SshStdioBridge {
    pub(super) fn start(
        target: String,
        remote_command: String,
        local_socket: PathBuf,
        ssh_options: Option<&ManagedSshOptions>,
        noninteractive: bool,
    ) -> io::Result<Self> {
        Self::start_command(
            target,
            remote_command,
            local_socket,
            ssh_options,
            noninteractive,
        )
    }

    pub(super) fn start_command(
        target: String,
        remote_command: String,
        local_socket: PathBuf,
        ssh_options: Option<&ManagedSshOptions>,
        noninteractive: bool,
    ) -> io::Result<Self> {
        crate::ipc::prepare_socket_path(&local_socket, |path| {
            format!("remote bridge is already listening at {}", path.display())
        })?;
        let listener = crate::ipc::bind_private_local_listener(&local_socket)?;
        let socket_identity = crate::ipc::socket_file_identity(&local_socket)?;
        if let Err(err) =
            crate::ipc::restrict_socket_permissions(&local_socket, BRIDGE_SOCKET_PERMISSION_MODE)
        {
            let _ = crate::ipc::remove_socket_file_if_owned(&local_socket, &socket_identity);
            return Err(err);
        }
        if let Err(err) = listener.set_nonblocking(ListenerNonblockingMode::Accept) {
            let _ = crate::ipc::remove_socket_file_if_owned(&local_socket, &socket_identity);
            return Err(err);
        }

        let should_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&should_stop);
        let thread_ssh_options = ssh_options.cloned();
        let (failure_tx, failure_rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok(stream) => {
                        let stream = match prepare_remote_bridge_stream(stream) {
                            Ok(stream) => stream,
                            Err(err) => {
                                tracing::error!(
                                    error = %err,
                                    "remote bridge failed to prepare client socket"
                                );
                                continue;
                            }
                        };
                        if let Err(err) = bridge_connection(
                            stream,
                            &target,
                            &remote_command,
                            thread_ssh_options.as_ref(),
                            noninteractive,
                            &thread_stop,
                        ) {
                            let _ =
                                failure_tx.try_send(io::Error::new(err.kind(), err.to_string()));
                            if noninteractive {
                                tracing::warn!(error = %err, "saved SSH endpoint bridge failed");
                            } else {
                                eprintln!("herdr: remote bridge failed: {err}");
                            }
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(BRIDGE_ACCEPT_POLL);
                    }
                    Err(err) => {
                        if noninteractive {
                            tracing::warn!(error = %err, "saved SSH endpoint listener failed");
                        } else {
                            eprintln!("herdr: remote bridge listener failed: {err}");
                        }
                        break;
                    }
                }
            }
        });

        Ok(Self {
            local_socket,
            socket_identity,
            should_stop,
            failure_rx,
            thread: Some(thread),
        })
    }

    pub(super) fn reported_failure(&self) -> Option<io::Error> {
        self.failure_rx
            .recv_timeout(BRIDGE_FAILURE_REPORT_TIMEOUT)
            .ok()
    }
}

fn prepare_remote_bridge_stream(
    mut stream: crate::ipc::LocalStream,
) -> io::Result<crate::ipc::LocalStream> {
    crate::ipc::set_local_stream_polling(&mut stream, false)?;
    Ok(stream)
}

impl Drop for SshStdioBridge {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::Release);
        #[cfg(unix)]
        let _ = crate::ipc::remove_socket_file_if_owned(&self.local_socket, &self.socket_identity);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        #[cfg(windows)]
        let _ = crate::ipc::remove_socket_file_if_owned(&self.local_socket, &self.socket_identity);
    }
}

fn ssh_config_quote(path: &str) -> String {
    format!("\"{path}\"")
}

fn ssh_config_include_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '\\' {
        ssh_config_quote(&path.replace('\\', "/"))
    } else {
        ssh_config_quote(&path)
    }
}

/// Returns the `Include` value for the user's SSH config, or `None` when there
/// is nothing useful to include.
///
/// Git for Windows' OpenSSH (MSYS) does not resolve Windows drive-letter paths
/// inside `Include`, so an absolute `C:/.../.ssh/config` path is silently
/// ignored when herdr runs under Git Bash and host aliases stop resolving.
/// `~/.ssh/config` is expanded by both Windows OpenSSH (to the user profile)
/// and MSYS OpenSSH (through `HOME`), so each shell's `ssh` reads the same user
/// config it would read by default. A missing config is harmless because
/// OpenSSH ignores an `Include` that matches nothing.
#[cfg(windows)]
fn ssh_user_config_include(_path: Option<&Path>) -> Option<String> {
    Some(ssh_config_quote("~/.ssh/config"))
}

#[cfg(not(windows))]
fn ssh_user_config_include(path: Option<&Path>) -> Option<String> {
    path.filter(|path| path.is_file())
        .map(ssh_config_include_path)
}

/// Builds a temporary ssh config that includes the user's settings first, so
/// OpenSSH's first-value-wins behavior preserves explicit user keepalives.
fn write_managed_ssh_config(target: &str) -> io::Result<ManagedSshConfig> {
    let paths = crate::platform::remote_ssh_config_paths();
    let control_path = if paths.multiplexing {
        Some(crate::platform::shared_ssh_control_path(
            &crate::config::config_path(),
            target,
        )?)
    } else {
        None
    };

    let dir = crate::platform::create_remote_ssh_config_dir(SSH_CONTROL_SOCKET_NAME)?;
    let path = dir.join("config");
    let mut contents = String::new();
    if let Some(include) = ssh_user_config_include(paths.user_config.as_deref()) {
        contents.push_str(&format!("Include {include}\n"));
    }
    if let Some(system_config) = paths.system_config.filter(|path| path.is_file()) {
        contents.push_str(&format!(
            "Include {}\n",
            ssh_config_include_path(&system_config)
        ));
    }
    contents.push_str("Host *\n");
    contents.push_str("  ServerAliveInterval 15\n");
    contents.push_str("  ServerAliveCountMax 4\n");

    let write_result = (|| {
        let mut file = crate::platform::create_remote_ssh_config_file(&path)?;
        file.write_all(contents.as_bytes())
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_dir_all(&dir);
        return Err(err);
    }
    Ok(ManagedSshConfig {
        options: ManagedSshOptions {
            config_path: path,
            control_path,
            _directory: Arc::new(ManagedSshConfigDirectory(dir)),
        },
    })
}

struct BridgeUploadStop {
    stopped: AtomicBool,
    wake: crate::platform::RemoteBridgeWake,
}

impl BridgeUploadStop {
    fn new() -> io::Result<Self> {
        Ok(Self {
            stopped: AtomicBool::new(false),
            wake: crate::platform::RemoteBridgeWake::new()?,
        })
    }

    fn cancel(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            if let Err(error) = self.wake.cancel() {
                tracing::debug!(%error, "remote bridge read cancellation failed");
            }
        }
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

#[cfg(all(test, unix))]
pub(crate) fn bridge_upload_cancellation_for_test(
    stream: crate::ipc::LocalStream,
    mut writer: impl io::Write + Send + 'static,
) -> impl FnOnce() {
    stream.set_nonblocking(true).unwrap();
    let stop = Arc::new(BridgeUploadStop::new().unwrap());
    let worker_stop = Arc::clone(&stop);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let closed = AtomicBool::new(false);
        let result = copy_local_stream_to_writer(
            stream,
            &mut writer,
            &worker_stop,
            &AtomicBool::new(false),
            &closed,
        );
        done_tx
            .send((result, closed.load(Ordering::Acquire)))
            .unwrap();
    });
    move || {
        stop.cancel();
        let (result, closed) = done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        worker.join().unwrap();
        result.unwrap();
        assert!(!closed, "upload cancellation must not report peer EOF");
    }
}

fn bridge_connection(
    mut stream: crate::ipc::LocalStream,
    target: &str,
    remote_command: &str,
    ssh_options: Option<&ManagedSshOptions>,
    noninteractive: bool,
    bridge_stop: &Arc<AtomicBool>,
) -> io::Result<()> {
    let upload_stop = Arc::new(BridgeUploadStop::new()?);
    let mut command = Command::new("ssh");
    apply_managed_ssh_options(&mut command, ssh_options);
    if noninteractive {
        apply_noninteractive_ssh_options(&mut command);
    }
    command
        .arg("-T")
        .arg(target)
        .arg(remote_command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if noninteractive {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });

    let mut child = command
        .spawn()
        .map_err(|err| io::Error::new(err.kind(), format!("failed to start ssh bridge: {err}")))?;
    let mut child_stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => return terminate_bridge_child(child, "ssh bridge stdin missing"),
    };
    let child_stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => return terminate_bridge_child(child, "ssh bridge stdout missing"),
    };
    let stderr_reader = if noninteractive {
        let Some(child_stderr) = child.stderr.take() else {
            return terminate_bridge_child(child, "ssh bridge stderr missing");
        };
        Some(thread::spawn(move || capture_ssh_stderr(child_stderr)))
    } else {
        None
    };
    let stream_to_child = match stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
    };
    if let Err(err) = crate::ipc::set_local_stream_polling(&mut stream, true) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err);
    }
    let mut child_to_stream = stream;

    let connection_stop = Arc::new(AtomicBool::new(false));
    let upload_failed = Arc::new(AtomicBool::new(false));
    let download_done = Arc::new(AtomicBool::new(false));
    let client_closed = Arc::new(AtomicBool::new(false));
    let upload_cancel = Arc::clone(&upload_stop);
    let upload_bridge_stop = Arc::clone(bridge_stop);
    let upload_failed_worker = Arc::clone(&upload_failed);
    let upload_client_closed = Arc::clone(&client_closed);
    let upload = thread::spawn(move || {
        let result = copy_local_stream_to_writer(
            stream_to_child,
            &mut child_stdin,
            &upload_cancel,
            &upload_bridge_stop,
            &upload_client_closed,
        );
        upload_failed_worker.store(result.is_err(), Ordering::Release);
        result
    });
    let download_stop = Arc::clone(&connection_stop);
    let download_bridge_stop = Arc::clone(bridge_stop);
    let download_done_worker = Arc::clone(&download_done);
    let download_upload_stop = Arc::clone(&upload_stop);
    let download = thread::spawn(move || {
        let mut child_stdout = io::BufReader::new(child_stdout);
        let result = discard_remote_output_preamble(&mut child_stdout).and_then(|()| {
            copy_reader_to_local_stream(
                &mut child_stdout,
                &mut child_to_stream,
                &download_stop,
                &download_bridge_stop,
            )
        });
        download_done_worker.store(true, Ordering::Release);
        download_upload_stop.cancel();
        result
    });

    let mut stopped_at = None;
    let (status_result, child_exited) = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                upload_stop.cancel();
                break (Ok(status), true);
            }
            Ok(None) => {}
            Err(err) => {
                connection_stop.store(true, Ordering::Release);
                upload_stop.cancel();
                let _ = child.kill();
                let _ = child.wait();
                break (Err(err), false);
            }
        }
        if bridge_stop.load(Ordering::Acquire) {
            connection_stop.store(true, Ordering::Release);
            upload_stop.cancel();
            let _ = child.kill();
            break (child.wait(), false);
        }
        if client_closed.load(Ordering::Acquire)
            || upload_failed.load(Ordering::Acquire)
            || download_done.load(Ordering::Acquire)
        {
            upload_stop.cancel();
            let stopped_at = stopped_at.get_or_insert_with(Instant::now);
            if stopped_at.elapsed() >= Duration::from_millis(250) {
                connection_stop.store(true, Ordering::Release);
                let _ = child.kill();
                break (child.wait(), false);
            }
        }
        thread::sleep(BRIDGE_ACCEPT_POLL);
    };
    upload_stop.cancel();
    if !child_exited {
        connection_stop.store(true, Ordering::Release);
    }
    let upload_result = upload
        .join()
        .map_err(|_| io::Error::other("remote bridge upload worker panicked"))?;
    let download_result = download
        .join()
        .map_err(|_| io::Error::other("remote bridge download worker panicked"))?;
    let stderr = match stderr_reader {
        Some(reader) => reader
            .join()
            .map_err(|_| io::Error::other("SSH stderr reader panicked"))??,
        None => Vec::new(),
    };
    let status = status_result?;

    let stopping = bridge_stop.load(Ordering::Acquire);
    let client_closed = client_closed.load(Ordering::Acquire);
    if child_exited && !status.success() && !stopping && !client_closed {
        return Err(ssh_bridge_exit_error(status, &stderr));
    }
    if !stopping && !client_closed {
        upload_result.map_err(|err| {
            io::Error::new(err.kind(), format!("remote bridge upload failed: {err}"))
        })?;
        download_result.map_err(|err| {
            io::Error::new(err.kind(), format!("remote bridge download failed: {err}"))
        })?;
    }

    if status.success() || stopping || client_closed {
        Ok(())
    } else {
        Err(ssh_bridge_exit_error(status, &stderr))
    }
}

fn ssh_bridge_exit_error(status: std::process::ExitStatus, stderr: &[u8]) -> io::Error {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    let message = if stderr.is_empty() {
        format!("ssh bridge exited with {status}")
    } else {
        format!("remote SSH connection failed: {stderr}")
    };
    io::Error::new(io::ErrorKind::ConnectionAborted, message)
}

fn capture_ssh_stderr(mut stderr: impl io::Read) -> io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 4 * 1024];
    loop {
        let read = stderr.read(&mut buffer)?;
        if read == 0 {
            return Ok(captured);
        }
        let remaining = NONINTERACTIVE_SSH_STDERR_LIMIT.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

fn discard_remote_output_preamble(reader: &mut impl io::BufRead) -> io::Result<()> {
    let marker = REMOTE_OUTPUT_READY_MARKER.as_bytes();
    let mut matched = 0;
    let mut matching = true;
    loop {
        let (consumed, ready) = {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                if matching && matched == marker.len() {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "remote command exited before producing its output marker",
                ));
            }

            let mut consumed = 0;
            let mut ready = false;
            for &byte in buffer {
                consumed += 1;
                if byte == b'\n' {
                    if matching && matched == marker.len() {
                        ready = true;
                        break;
                    }
                    matched = 0;
                    matching = true;
                } else if matching && matched < marker.len() && byte == marker[matched] {
                    matched += 1;
                } else if matching && (matched != marker.len() || byte != b'\r') {
                    matching = false;
                }
            }
            (consumed, ready)
        };
        reader.consume(consumed);
        if ready {
            return Ok(());
        }
    }
}

fn terminate_bridge_child(mut child: std::process::Child, message: &'static str) -> io::Result<()> {
    let _ = child.kill();
    let _ = child.wait();
    Err(io::Error::new(io::ErrorKind::BrokenPipe, message))
}

fn copy_reader_to_local_stream<R: io::Read>(
    reader: &mut R,
    stream: &mut crate::ipc::LocalStream,
    connection_stop: &AtomicBool,
    bridge_stop: &AtomicBool,
) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;

    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        let mut written = 0;
        while written < read {
            if connection_stop.load(Ordering::Acquire) || bridge_stop.load(Ordering::Acquire) {
                return Ok(total);
            }
            let chunk_len = (read - written).min(4 * 1024);
            match stream.write(&buffer[written..written + chunk_len]) {
                Ok(0) => thread::sleep(BRIDGE_IO_POLL),
                Ok(count) => written += count,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(BRIDGE_IO_POLL);
                }
                Err(err) => return Err(err),
            }
        }
        stream.flush()?;
        total += read as u64;
    }
}

fn copy_local_stream_to_writer<W: io::Write>(
    mut stream: crate::ipc::LocalStream,
    writer: &mut W,
    connection_stop: &BridgeUploadStop,
    bridge_stop: &AtomicBool,
    client_closed: &AtomicBool,
) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;

    while !connection_stop.is_stopped() && !bridge_stop.load(Ordering::Acquire) {
        #[cfg(all(test, unix))]
        tests::UPLOAD_READ_ATTEMPTS.with(|attempts| {
            if let Some(attempts) = attempts.borrow().as_ref() {
                attempts.fetch_add(1, Ordering::Relaxed);
            }
        });
        match crate::ipc::poll_local_stream_read_count(&mut stream, &mut buffer)? {
            crate::ipc::LocalStreamReadCount::Data(read) => {
                writer.write_all(&buffer[..read])?;
                writer.flush()?;
                total += read as u64;
            }
            crate::ipc::LocalStreamReadCount::Pending => {
                connection_stop.wake.wait(&stream)?;
            }
            crate::ipc::LocalStreamReadCount::Closed => {
                client_closed.store(true, Ordering::Release);
                break;
            }
        }
    }

    Ok(total)
}

fn run_client_process(
    local_socket: &Path,
    reattach_command: &str,
    keybindings: RemoteKeybindings,
) -> io::Result<()> {
    let exe = crate::managed_install::command_executable()?;
    let status = Command::new(exe)
        .arg("client")
        .env(
            crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
            local_socket,
        )
        .env(REATTACH_COMMAND_ENV_VAR, reattach_command)
        .env(REMOTE_KEYBINDINGS_ENV_VAR, keybindings.as_str())
        .env_remove(crate::api::SOCKET_PATH_ENV_VAR)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            format!("remote client exited with {status}"),
        ))
    }
}

fn local_forward_socket_path(target: &str, session_name: &str) -> PathBuf {
    let pid = std::process::id();
    let target_clean = sanitize_path_component(target);
    let session_clean = sanitize_path_component(session_name);
    let readable_name = format!("herdr-remote-{pid}-{target_clean}-{session_clean}.sock");
    let target_prefix: String = target_clean.chars().take(8).collect();
    let hash = short_socket_hash(target, session_name);
    let short_name = format!("herdr-r-{pid}-{target_prefix}-{hash}.sock");
    crate::platform::remote_bridge_endpoint_path(&readable_name, &short_name)
}

#[cfg(all(test, unix))]
fn fits_unix_socket_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().len() <= 103
}

fn short_socket_hash(target: &str, session: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    target.hash(&mut hasher);
    0u8.hash(&mut hasher);
    session.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn sanitize_path_component(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();

    sanitized.trim_matches('-').chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use interprocess::local_socket::traits::Stream as _;

    #[cfg(windows)]
    #[test]
    fn windows_bridge_copy_progresses_with_polling_and_peer_disconnect() {
        use std::sync::mpsc;

        let socket = local_forward_socket_path("polling-copy-test", "default");
        crate::ipc::prepare_socket_path(&socket, |_| "test bridge is already active".into())
            .unwrap();
        let listener = crate::ipc::bind_private_local_listener(&socket).unwrap();
        let socket_identity = crate::ipc::socket_file_identity(&socket).unwrap();
        let mut client = crate::ipc::connect_local_stream(&socket).unwrap();
        let mut server = prepare_remote_bridge_stream(listener.accept().unwrap()).unwrap();
        crate::ipc::set_local_stream_polling(&mut server, true).unwrap();
        client.set_nonblocking(true).unwrap();
        let (phase_tx, phase_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let writer = thread::spawn(move || {
            let stopped = AtomicBool::new(false);
            let payload = vec![0x5a; 128 * 1024];
            let first = copy_reader_to_local_stream(
                &mut io::Cursor::new(&payload),
                &mut server,
                &stopped,
                &stopped,
            );
            phase_tx.send(first).unwrap();
            continue_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let second = copy_reader_to_local_stream(
                &mut io::Cursor::new(&payload),
                &mut server,
                &stopped,
                &stopped,
            );
            let _ = phase_tx.send(second);
        });

        let mut received = vec![0; 128 * 1024];
        io::Read::read_exact(
            &mut crate::ipc::LocalStreamDeadlineReader::new(&mut client, Duration::from_secs(3)),
            &mut received,
        )
        .expect("polling bridge download must progress");
        assert!(received.iter().all(|byte| *byte == 0x5a));
        assert_eq!(
            phase_rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap(),
            128 * 1024
        );
        continue_tx.send(()).unwrap();
        let mut first_byte = [0];
        io::Read::read_exact(
            &mut crate::ipc::LocalStreamDeadlineReader::new(&mut client, Duration::from_secs(3)),
            &mut first_byte,
        )
        .expect("second download must begin before disconnect");
        // A disappearing client must release a download even when it no longer reads.
        drop(client);
        assert!(phase_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .is_err());
        writer.join().unwrap();
        drop(listener);
        crate::ipc::remove_socket_file_if_owned(&socket, &socket_identity).unwrap();
    }

    fn decode_windows_command(command: &str) -> String {
        use base64::Engine as _;
        let encoded = command
            .split_once("FromBase64String('")
            .expect("encoded PowerShell command")
            .1
            .split_once('\'')
            .unwrap()
            .0;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        let utf16 = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&utf16).expect("UTF-16LE")
    }

    #[cfg(unix)]
    thread_local! {
        pub(super) static UPLOAD_READ_ATTEMPTS: std::cell::RefCell<Option<Arc<std::sync::atomic::AtomicUsize>>> = const { std::cell::RefCell::new(None) };
    }

    #[cfg(unix)]
    fn upload_test_streams(name: &str) -> (crate::ipc::LocalStream, crate::ipc::LocalStream) {
        let socket = local_forward_socket_path(name, "upload-test");
        let listener = crate::ipc::bind_private_local_listener(&socket).unwrap();
        let client = crate::ipc::connect_local_stream(&socket).unwrap();
        let server = listener.accept().unwrap();
        server.set_nonblocking(true).unwrap();
        drop(listener);
        std::fs::remove_file(socket).unwrap();
        (client, server)
    }

    #[cfg(unix)]
    #[test]
    fn bridge_upload_idle_waits_without_repeated_reads_and_cancels() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::mpsc;

        let (mut client, stream) = upload_test_streams("idle");
        let attempts = Arc::new(AtomicUsize::new(0));
        let worker_attempts = Arc::clone(&attempts);
        let stop = Arc::new(BridgeUploadStop::new().unwrap());
        let worker_stop = Arc::clone(&stop);
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            UPLOAD_READ_ATTEMPTS.with(|slot| *slot.borrow_mut() = Some(worker_attempts));
            let mut output = Vec::new();
            let closed = AtomicBool::new(false);
            let result = copy_local_stream_to_writer(
                stream,
                &mut output,
                &worker_stop,
                &AtomicBool::new(false),
                &closed,
            );
            done_tx
                .send((result, output, closed.load(Ordering::Acquire)))
                .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while attempts.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline, "upload worker did not start");
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(100));
        let idle_reads = attempts.load(Ordering::Relaxed);
        client.write_all(b"pane input").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while attempts.load(Ordering::Relaxed) < idle_reads + 2 {
            assert!(
                Instant::now() < deadline,
                "input did not wake the upload worker"
            );
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(100));
        let reads_after_input = attempts.load(Ordering::Relaxed);
        stop.cancel();
        let (result, output, closed) = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert_eq!(result.unwrap(), 10);
        assert_eq!(output, b"pane input");
        assert!(!closed, "cancellation is not a peer disconnect");
        assert_eq!(idle_reads, 1, "idle forwarding must wait, not retry reads");
        assert_eq!(
            reads_after_input, 3,
            "forwarding must sleep again after input"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bridge_upload_cancel_before_wait_preserves_download() {
        use std::io::Read as _;

        let (mut client, stream) = upload_test_streams("cancel-before-wait");
        let mut download = stream.try_clone().unwrap();
        let stop = BridgeUploadStop::new().unwrap();
        stop.cancel();
        stop.cancel();
        let closed = AtomicBool::new(false);
        let count = copy_local_stream_to_writer(
            stream,
            &mut Vec::new(),
            &stop,
            &AtomicBool::new(false),
            &closed,
        )
        .unwrap();
        assert_eq!(count, 0);
        assert!(!closed.load(Ordering::Acquire));
        download.write_all(b"final frame").unwrap();
        let mut output = [0; 11];
        client.read_exact(&mut output).unwrap();
        assert_eq!(&output, b"final frame");
    }

    #[cfg(unix)]
    #[test]
    fn bridge_upload_cancel_between_stop_check_and_wait_is_retained() {
        let (_client, stream) = upload_test_streams("cancel-before-poll");
        let stop = BridgeUploadStop::new().unwrap();
        assert!(!stop.is_stopped());
        stop.cancel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            done_tx.send(stop.wake.wait(&stream)).unwrap();
        });
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bridge_upload_drains_input_before_peer_eof() {
        let (mut client, stream) = upload_test_streams("drain");
        let payload = vec![b'x'; 1024 * 1024];
        let expected = payload.clone();
        let worker = thread::spawn(move || {
            let stop = BridgeUploadStop::new().unwrap();
            let mut output = Vec::new();
            let closed = AtomicBool::new(false);
            let count = copy_local_stream_to_writer(
                stream,
                &mut output,
                &stop,
                &AtomicBool::new(false),
                &closed,
            )
            .unwrap();
            assert!(closed.load(Ordering::Acquire));
            assert_eq!(count, output.len() as u64);
            output
        });
        client.write_all(&payload).unwrap();
        drop(client);
        assert_eq!(worker.join().unwrap(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn bridge_socket_is_user_only() {
        use std::os::unix::fs::PermissionsExt;

        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-permissions-test-{}.sock",
            std::process::id()
        ));
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let bridge = SshStdioBridge::start(
            "example".to_string(),
            remote_bridge_command(&remote_herdr, "default", false).unwrap(),
            socket.clone(),
            None,
            false,
        )
        .expect("start bridge listener");

        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, BRIDGE_SOCKET_PERMISSION_MODE);

        drop(bridge);
        let _ = std::fs::remove_file(socket);
    }

    #[cfg(unix)]
    #[test]
    fn accepted_bridge_stream_is_reset_to_blocking() {
        use std::os::fd::AsRawFd as _;

        fn is_nonblocking(stream: &crate::ipc::LocalStream) -> bool {
            let fd = match stream {
                crate::ipc::LocalStream::UdSocket(stream) => stream.inner().as_raw_fd(),
            };
            // SAFETY: F_GETFL only reads flags from the live descriptor owned by `stream`.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            assert!(flags >= 0, "fcntl(F_GETFL): {}", io::Error::last_os_error());
            flags & libc::O_NONBLOCK != 0
        }

        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-blocking-test-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::ipc::bind_private_local_listener(&socket).expect("bind listener");
        let client = crate::ipc::connect_local_stream(&socket).expect("connect client");
        let mut server = listener.accept().expect("accept client");

        crate::ipc::set_local_stream_polling(&mut server, true)
            .expect("force the macOS accepted-stream state");
        assert!(is_nonblocking(&server));
        let server = prepare_remote_bridge_stream(server).expect("prepare bridge stream");
        assert!(!is_nonblocking(&server));

        drop(server);
        drop(client);
        drop(listener);
        let _ = std::fs::remove_file(socket);
    }

    #[cfg(windows)]
    #[test]
    fn bridge_stream_delivers_large_frame_before_delayed_reply() {
        use std::io::Read as _;
        use std::sync::mpsc;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn read_frame(stream: &mut crate::ipc::LocalStream) -> Vec<u8> {
            let mut length = [0; 4];
            stream.read_exact(&mut length).expect("read frame length");
            let length = usize::try_from(u32::from_le_bytes(length)).expect("frame length fits");
            let mut body = vec![0; length];
            stream.read_exact(&mut body).expect("read frame body");
            body
        }

        fn write_frame(stream: &mut crate::ipc::LocalStream, body: &[u8]) {
            let length = u32::try_from(body.len()).expect("frame length fits");
            stream
                .write_all(&length.to_le_bytes())
                .expect("write frame length");
            stream.write_all(body).expect("write frame body");
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-large-frame-{}-{nonce}.sock",
            std::process::id()
        ));
        let listener = crate::ipc::bind_private_local_listener(&socket).expect("bind listener");
        let (large_received_tx, large_received_rx) = mpsc::channel();
        let client_socket = socket.clone();
        let client = thread::spawn(move || {
            let mut stream =
                crate::ipc::connect_local_stream(&client_socket).expect("connect client");
            assert_eq!(read_frame(&mut stream), b"welcome");
            assert_eq!(read_frame(&mut stream), vec![b'x'; 16 * 1024]);
            large_received_tx.send(()).expect("signal large frame");
            assert_eq!(read_frame(&mut stream), b"pong");
        });
        let mut server = prepare_remote_bridge_stream(listener.accept().expect("accept client"))
            .expect("prepare bridge stream");

        crate::ipc::set_local_stream_polling(&mut server, true)
            .expect("enable bridge read polling");
        write_frame(&mut server, b"welcome");
        write_frame(&mut server, &vec![b'x'; 16 * 1024]);
        large_received_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("client received large frame");
        write_frame(&mut server, b"pong");

        client.join().expect("client thread");
        drop(server);
        drop(listener);
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn bridge_drop_while_waiting_for_client_is_bounded() {
        let socket = local_forward_socket_path("drop-test", "default");
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let bridge = SshStdioBridge::start(
            "example".to_string(),
            remote_bridge_command(&remote_herdr, "default", false).unwrap(),
            socket.clone(),
            None,
            false,
        )
        .expect("start bridge listener");
        let started = Instant::now();

        drop(bridge);

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!socket.exists());
    }

    #[cfg(unix)]
    #[test]
    fn managed_ssh_config_includes_user_config_then_fallback() {
        use std::os::unix::fs::PermissionsExt;

        let managed_config = write_managed_ssh_config("example").expect("write managed config");
        let path = managed_config.options.config_path.clone();
        let control_path = managed_config
            .options
            .control_path
            .clone()
            .expect("Unix managed config has a control path");
        let contents = std::fs::read_to_string(&path).expect("read keepalive config");

        // herdr's fallback transport settings are present...
        assert!(
            contents.contains("Host *"),
            "config should add a Host * fallback block: {contents}"
        );
        assert!(
            contents.contains("ServerAliveInterval 15"),
            "config should set the keepalive interval: {contents}"
        );
        assert!(
            contents.contains("ServerAliveCountMax 4"),
            "config should set the keepalive count: {contents}"
        );
        assert!(!contents.contains("ControlMaster"));
        assert!(!contents.contains("ControlPersist"));
        assert!(!contents.contains("ControlPath"));
        // ...and any user config is Included (quoted) BEFORE it so
        // first-value-wins keeps the user's own settings.
        if let Some(home) = std::env::var_os("HOME") {
            let user_config = PathBuf::from(home).join(".ssh").join("config");
            if user_config.is_file() {
                let include = format!(
                    "Include {}",
                    ssh_config_quote(&user_config.to_string_lossy())
                );
                let include_at = contents.find(&include).expect("user config Included");
                let fallback_at = contents.find("Host *").expect("fallback present");
                assert!(
                    include_at < fallback_at,
                    "user config must be Included before herdr's fallback: {contents}"
                );
            }
        }

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, BRIDGE_SOCKET_PERMISSION_MODE,
            "keepalive config must be user-only"
        );
        // The config lives in a private 0700 dir, not a predictable temp path.
        let dir = path.parent().expect("config has a parent dir");
        let dir_mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "ssh config dir must be user-only");
        assert!(
            fits_unix_socket_path(&control_path),
            "control socket path must fit portable Unix socket limits"
        );

        drop(managed_config);
    }

    #[cfg(unix)]
    #[test]
    fn shared_ssh_transport_survives_helper_config_drop() {
        let first = write_managed_ssh_config("example").unwrap();
        let second = write_managed_ssh_config("example").unwrap();
        let socket = first.options.control_path.clone().unwrap();
        assert_eq!(Some(&socket), second.options.control_path.as_ref());
        assert_ne!(socket.parent(), first.options.config_path.parent());
        let config_path = first.options.config_path.clone();
        drop(first);
        assert!(!config_path.exists());
        assert!(socket.parent().unwrap().is_dir());
    }

    #[test]
    fn ssh_authentication_diagnostics_are_narrow() {
        for message in [
            "user@host: Permission denied (publickey).",
            "Permission denied (keyboard-interactive,password).",
            "Permission denied (password).",
            "sign_and_send_pubkey: signing failed for ED25519 from agent: agent refused operation",
        ] {
            assert!(ssh_error_requires_authentication(message), "{message}");
        }
        for message in [
            "Host key verification failed.",
            "REMOTE HOST IDENTIFICATION HAS CHANGED!",
            "Permission denied opening /tmp/file",
            "Connection refused",
            "agent disconnected",
            "Permission denied (publickey). Host key verification failed.",
        ] {
            assert!(!ssh_error_requires_authentication(message), "{message}");
        }
    }

    #[test]
    fn bridge_options_keep_temporary_config_alive_after_helper_drop() {
        let config = write_managed_ssh_config("example").unwrap();
        let path = config.options.config_path.clone();
        let worker_options = config.options.clone();
        drop(config);
        assert!(path.is_file());
        drop(worker_options);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn authentication_command_uses_shared_transport_without_askpass_or_host_key_relaxation() {
        let config = write_managed_ssh_config("example").unwrap();
        let setup = RemoteSsh::new("example".into(), true, "other-session".into(), false);
        assert_eq!(
            config.options.control_path,
            setup.options().unwrap().control_path
        );
        let authentication = authentication_command_with_config("example", config);
        let command = &authentication.command;
        assert_eq!(command.get_program(), "ssh");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>();
        for required in [
            "ControlMaster=auto",
            "ControlPersist=600",
            "BatchMode=no",
            "StrictHostKeyChecking=yes",
        ] {
            assert!(args.iter().any(|arg| arg == required), "missing {required}");
        }
        assert_eq!(&args[args.len() - 3..], &["-T", "example", "exit"]);
        let env = command.get_envs().collect::<Vec<_>>();
        assert!(env.iter().any(
            |(key, value)| *key == std::ffi::OsStr::new("SSH_ASKPASS_REQUIRE")
                && *value == Some(std::ffi::OsStr::new("never"))
        ));
        assert!(env
            .iter()
            .any(|(key, value)| *key == std::ffi::OsStr::new("SSH_ASKPASS") && value.is_none()));
    }

    #[test]
    fn authentication_command_rejects_option_injection() {
        assert_eq!(
            ssh_authentication_command("-oProxyCommand=bad")
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn unmanaged_ssh_setup_preserves_plain_transport() {
        let ssh = RemoteSsh::new("example".into(), false, "main".into(), false);
        assert!(ssh.options().is_none());
        assert!(!ssh.command().get_args().any(|arg| arg == "-F"));
    }

    #[test]
    fn ssh_config_quote_wraps_path_with_spaces() {
        assert_eq!(
            ssh_config_quote("/home/a b/.ssh/config"),
            "\"/home/a b/.ssh/config\""
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_ssh_command_uses_managed_config_when_present() {
        let managed_config = write_managed_ssh_config("example").expect("write managed config");
        let config_path = managed_config.options.config_path.clone();
        let control_path = managed_config.options.control_path.clone().unwrap();
        let ssh = RemoteSsh {
            target: "example".to_string(),
            session_name: crate::session::DEFAULT_SESSION_NAME.into(),
            managed_config: Some(managed_config),
            interactive_progress: false,
            noninteractive: false,
        };

        let command = ssh.command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "-C".to_string(),
                "-F".to_string(),
                config_path.to_string_lossy().into_owned(),
                "-S".to_string(),
                control_path.to_string_lossy().into_owned(),
                "-o".to_string(),
                "ControlMaster=auto".to_string(),
                "-o".to_string(),
                "ControlPersist=600".to_string(),
                "-T".to_string(),
                "example".to_string(),
            ]
        );

        let scp_args = ssh
            .scp_command()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            scp_args,
            vec![
                "-O".to_string(),
                "-C".to_string(),
                "-F".to_string(),
                config_path.to_string_lossy().into_owned(),
                "-o".to_string(),
                format!("ControlPath=\"{}\"", control_path.to_string_lossy()),
                "-o".to_string(),
                "ControlMaster=auto".to_string(),
                "-o".to_string(),
                "ControlPersist=600".to_string(),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_managed_ssh_config_uses_keepalives_without_control_socket() {
        let managed_config = write_managed_ssh_config("example").expect("write managed config");
        let config_path = managed_config.options.config_path.clone();
        assert!(managed_config.options.control_path.is_none());
        let contents = std::fs::read_to_string(&config_path).expect("read managed config");
        assert!(contents.contains("ServerAliveInterval 15"));
        assert!(contents.contains("ServerAliveCountMax 4"));
        // Git Bash's MSYS OpenSSH ignores drive-letter `Include` paths, so the
        // user config must be referenced through `~` for aliases to resolve.
        let include_at = contents
            .find("Include \"~/.ssh/config\"")
            .expect("user config Included through home");
        let fallback_at = contents.find("Host *").expect("fallback present");
        assert!(
            include_at < fallback_at,
            "user config must be Included before herdr's fallback: {contents}"
        );

        let ssh = RemoteSsh {
            target: "example".to_string(),
            session_name: crate::session::DEFAULT_SESSION_NAME.into(),
            managed_config: Some(managed_config),
            interactive_progress: false,
            noninteractive: false,
        };
        let args = ssh
            .command()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-C".to_string(),
                "-F".to_string(),
                config_path.to_string_lossy().into_owned(),
                "-T".to_string(),
                "example".to_string(),
            ]
        );
        let scp_args = ssh
            .scp_command()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            scp_args,
            vec![
                "-O".to_string(),
                "-C".to_string(),
                "-F".to_string(),
                config_path.to_string_lossy().into_owned(),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_ssh_config_include_uses_forward_slashes() {
        assert_eq!(
            ssh_config_include_path(Path::new(r"C:\Users\A B\.ssh\config")),
            r#""C:/Users/A B/.ssh/config""#
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_ssh_auth_output_arrives_before_exit_and_remains_captured() {
        use std::sync::mpsc;

        struct NoticeWriter(mpsc::Sender<Vec<u8>>);
        impl io::Write for NoticeWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.send(bytes.to_vec()).map_err(io::Error::other)?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let job = crate::platform::ChildProcessJob::new_kill_on_close().unwrap();
        let mut command = Command::new("cmd.exe");
        command
            .args([
                "/D",
                "/C",
                "(echo auth-notice 1>&2) & set /p approval= & echo reply & exit /b 23",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::platform::configure_background_command(&mut command);
        let mut child = command.spawn().unwrap();
        if let Err(error) = job.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("contain authentication fixture: {error}");
        }
        let mut approval = child.stdin.take().unwrap();
        let (notice_tx, notice_rx) = mpsc::channel();
        let (output_tx, output_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = output_tx.send(output_with_forwarded_stderr(
                child,
                None,
                NoticeWriter(notice_tx),
            ));
        });
        // The child cannot exit until the test receives the relayed notice and
        // releases its stdin. Capturing stderr only after exit fails this check.
        let notice = notice_rx.recv_timeout(Duration::from_secs(5));
        let approval_result = approval.write_all(b"approved\r\n");
        drop(approval);
        let output = output_rx.recv_timeout(Duration::from_secs(5));
        if output.is_err() {
            job.terminate().unwrap();
        }
        worker.join().unwrap();
        let output = output.unwrap().unwrap();
        approval_result.unwrap();
        assert!(String::from_utf8_lossy(&notice.unwrap()).contains("auth-notice"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("auth-notice"));
        assert!(String::from_utf8_lossy(&output.stdout).contains("reply"));
        assert_eq!(output.status.code(), Some(23));
    }

    #[cfg(windows)]
    #[test]
    fn windows_ssh_user_config_include_uses_home_shorthand() {
        // MSYS/Git Bash OpenSSH does not resolve `C:/...` in `Include`, so the
        // user config is referenced through `~` regardless of the profile path.
        assert_eq!(
            ssh_user_config_include(Some(Path::new(r"C:\Users\A B\.ssh\config"))),
            Some(r#""~/.ssh/config""#.to_string())
        );
        assert_eq!(
            ssh_user_config_include(None),
            Some(r#""~/.ssh/config""#.to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_probe_preserves_setup_ssh_options() {
        let managed_config = write_managed_ssh_config("example").expect("write managed config");
        let marker = managed_config
            .options
            .config_path
            .with_file_name("probe-ran");
        fs::write(
            &managed_config.options.config_path,
            format!(
                "Host *\n  ProxyCommand /bin/sh -c {}\n",
                shell_quote(&format!(
                    ": > {}; exit 1",
                    shell_quote(&marker.to_string_lossy())
                ))
            ),
        )
        .expect("write isolated probe config");
        let ssh = RemoteSsh {
            target: "herdr-probe.invalid".into(),
            session_name: "probe-options".into(),
            managed_config: Some(managed_config),
            noninteractive: false,
            interactive_progress: false,
        };
        let mut remote = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        remote.client = Some(RemoteClientStatusJson {
            binary: None,
            version: None,
            protocol: None,
            endpoint_protocol_generation: Some(
                crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
            ),
            endpoint_capabilities: vec![
                crate::protocol::endpoint::REMOTE_CONNECT_ONLY_CAPABILITY.into()
            ],
            remote_bridge_idle_timeout: false,
        });

        let error = probe_remote_endpoint(&ssh, &remote).expect_err("proxy refuses connection");

        assert!(
            marker.exists(),
            "endpoint probe discarded the authenticated setup's SSH options: {error}"
        );
    }

    #[test]
    fn noninteractive_ssh_stderr_capture_is_bounded() {
        let stderr = vec![b'x'; NONINTERACTIVE_SSH_STDERR_LIMIT + 4096];
        let captured = capture_ssh_stderr(stderr.as_slice()).expect("capture stderr");
        assert_eq!(captured.len(), NONINTERACTIVE_SSH_STDERR_LIMIT);
    }

    #[test]
    fn noninteractive_ssh_command_cannot_prompt_or_accept_unknown_hosts() {
        let ssh = RemoteSsh::new_noninteractive("example".into(), "named-session".into());
        let args = ssh
            .command()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for required in [
            "-C",
            "BatchMode=yes",
            "NumberOfPasswordPrompts=0",
            "StrictHostKeyChecking=yes",
            "ConnectTimeout=10",
            "ConnectionAttempts=1",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=4",
        ] {
            assert!(args.iter().any(|arg| arg == required), "missing {required}");
        }
        assert_eq!(args.iter().any(|arg| arg == "-F"), ssh.options().is_some());
    }

    #[test]
    fn remote_setup_approval_requires_input_and_rejects_unrecognized_answers() {
        for default in [false, true] {
            for input in ["", "maybe\n"] {
                assert_eq!(
                    read_remote_confirmation(&mut input.as_bytes(), default)
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::Interrupted
                );
            }
            assert_eq!(
                read_remote_confirmation(&mut "\n".as_bytes(), default).unwrap(),
                default
            );
            assert!(read_remote_confirmation(&mut "YES\n".as_bytes(), default).unwrap());
            assert!(!read_remote_confirmation(&mut "no\n".as_bytes(), default).unwrap());
        }
    }

    #[test]
    fn saved_machine_compatibility_uses_capabilities_not_release_or_private_protocol() {
        let mut status = RemoteClientStatusJson {
            binary: None,
            version: Some("0.1.0".into()),
            protocol: Some(1),
            endpoint_protocol_generation: Some(
                crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
            ),
            endpoint_capabilities: vec![
                crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
                crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
                crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY.into(),
                crate::protocol::endpoint::REMOTE_CONNECT_ONLY_CAPABILITY.into(),
            ],
            remote_bridge_idle_timeout: false,
        };
        assert!(status.supports_endpoint_requirement(true));
        for index in 0..status.endpoint_capabilities.len() {
            let removed = status.endpoint_capabilities.remove(index);
            assert!(!status.supports_endpoint_requirement(true));
            assert!(status.supports_endpoint_requirement(false));
            status.endpoint_capabilities.insert(index, removed);
        }
        status.endpoint_protocol_generation = None;
        assert!(!status.supports_endpoint_requirement(true));
    }

    #[test]
    fn saved_machine_server_commands_are_scoped_to_the_explicit_session() {
        let herdr =
            RemoteHerdr::for_platform(RemotePlatform::from_uname("Linux", "x86_64").unwrap());
        for command in [
            "status server --json",
            "server stop",
            "remote-client-bridge",
        ] {
            assert_eq!(
                remote_session_command(&herdr, "agents", command),
                format!("{} --session agents {command}", herdr.shell_path)
            );
            assert_eq!(
                remote_session_command(&herdr, crate::session::DEFAULT_SESSION_NAME, command),
                format!("{} --session default {command}", herdr.shell_path)
            );
        }
    }

    #[test]
    fn remote_ssh_commands_compress_without_managed_config() {
        let ssh = RemoteSsh {
            target: "example".to_string(),
            session_name: crate::session::DEFAULT_SESSION_NAME.into(),
            managed_config: None,
            interactive_progress: false,
            noninteractive: false,
        };

        let command = ssh.command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(args, vec!["-C", "-T", "example"]);
        assert_eq!(
            ssh.scp_command().get_args().collect::<Vec<_>>(),
            vec!["-O", "-C"]
        );
    }

    #[test]
    fn remote_install_stream_command_avoids_shell_c_wrapper() {
        let command = remote_install_stream_command("/home/a b/.local/bin/herdr.tmp.123");

        assert_eq!(command, "tee '/home/a b/.local/bin/herdr.tmp.123'");
    }

    #[test]
    fn remote_install_prepare_and_commit_scripts_quote_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let prepare = remote_install_prepare_script(&remote_herdr);

        assert!(prepare.contains("mkdir -p \"$dir\""));
        assert!(prepare.contains("printf '%s\\0%s\\0' \"$tmp\" \"$dest\""));
        assert_eq!(
            parse_remote_install_paths(b"/home/a b/herdr.tmp.42\0/home/a b/herdr\0").unwrap(),
            (
                "/home/a b/herdr.tmp.42".to_string(),
                "/home/a b/herdr".to_string()
            )
        );
        assert_eq!(
            parse_remote_install_paths(b"/home/a b\n/herdr.tmp.42\0/home/a b\n/herdr\0").unwrap(),
            (
                "/home/a b\n/herdr.tmp.42".to_string(),
                "/home/a b\n/herdr".to_string()
            )
        );
        assert_eq!(
            remote_install_commit_script("/home/a b/herdr.tmp.42", "/home/a b/herdr"),
            "set -eu\nchmod 755 '/home/a b/herdr.tmp.42'\nmv '/home/a b/herdr.tmp.42' '/home/a b/herdr'\n"
        );
    }

    #[test]
    fn extract_remote_args_removes_space_form() {
        let args = vec![
            "herdr".into(),
            "--remote".into(),
            "dev".into(),
            "--help".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr", "--help"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn remote_progress_is_interactive_and_never_json() {
        assert!(remote_progress_enabled(false, true, true));
        assert!(!remote_progress_enabled(true, true, true));
        assert!(!remote_progress_enabled(false, false, true));
        assert!(!remote_progress_enabled(false, true, false));
    }

    #[test]
    fn extract_remote_args_removes_equals_form() {
        let args = vec!["herdr".into(), "--remote=user@host".into()];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "user@host");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_server() {
        let args = vec![
            "herdr".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_space_form() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote-keybindings".into(),
            "server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        assert_eq!(remote.unwrap().keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_explicit_handoff() {
        let args = vec!["herdr".into(), "--remote=dev".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert!(remote.live_handoff);
    }

    #[test]
    fn extract_remote_args_accepts_explicit_provision_contract() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--provision".into(),
            "--yes".into(),
            "--json".into(),
        ];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert!(remote.provision);
        assert!(remote.yes);
        assert!(remote.json);
        assert!(!remote.live_handoff);
    }

    #[test]
    fn extract_remote_args_allows_yes_for_one_attach_and_reserves_json_for_provision() {
        let args = vec!["herdr".into(), "--remote=dev".into(), "--yes".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert!(remote.yes);
        assert!(!remote.provision);
        assert!(!remote.json);

        let args = vec!["herdr".into(), "--remote=dev".into(), "--json".into()];
        assert_eq!(
            extract_remote_args(&args).unwrap_err(),
            "--json requires --remote with --provision"
        );
    }

    #[test]
    fn extract_remote_args_preserves_child_remote_options_after_separator() {
        let args = vec![
            "herdr".into(),
            "agent".into(),
            "start".into(),
            "repro".into(),
            "--".into(),
            "child".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
            "--handoff".into(),
        ];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_preserves_handoff_without_remote() {
        let args = vec!["herdr".into(), "update".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_rejects_remote_keybindings_without_remote() {
        let args = vec!["herdr".into(), "--remote-keybindings=server".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings requires --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_remote_keybindings() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote-keybindings=local".into(),
            "--remote-keybindings=server".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings can only be specified once");
    }

    #[test]
    fn extract_remote_args_requires_value() {
        let args = vec!["herdr".into(), "--remote".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_empty_value() {
        let args = vec!["herdr".into(), "--remote=".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_values() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote=prod".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote can only be specified once");
    }

    #[test]
    fn extract_remote_args_rejects_option_like_target() {
        let args = vec!["herdr".into(), "--remote".into(), "-oProxyCommand=x".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote target must not start with '-'");
    }

    #[test]
    fn sanitize_path_component_removes_shell_sensitive_chars() {
        assert_eq!(sanitize_path_component("user@host:22"), "user-host-22");
    }

    #[test]
    fn remote_platform_maps_uname_values() {
        assert_eq!(
            RemotePlatform::from_uname("Linux", "amd64")
                .unwrap()
                .asset_key(),
            "linux-x86_64"
        );
        assert_eq!(
            RemotePlatform::from_uname("Darwin", "arm64")
                .unwrap()
                .asset_key(),
            "macos-aarch64"
        );
        assert!(RemotePlatform::from_uname("FreeBSD", "x86_64").is_none());
    }

    #[test]
    fn machine_metadata_keeps_raw_resolved_paths_not_shell_expressions() {
        let remote = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert!(remote.machine_metadata().is_none());
        let path = "/home/user's files/$literal/herdr";
        let mut resolved = remote.with_posix_path(path);
        resolved.client = Some(
            parse_client_status_json(
                &serde_json::json!({
                    "binary": path, "endpoint_protocol_generation": 1
                })
                .to_string(),
            )
            .unwrap(),
        );
        assert_eq!(resolved.machine_metadata().unwrap().executable, path);
        assert_eq!(resolved.shell_path, shell_quote(path));
        let mut remote = RemoteHerdr::for_windows(
            RemotePlatform {
                os: "windows",
                arch: "x86_64",
            },
            r"C:\Users\A B",
            None,
            super::super::windows::WindowsSshShell::Pwsh,
        );
        assert!(remote.machine_metadata().is_none());
        let path = r"C:\Users\A B\herdr.exe";
        remote.client = Some(
            parse_client_status_json(
                &serde_json::json!({
                    "binary": path, "endpoint_protocol_generation": 1
                })
                .to_string(),
            )
            .unwrap(),
        );
        assert_eq!(remote.machine_metadata().unwrap().executable, path);
    }

    #[test]
    fn cached_windows_api_command_checks_before_starting_the_stream() {
        let path = r"C:\Users\A'B\herdr.exe";
        let command = cached_remote_api_command(
            &crate::client::endpoint::SshMachineMetadata {
                os: "windows".into(),
                executable: path.into(),
                windows: Some(crate::client::endpoint::WindowsSshMetadata {
                    shell: super::super::windows::WindowsSshShell::Pwsh,
                    sidecar: true,
                }),
            },
            "fleet",
        )
        .unwrap();
        let (probe, stream) = command.split_once("; if ($LASTEXITCODE").unwrap();
        let script = decode_windows_command(probe);
        assert!(script.contains(&crate::platform::quote_powershell_arg(path)));
        assert!(script.contains(STALE_API_METADATA));
        assert!(script.contains("'--session' 'fleet' 'remote-api-bridge' --check"));
        assert!(script.contains("$LASTEXITCODE -eq 0"));
        assert!(stream.contains("-ne 0) { exit $LASTEXITCODE }"));
        assert!(stream.contains(REMOTE_OUTPUT_READY_MARKER));
        assert!(stream.contains("& $herdr '--session' 'fleet' 'remote-api-bridge'"));
        assert!(stream.contains("HERDR_REMOTE_SIDECAR_V1"));
        assert!(!stream.contains("Start-Process"));
    }

    #[test]
    fn remote_output_framing_discards_any_banner_and_preserves_binary() {
        let payload = [0, 1, 2, 0xff, b'\n'];
        let mut input = vec![b'x'; 4 * 1024 * 1024];
        input.extend_from_slice(b"\r\nherdr-remote-output-ready:1\r\n");
        input.extend_from_slice(&payload);
        let mut reader = io::BufReader::with_capacity(17, io::Cursor::new(input));

        discard_remote_output_preamble(&mut reader).unwrap();
        let mut output = Vec::new();
        io::Read::read_to_end(&mut reader, &mut output).unwrap();
        assert_eq!(output, payload);

        let mut missing = b"profile output without marker".to_vec();
        assert!(normalize_remote_stdout(&mut missing, true).is_err());
        normalize_remote_stdout(&mut missing, false).unwrap();
        assert_eq!(missing, b"profile output without marker");

        let mut platform = b"profile output\nherdr-remote-output-ready:1\nLinux\nx86_64\n".to_vec();
        normalize_remote_stdout(&mut platform, true).unwrap();
        let platform = String::from_utf8(platform).unwrap();
        let mut lines = platform.lines();
        assert_eq!(
            RemotePlatform::from_uname(lines.next().unwrap(), lines.next().unwrap()),
            Some(RemotePlatform {
                os: "linux",
                arch: "x86_64"
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn reattach_command_includes_remote_and_session() {
        assert_eq!(
            reattach_command(
                "target/release/herdr",
                "user@host",
                "work",
                RemoteKeybindings::Local,
                false,
            ),
            "target/release/herdr --remote user@host --session work"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host name",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                false,
            ),
            "herdr --remote 'host name'"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Server,
                false,
            ),
            "herdr --remote host --remote-keybindings server"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                true,
            ),
            "herdr --remote host --handoff"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_reattach_command_uses_current_executable() {
        let executable = std::env::current_exe().expect("current test executable");
        assert_eq!(
            reattach_command(
                r"C:\Program Files\Herdr\herdr.exe",
                "host'name",
                "work'name",
                RemoteKeybindings::Local,
                false,
            ),
            format!(
                "& '{}' --remote 'host''name' --session 'work''name'",
                executable.display().to_string().replace('\'', "''")
            )
        );
    }

    #[test]
    fn remote_api_bridge_always_selects_the_saved_session() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        for session in ["default", "agents"] {
            assert_eq!(
                remote_api_bridge_command(&remote_herdr, session, false).unwrap(),
                posix_remote_output_command(&format!(
                    "exec \"$HOME/.local/bin/herdr\" --session {session} remote-api-bridge"
                ))
            );
        }
    }

    #[test]
    fn remote_bridge_idle_timeout_requires_explicit_support_and_opt_in() {
        let legacy = parse_client_status_json(r#"{"endpoint_protocol_generation":1}"#).unwrap();
        assert!(!legacy.remote_bridge_idle_timeout);
        let current = parse_client_status_json(
            r#"{"endpoint_protocol_generation":1,"remote_bridge_idle_timeout":true}"#,
        )
        .unwrap();
        assert!(current.remote_bridge_idle_timeout);
        let mut remote = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert!(!remote_bridge_command(&remote, "agents", false)
            .unwrap()
            .contains("--idle-timeout-v1"));
        assert!(remote_bridge_command(&remote, "agents", true).is_err());
        let mut current = current;
        current
            .endpoint_capabilities
            .push(crate::protocol::endpoint::REMOTE_CONNECT_ONLY_CAPABILITY.into());
        remote.client = Some(current);
        assert!(!remote_bridge_command(&remote, "agents", false)
            .unwrap()
            .contains("--idle-timeout-v1"));
        assert!(remote_bridge_command(&remote, "agents", true)
            .unwrap()
            .ends_with(" --session agents remote-client-bridge --connect-only --idle-timeout-v1"));
    }

    #[test]
    fn remote_bridge_command_uses_installed_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME, false)
                .unwrap(),
            "printf '\n%s\n' 'herdr-remote-output-ready:1'\nexec \"$HOME/.local/bin/herdr\" --session default remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_uses_path_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "/usr/bin/herdr\n")
            .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME, false)
                .unwrap(),
            "printf '\n%s\n' 'herdr-remote-output-ready:1'\nexec /usr/bin/herdr --session default remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_quotes_discovered_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/herdr bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME, false)
                .unwrap(),
            "printf '\n%s\n' 'herdr-remote-output-ready:1'\nexec '/opt/herdr bin/herdr' --session default remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_uses_macos_path_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "macos",
            arch: "aarch64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/homebrew/bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME, false)
                .unwrap(),
            "printf '\n%s\n' 'herdr-remote-output-ready:1'\nexec /opt/homebrew/bin/herdr --session default remote-client-bridge"
        );
        assert_eq!(remote_herdr.platform.asset_key(), "macos-aarch64");
    }

    #[test]
    fn remote_path_discovery_reads_multiple_absolute_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let candidates = remote_herdrs_from_path_discovery(
            &remote_herdr,
            "/usr/bin/herdr\nbin/herdr\n /opt/herdr bin/herdr\n",
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].shell_path, "/usr/bin/herdr");
        assert_eq!(candidates[1].shell_path, "'/opt/herdr bin/herdr'");
    }

    #[test]
    fn remote_path_discovery_ignores_mise_shims() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let candidates = remote_herdrs_from_path_discovery(
            &remote_herdr,
            "/home/can/.local/share/mise/shims/herdr\n/home/can/.local/share/mise/installs/herdr/0.7.1/bin/herdr\n",
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].shell_path,
            "/home/can/.local/share/mise/installs/herdr/0.7.1/bin/herdr"
        );
    }

    #[test]
    fn known_remote_binary_candidate_script_includes_mise_and_nix_paths() {
        let script = known_remote_binary_candidate_script(&RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });

        assert!(script.contains("emit \"$home/.local/bin/herdr\""));
        assert!(!script.contains("mise/shims/herdr"));
        assert!(script.contains(&format!("version={}", shell_quote(&current_version()))));
        assert!(
            script.contains("emit \"$home/.local/share/mise/installs/herdr/$version/bin/herdr\"")
        );
        assert!(script.contains("emit \"$home/.local/share/mise/installs/herdr/$version/herdr\""));
        assert!(script.contains(
            "emit \"$home/.local/share/mise/installs/github-ogulcancelik-herdr/$version/herdr\""
        ));
        assert!(script.contains("emit \"$home/.nix-profile/bin/herdr\""));
        assert!(script.contains("emit \"/etc/profiles/per-user/$user/bin/herdr\""));
        assert!(script.contains("emit \"/run/current-system/sw/bin/herdr\""));
        assert!(script.contains("emit \"/home/linuxbrew/.linuxbrew/bin/herdr\""));
        assert!(!script.contains("emit \"/opt/homebrew/bin/herdr\""));
    }

    #[test]
    fn known_remote_binary_candidate_script_includes_macos_homebrew_paths() {
        let script = known_remote_binary_candidate_script(&RemotePlatform {
            os: "macos",
            arch: "aarch64",
        });

        assert!(script.contains("emit \"/opt/homebrew/bin/herdr\""));
        assert!(script.contains("emit \"/usr/local/bin/herdr\""));
        assert!(!script.contains("emit \"/home/linuxbrew/.linuxbrew/bin/herdr\""));
    }

    #[test]
    fn remote_path_discovery_quotes_single_quotes_in_discovered_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/herdr's/bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME, false)
                .unwrap(),
            "printf '\n%s\n' 'herdr-remote-output-ready:1'\nexec '/opt/herdr'\\''s/bin/herdr' --session default remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_ignores_relative_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "bin/herdr\n");

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn remote_path_discovery_ignores_empty_output() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "\n");

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn remote_shell_path_warning_accepts_managed_install() {
        assert!(remote_shell_resolves_managed_install(
            "/home/can/.local/bin/herdr\n"
        ));
        assert!(remote_shell_resolves_managed_install(
            "/Users/can/.local/bin/herdr\n"
        ));
        assert!(!remote_shell_resolves_managed_install(
            "/usr/local/bin/herdr\n"
        ));
        assert!(!remote_shell_resolves_managed_install(""));
    }

    #[test]
    fn parse_client_status_json_reads_last_json_record() {
        let status = parse_client_status_json(
            "wrapper output\n{\"version\":\"0.8.0\",\"protocol\":20,\"endpoint_protocol_generation\":1,\"endpoint_capabilities\":[\"surface_interest\",\"health_check\"]}\n{\"wrapper\":true}\n",
        )
        .unwrap();
        assert_eq!(status.version.as_deref(), Some("0.8.0"));
        assert_eq!(status.protocol, Some(20));
        assert_eq!(status.endpoint_protocol_generation, Some(1));
        assert_eq!(
            status.endpoint_capabilities,
            vec!["surface_interest", "health_check"]
        );
        assert!(
            parse_client_status_json(r#"{"endpoint_protocol_generation":"unknown"}"#).is_none()
        );
    }

    #[test]
    fn saved_machine_setup_handoffs_old_server_missing_presentation_fence() {
        // Installed broker capabilities cannot substitute for the running server's negotiation.
        let installed = parse_client_status_json(
            r#"{"version":"0.8.2","protocol":22,"endpoint_protocol_generation":1,"endpoint_capabilities":["surface_interest","presentation_effects_fence","health_check","remote_connect_only"]}"#,
        )
        .unwrap();
        let running_binary = parse_client_status_json(
            r#"{"version":"0.8.2","protocol":22,"endpoint_protocol_generation":1,"endpoint_capabilities":["surface_interest","health_check","remote_connect_only"]}"#,
        )
        .unwrap();
        assert!(installed.supports_endpoint_requirement(true));
        assert!(!running_binary.supports_endpoint_requirement(true));
        for (live_capabilities, expected) in [
            (
                running_binary.endpoint_capabilities,
                RemoteInstallRunningServerPlan::LiveHandoff,
            ),
            (
                installed.endpoint_capabilities,
                RemoteInstallRunningServerPlan::KeepRunning,
            ),
        ] {
            let live_negotiation = crate::client::endpoint::EndpointNegotiation::new(
                vec!["client_shell.surface.set".into()],
                live_capabilities,
            );
            let RemoteServerStatus::Running {
                endpoint_protocol_generation,
                surface_interest,
                health_check,
                live_handoff,
                detached_server_daemon,
                ..
            } = parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.8.2","protocol":22,"capabilities":{"live_handoff":true,"detached_server_daemon":true,"endpoint_protocol_generation":1,"surface_interest":true,"health_check":true}}"#,
            )
            .unwrap()
            .with_endpoint_negotiation(&live_negotiation) else {
                panic!("captured server must be running");
            };

            assert_eq!(
                remote_install_running_server_plan(
                    endpoint_protocol_generation,
                    detached_server_daemon,
                    surface_interest,
                    health_check,
                    live_handoff,
                    true,
                    true,
                ),
                expected,
                "setup must follow the running server's negotiated capabilities",
            );
        }
    }

    #[test]
    fn parse_remote_server_status_json_reads_running_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8,"capabilities":{"live_handoff":true,"detached_server_daemon":true,"endpoint_protocol_generation":1,"surface_interest":true,"health_check":true}}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                protocol: Some(8),
                binary: None,
                endpoint_protocol_generation: Some(1),
                surface_interest: true,
                health_check: true,
                live_handoff: true,
                detached_server_daemon: true
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_treats_missing_capability_as_old_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                protocol: Some(8),
                binary: None,
                endpoint_protocol_generation: None,
                surface_interest: false,
                health_check: false,
                live_handoff: false,
                detached_server_daemon: false
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_reads_stopped_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"not_running","running":false,"version":null,"protocol":null}"#
            )
            .unwrap(),
            RemoteServerStatus::NotRunning
        );
    }

    #[test]
    fn remote_preview_manifest_falls_back_to_archived_exact_build_assets() {
        let mut manifest: RemotePreviewManifest = serde_json::from_str(
            r#"{
                "prerelease": false,
                "build_id": "2026-06-06-new",
                "protocol": 12,
                "assets": {
                    "linux-x86_64": {
                        "url": "https://example.com/new",
                        "sha256": "new"
                    }
                },
                "builds": {
                    "2026-06-02-old": {
                        "protocol": 11,
                        "assets": {
                            "linux-x86_64": {
                                "url": "https://example.com/old",
                                "sha256": "old"
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let (protocol, assets) =
            preview_assets_for_build(&manifest, "2026-06-02-old").expect("archived build");
        let asset = assets.get("linux-x86_64").expect("asset");
        assert_eq!(protocol, 11);
        assert_eq!(asset.url(), "https://example.com/old");
        assert_eq!(asset.sha256(), Some("old"));

        manifest.prerelease = true;
        assert!(preview_assets_for_build(&manifest, "2026-06-02-old").is_err());
    }

    #[test]
    fn install_source_description_uses_override_binary() {
        let platform = RemotePlatform {
            os: "linux",
            arch: "aarch64",
        };
        assert_eq!(
            install_source_description_for(&platform, Some(Path::new("/tmp/herdr-aarch64")), false),
            "HERDR_REMOTE_BINARY (/tmp/herdr-aarch64)"
        );
    }

    #[test]
    fn explicit_windows_payload_prevents_reusing_a_matching_remote_binary() {
        let mut detected = DetectedWindowsHerdr {
            remote_herdr: RemoteHerdr::for_windows(
                RemotePlatform {
                    os: "windows",
                    arch: "x86_64",
                },
                r"C:\Users\dev",
                None,
                super::super::windows::WindowsSshShell::Cmd,
            ),
            server_status: RemoteServerStatus::NotRunning,
            matches_current: true,
        };
        detected.remote_herdr.client = Some(RemoteClientStatusJson {
            remote_bridge_idle_timeout: false,
            binary: Some(r"C:\Users\dev\.herdr\remote\herdr.exe".into()),
            version: Some(current_version()),
            protocol: Some(CURRENT_PROTOCOL),
            endpoint_protocol_generation: Some(
                crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
            ),
            endpoint_capabilities: vec![
                crate::protocol::endpoint::WINDOWS_REMOTE_HOST_CAPABILITY.into()
            ],
        });
        assert!(can_reuse_detected_windows_herdr(
            &detected, None, false, true
        ));
        assert!(!can_reuse_detected_windows_herdr(
            &detected,
            Some(Path::new("payload.zip")),
            false,
            false,
        ));
        detected.remote_herdr.client.as_mut().unwrap().version = Some("older-compatible".into());
        detected.matches_current = false;
        assert!(can_reuse_detected_windows_herdr(
            &detected, None, false, false
        ));
        assert!(!can_reuse_detected_windows_herdr(
            &detected, None, false, true
        ));
        assert!(!detected
            .remote_herdr
            .client
            .as_ref()
            .unwrap()
            .matches_deployment_identity());
        detected
            .remote_herdr
            .client
            .as_mut()
            .unwrap()
            .endpoint_capabilities
            .clear();
        assert!(!can_reuse_detected_windows_herdr(
            &detected, None, false, false
        ));
    }

    #[test]
    fn windows_replacement_requires_stop_consent_even_for_compatible_servers() {
        let status = RemoteServerStatus::Running {
            version: Some("older-compatible".into()),
            protocol: Some(CURRENT_PROTOCOL - 1),
            binary: Some(r"C:\Users\dev\.herdr\remote\herdr.exe".into()),
            endpoint_protocol_generation: Some(
                crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
            ),
            surface_interest: true,
            health_check: true,
            live_handoff: false,
            detached_server_daemon: true,
        };
        assert_eq!(
            approve_windows_replacement(&status, false, || Ok(false))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(approve_windows_replacement(&status, false, || Ok(true)).unwrap());
        assert!(
            approve_windows_replacement(&status, true, || panic!("--yes must not prompt")).unwrap()
        );
        assert!(
            !approve_windows_replacement(&RemoteServerStatus::NotRunning, false, || panic!(
                "nothing to stop"
            ))
            .unwrap()
        );
    }

    #[test]
    fn saved_windows_commands_preserve_session_shell_and_sidecar() {
        for session in ["saved-work", crate::session::DEFAULT_SESSION_NAME] {
            let ssh = RemoteSsh::new_noninteractive("host".into(), session.into());
            assert!(ssh.command().get_args().any(|arg| arg == "BatchMode=yes"));
            for shell in [
                super::super::windows::WindowsSshShell::Cmd,
                super::super::windows::WindowsSshShell::Pwsh,
            ] {
                let mut remote = RemoteHerdr::for_windows(
                    RemotePlatform::windows("AMD64").unwrap(),
                    r"C:\Users\dev",
                    None,
                    shell,
                );
                assert!(remote_bridge_command(&remote, &ssh.session_name, true).is_err());
                remote.client = Some(RemoteClientStatusJson {
                    remote_bridge_idle_timeout: false,
                    binary: Some(remote.shell_path.clone()),
                    version: Some(current_version()),
                    protocol: Some(CURRENT_PROTOCOL),
                    endpoint_protocol_generation: Some(
                        crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
                    ),
                    endpoint_capabilities: vec![
                        crate::protocol::endpoint::REMOTE_CONNECT_ONLY_CAPABILITY.into(),
                    ],
                });
                let bridge = remote_bridge_command(&remote, &ssh.session_name, true).unwrap();
                assert!(bridge.contains("--connect-only"));
                assert!(bridge.contains(session));
                assert!(bridge.contains("--session"));
                assert!(bridge.contains("remote-client-bridge"));
                assert!(bridge.contains("HERDR_REMOTE_SIDECAR_V1"));
                assert!(!bridge.contains("/bin/sh") && !bridge.contains("/dev/null"));
                let install = windows_install_script(
                    &remote,
                    r"C:\Users\dev\.herdr\payload.zip",
                    &"a".repeat(64),
                    Some(&remote),
                    &ssh.session_name,
                );
                assert!(install.contains(&format!("-SessionName '{session}'")));
                assert!(
                    install.contains("-ExistingHerdr 'C:\\Users\\dev\\.herdr\\remote\\herdr.exe'")
                );
                assert!(install.contains("-ExistingSidecar $true"));
            }
        }
    }

    #[test]
    fn install_source_description_uses_local_binary_when_allowed() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, true),
            "the current local herdr binary"
        );
    }

    #[test]
    fn install_source_description_uses_release_asset_when_local_binary_cannot_seed_remote() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, false),
            format!(
                "the {} {} asset for {}",
                current_version(),
                current_channel(),
                platform.asset_key()
            )
        );
    }

    #[test]
    fn resolve_install_source_uses_override_binary_without_temporary_cleanup() {
        let platform = RemotePlatform {
            os: "linux",
            arch: "aarch64",
        };
        let source = resolve_install_source(&platform, Some(PathBuf::from("/tmp/herdr-aarch64")))
            .expect("override source");
        assert_eq!(source.path, PathBuf::from("/tmp/herdr-aarch64"));
        assert!(source.temporary_dir.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_local_forward_endpoint_uses_private_state_dir() {
        let path = local_forward_socket_path("user@example.com", "work");
        assert!(path.starts_with(crate::platform::remote_private_temp_base()));
        assert!(path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("herdr-r-")));
    }

    #[cfg(unix)]
    fn remote_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[cfg(unix)]
    fn socket_path_byte_len(path: &Path) -> usize {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    }

    #[cfg(unix)]
    #[test]
    fn local_forward_socket_path_uses_readable_name_when_it_fits() {
        let _guard = remote_env_lock().lock().unwrap();
        // Short target + session leave plenty of room — keep the human-
        // readable form so the socket path stays grep-friendly.
        let path = local_forward_socket_path("dev", "default");
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        assert!(
            filename.starts_with("herdr-remote-"),
            "expected readable name, got {filename}"
        );
        assert!(filename.contains("-dev-default."), "got {filename}");
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_forward_socket_path_fits_in_sun_path() {
        let _guard = remote_env_lock().lock().unwrap();
        // Worst case for the readable form: macOS-style 49-char TMPDIR +
        // max-length sanitized components. Should fall back to the hashed
        // short name, which fits under TMPDIR.
        let target = "longish-host.example.com";
        let session = "a-fairly-long-session-name-here";
        let path = local_forward_socket_path(target, session);
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long for sun_path: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_forward_socket_path_falls_back_to_tmp_when_dir_is_long() {
        let _guard = remote_env_lock().lock().unwrap();
        // Force a TMPDIR long enough that even the hashed short name cannot
        // fit inside it. The fallback should drop to /tmp.
        let prior = std::env::var_os("TMPDIR");
        let long_dir = std::env::temp_dir().join("a".repeat(80));
        let _ = fs::create_dir_all(&long_dir);
        std::env::set_var("TMPDIR", &long_dir);

        let path = local_forward_socket_path("longish-host.example.com", "default");
        let fits = fits_unix_socket_path(&path);
        let parent = path.parent().map(Path::to_path_buf);
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        match prior {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        }
        let _ = fs::remove_dir_all(&long_dir);

        assert!(fits, "fallback path still overflows: {}", path.display());
        assert_eq!(parent.as_deref(), Some(Path::new("/tmp")));
        assert!(
            filename.starts_with("herdr-r-"),
            "expected hashed fallback, got {filename}"
        );
    }

    #[test]
    fn install_source_cleanup_removes_temporary_directory() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-install-source-cleanup-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).expect("create temp dir");
        let path = dir.join("herdr.tmp");
        fs::write(&path, b"test").expect("write temp file");

        InstallSource::temporary(path, dir.clone()).cleanup();

        assert!(!dir.exists());
    }
}
