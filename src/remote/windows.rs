//! Windows remote-host side of the SSH stdio bridge.

#[cfg(windows)]
use std::{
    io,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use base64::Engine as _;
#[cfg(windows)]
use interprocess::local_socket::traits::Stream as _;

const WINDOWS_REMOTE_SIDECAR_ENV: &str = "HERDR_REMOTE_SIDECAR_V1";
const WINDOWS_ATTACH_PROBE_SCRIPT: &str = include_str!("windows_attach_probe.ps1");
const WINDOWS_BOOTSTRAP_SCRIPT: &str = include_str!("windows_bootstrap.ps1");
const WINDOWS_PAYLOAD_LAYOUT_SCRIPT: &str = include_str!("windows_payload_layout.ps1");
const WINDOWS_PACKAGE_LOCAL_PAYLOAD_SCRIPT: &str =
    include_str!("windows_package_local_payload.ps1");
pub(crate) const REMOTE_SIDECAR_VALIDATE_ARG: &str = "--herdr-private-validate-remote-sidecar-v1";
static REMOTE_SIDECAR_ACTIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WindowsSshShell {
    Cmd,
    Pwsh,
    WindowsPowerShell,
    Unsupported(String),
}

impl WindowsSshShell {
    pub(super) fn from_default_shell(default_shell: &str) -> Self {
        let executable = default_shell
            .trim()
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or_default();
        if executable.eq_ignore_ascii_case("cmd.exe") || executable.eq_ignore_ascii_case("cmd") {
            Self::Cmd
        } else if executable.eq_ignore_ascii_case("pwsh.exe")
            || executable.eq_ignore_ascii_case("pwsh")
        {
            Self::Pwsh
        } else if executable.eq_ignore_ascii_case("powershell.exe")
            || executable.eq_ignore_ascii_case("powershell")
        {
            Self::WindowsPowerShell
        } else {
            Self::Unsupported(default_shell.trim().to_string())
        }
    }
}

#[cfg(windows)]
pub(crate) fn run_remote_client_bridge(allow_start: bool) -> io::Result<()> {
    if allow_start {
        ensure_remote_server_running()?;
    }

    let socket_path = crate::server::socket_paths::client_socket_path();
    let stream = crate::ipc::connect_local_stream(&socket_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to connect to remote Herdr client socket {}: {err}",
                socket_path.display()
            ),
        )
    })?;
    let (mut socket_to_stdout, mut stdin_to_socket) = stream.split();
    let _download = thread::spawn(move || {
        let mut stdout = io::stdout().lock();
        let _ = copy_flush(&mut socket_to_stdout, &mut stdout);
    });

    let mut stdin = io::stdin();
    copy_flush(&mut stdin, &mut stdin_to_socket).map(|_| ())
}

#[cfg(windows)]
pub(crate) fn adopt_remote_sidecar_lease() -> io::Result<Option<std::fs::File>> {
    use std::os::windows::fs::OpenOptionsExt;

    let requested = std::env::var_os(sidecar_environment_name()).is_some();
    let _ = REMOTE_SIDECAR_ACTIVE.set(requested);
    if !requested {
        return Ok(None);
    }
    let executable = std::env::current_exe()?;
    let root = executable.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "remote Windows sidecar executable has no parent directory",
        )
    })?;
    let lease_path = root.join(".lease");
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        // Keep the lease itself shareable, but deny deletion of the file and its
        // parent directory while this process or its detached server holds it.
        .share_mode(1 | 2)
        .open(&lease_path)
        .map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "failed to open Windows remote sidecar lease {}: {err}",
                    lease_path.display()
                ),
            )
        })?;
    Ok(Some(lease))
}

#[cfg(not(windows))]
pub(crate) fn adopt_remote_sidecar_lease() -> std::io::Result<Option<std::fs::File>> {
    let _ = REMOTE_SIDECAR_ACTIVE.set(false);
    Ok(None)
}

pub(crate) fn remote_sidecar_active() -> bool {
    REMOTE_SIDECAR_ACTIVE.get().copied().unwrap_or(false)
}

#[cfg(windows)]
pub(crate) fn validate_remote_sidecar_payload(expected_sha256: Option<&str>) -> io::Result<()> {
    if !remote_sidecar_active() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote sidecar payload validation requires sidecar mode",
        ));
    }
    let executable = std::env::current_exe()?;
    let root = executable.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "remote Windows sidecar executable has no parent directory",
        )
    })?;
    run_remote_sidecar_payload_validation(root, &executable, expected_sha256)
}

#[cfg(not(windows))]
pub(crate) fn validate_remote_sidecar_payload(
    _expected_sha256: Option<&str>,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "remote Windows sidecar payload validation requires Windows",
    ))
}

#[cfg(windows)]
fn run_remote_sidecar_payload_validation(
    root: &std::path::Path,
    executable: &std::path::Path,
    expected_sha256: Option<&str>,
) -> io::Result<()> {
    if expected_sha256.is_some_and(|sha256| {
        sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected remote sidecar executable SHA-256 is invalid",
        ));
    }
    let script = format!(
        "{WINDOWS_PAYLOAD_LAYOUT_SCRIPT}\nAssert-HerdrPortablePayload -Root $env:HERDR_REMOTE_PAYLOAD_ROOT -AllowLease\nif (-not [string]::IsNullOrEmpty($env:HERDR_REMOTE_PAYLOAD_SHA256) -and (Get-HerdrFileSha256 -Path $env:HERDR_REMOTE_PAYLOAD_EXE) -cne $env:HERDR_REMOTE_PAYLOAD_SHA256) {{ throw 'remote sidecar executable hash mismatch' }}"
    );
    let encoded = encoded_powershell_script(&script);
    let job = crate::platform::ChildProcessJob::new_kill_on_close()?;
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ])
        .env("HERDR_REMOTE_PAYLOAD_ROOT", root)
        .env("HERDR_REMOTE_PAYLOAD_EXE", executable)
        .env(
            "HERDR_REMOTE_PAYLOAD_SHA256",
            expected_sha256.unwrap_or_default(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("failed to start remote sidecar payload validator: {err}"),
            )
        })?;
    if let Err(err) = job.assign(&child) {
        let _ = child.kill();
        let _ = crate::platform::wait_child_bounded(&mut child, Duration::from_secs(5));
        return Err(io::Error::new(
            err.kind(),
            format!("failed to contain remote sidecar payload validator: {err}"),
        ));
    }
    let status = match crate::platform::wait_child_bounded(&mut child, Duration::from_secs(30)) {
        Ok(Some(status)) => status,
        Ok(None) => {
            return match job.terminate_and_wait(&mut child, Duration::from_secs(5)) {
                Ok(()) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "remote sidecar payload validation exceeded 30 seconds",
                )),
                Err(err) => Err(io::Error::other(format!(
                    "remote sidecar payload validation timed out and cleanup failed: {err}"
                ))),
            };
        }
        Err(wait_err) => {
            return match job.terminate_and_wait(&mut child, Duration::from_secs(5)) {
                Ok(()) => Err(io::Error::new(
                    wait_err.kind(),
                    format!("failed to wait for remote sidecar payload validator: {wait_err}"),
                )),
                Err(cleanup_err) => Err(io::Error::other(format!(
                    "failed to wait for remote sidecar payload validator ({wait_err}); cleanup also failed: {cleanup_err}"
                ))),
            };
        }
    };
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote Windows sidecar payload validation failed",
        ))
    }
}

#[cfg(windows)]
pub(crate) fn configure_remote_sidecar_child(command: &mut std::process::Command) {
    if remote_sidecar_active() {
        command.env(sidecar_environment_name(), "1");
    }
}

#[cfg(not(windows))]
pub(crate) fn configure_remote_sidecar_child(_command: &mut std::process::Command) {}

#[cfg(windows)]
fn ensure_remote_server_running() -> io::Result<()> {
    let socket_path = crate::server::socket_paths::client_socket_path();
    if let Some(status) = crate::server::autodetect::read_server_status()? {
        if status
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.endpoint_protocol_generation)
            != Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION)
        {
            return Err(io::Error::other(
                "remote herdr server needs the stable endpoint protocol before this bridge can attach",
            ));
        }
        return crate::server::autodetect::wait_for_server_socket(
            &socket_path,
            Duration::from_secs(15),
        );
    }

    crate::server::autodetect::spawn_server_daemon()?;
    crate::server::autodetect::wait_for_server_socket(&socket_path, Duration::from_secs(15))
}

#[cfg(test)]
fn remote_bridge_command(session_name: &str) -> String {
    let mut arguments = Vec::new();
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        arguments.push("--session".to_string());
        arguments.push(session_name.to_string());
    }
    arguments.push("remote-client-bridge".to_string());
    streaming_herdr_command("herdr.exe", &arguments, false, &WindowsSshShell::Pwsh)
        .expect("test bridge command")
}

pub(super) fn streaming_herdr_command(
    executable: &str,
    arguments: &[String],
    sidecar: bool,
    shell: &WindowsSshShell,
) -> std::io::Result<String> {
    validate_streaming_shell(shell)?;
    match shell {
        WindowsSshShell::Pwsh => Ok(powershell_herdr_script(
            Some(executable),
            arguments,
            sidecar,
        )),
        WindowsSshShell::Cmd => cmd_herdr_command(executable, arguments, sidecar),
        WindowsSshShell::WindowsPowerShell | WindowsSshShell::Unsupported(_) => Err(
            std::io::Error::other("validated Windows OpenSSH shell became unsupported"),
        ),
    }
}

pub(super) fn validate_streaming_shell(shell: &WindowsSshShell) -> std::io::Result<()> {
    match shell {
        WindowsSshShell::Cmd | WindowsSshShell::Pwsh => Ok(()),
        WindowsSshShell::WindowsPowerShell => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Windows PowerShell 5.1 is configured as the OpenSSH DefaultShell and buffers Herdr's interactive byte stream; configure cmd.exe or pwsh.exe as the OpenSSH DefaultShell",
        )),
        WindowsSshShell::Unsupported(default_shell) => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "unsupported Windows OpenSSH DefaultShell {default_shell:?}; configure cmd.exe or pwsh.exe"
            ),
        )),
    }
}

pub(super) fn powershell_herdr_command(
    executable: Option<&str>,
    arguments: &[String],
    sidecar: bool,
) -> String {
    encoded_powershell_command(&powershell_herdr_script(executable, arguments, sidecar))
}

fn powershell_herdr_script(
    executable: Option<&str>,
    arguments: &[String],
    sidecar: bool,
) -> String {
    let mut script = match executable {
        Some(executable) => format!("$herdr = {}", powershell_quote(executable)),
        None => String::from(
            "$herdr = (Get-Command -Name 'herdr.exe' -CommandType Application -ErrorAction Stop).Source",
        ),
    };
    if sidecar {
        script.push_str(&format!(
            "; $env:{} = '1'; Remove-Item Env:{} -ErrorAction SilentlyContinue",
            sidecar_environment_name(),
            crate::HERDR_ENV_VAR,
        ));
    }
    script.push_str("; & $herdr");
    for argument in arguments {
        script.push(' ');
        script.push_str(&powershell_quote(argument));
    }
    script.push_str("; exit $LASTEXITCODE");
    script
}

fn cmd_herdr_command(
    executable: &str,
    arguments: &[String],
    sidecar: bool,
) -> std::io::Result<String> {
    if executable.contains(['"', '%', '\r', '\n', '\0'])
        || arguments
            .iter()
            .any(|argument| argument.contains(['"', '%', '\r', '\n', '\0']))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows remote bridge path or argument cannot be represented safely for cmd.exe",
        ));
    }

    let mut command = String::new();
    if sidecar {
        command.push_str("set HERDR_REMOTE_SIDECAR_V1=1&&set HERDR_ENV=&&");
    }
    command.push('"');
    command.push_str(executable);
    command.push('"');
    for argument in arguments {
        command.push(' ');
        command.push('"');
        command.push_str(argument);
        command.push('"');
    }
    Ok(command)
}

pub(super) fn powershell_attach_probe_command(
    expected_runtime_version: &str,
    expected_protocol: u32,
    expected_payload_sha256: Option<&str>,
    allow_path_candidate: bool,
    session_name: Option<&str>,
    require_surface_interest: bool,
    exact_identity: bool,
) -> String {
    let server_arguments = match session_name {
        Some(session_name) => format!("@('--session', {})", powershell_quote(session_name)),
        None => "@()".to_string(),
    };
    let expected_payload_sha256 = expected_payload_sha256
        .map(powershell_quote)
        .unwrap_or_else(|| "$null".to_string());
    let variables = format!(
        "$Runtime = {}\n$Protocol = {}\n$Hash = {}\n$V = {}\n$AllowPath = {}\n$Session = {}\n",
        powershell_quote(expected_runtime_version),
        expected_protocol,
        expected_payload_sha256,
        powershell_quote(REMOTE_SIDECAR_VALIDATE_ARG),
        if allow_path_candidate {
            "$true"
        } else {
            "$false"
        },
        server_arguments,
    );
    // Keep the encoded command and its Windows length budget checkout-independent.
    let probe = WINDOWS_ATTACH_PROBE_SCRIPT
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let mut capabilities = vec![crate::protocol::endpoint::WINDOWS_REMOTE_HOST_CAPABILITY];
    if require_surface_interest {
        capabilities.extend([
            crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY,
            crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY,
            crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY,
            crate::protocol::endpoint::REMOTE_CONNECT_ONLY_CAPABILITY,
        ]);
    }
    let requirements = format!(
        "$Exact = ${}\n$Generation = {}\n$Caps = @({})\n",
        if exact_identity { "true" } else { "false" },
        crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
        capabilities
            .into_iter()
            .map(powershell_quote)
            .collect::<Vec<_>>()
            .join(","),
    );
    let script = format!("{variables}{requirements}{probe}");
    encoded_powershell_command(&script)
}

pub(super) fn powershell_script_command(script: &str) -> String {
    encoded_powershell_command(script)
}

pub(super) fn powershell_bootstrap_script(invocation: &str) -> String {
    format!("{WINDOWS_PAYLOAD_LAYOUT_SCRIPT}\n{WINDOWS_BOOTSTRAP_SCRIPT}\n{invocation}\n")
}

pub(super) fn powershell_script_file_bytes(script: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(script.encode_utf16().flat_map(u16::to_le_bytes));
    bytes
}

pub(super) fn powershell_script_file_command(path: &str) -> String {
    let receiver = format!(
        "$ErrorActionPreference='Stop';$p={};try{{$b=[IO.File]::ReadAllBytes($p);if($b.Length -lt 2 -or $b[0] -ne 255 -or $b[1] -ne 254){{throw 'invalid Windows remote PowerShell bootstrap'}};$s=[Text.Encoding]::Unicode.GetString($b,2,$b.Length-2);&([ScriptBlock]::Create($s))}}finally{{if([IO.File]::Exists($p)){{[IO.File]::Delete($p)}}}}",
        powershell_quote(path)
    );
    encoded_powershell_command(&receiver)
}

#[cfg(windows)]
pub(super) fn local_payload_package_encoded_script() -> String {
    encoded_powershell_script(&format!(
        "{WINDOWS_PAYLOAD_LAYOUT_SCRIPT}\n{WINDOWS_PACKAGE_LOCAL_PAYLOAD_SCRIPT}"
    ))
}

fn encoded_powershell_command(script: &str) -> String {
    // Bounded probes and provisioning may use an explicit PowerShell process
    // because their output is consumed only after exit. The interactive bridge
    // deliberately bypasses this wrapper because Windows PowerShell buffers a
    // native child's stdout and can serialize diagnostics as CLIXML.
    // Windows PowerShell forces redirected stderr to CLIXML for -EncodedCommand,
    // even with -OutputFormat Text. Keep the payload encoded, but invoke a literal
    // decoder so diagnostics use the normal text formatter. Only base64 enters
    // this fixed command string, never raw script text or shell arguments.
    let encoded = encoded_powershell_script(script);
    format!("powershell.exe -NoLogo -NoProfile -NonInteractive -OutputFormat Text -Command \"& ([ScriptBlock]::Create([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{encoded}'))))\"")
}

fn encoded_powershell_script(script: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )
}

pub(super) fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn sidecar_environment_name() -> &'static str {
    WINDOWS_REMOTE_SIDECAR_ENV
}

#[cfg(windows)]
fn copy_flush<R: io::Read, W: io::Write>(reader: &mut R, writer: &mut W) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;

    loop {
        let bytes_read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        writer.write_all(&buffer[..bytes_read])?;
        writer.flush()?;
        total += bytes_read as u64;
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn connect_only_bridge_does_not_start_a_missing_server() {
        let root = std::env::temp_dir().join(format!("herdr-connect-only-{}", std::process::id()));
        let original = std::env::var_os(crate::api::SOCKET_PATH_ENV_VAR);
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, root.join("missing.sock"));
        let path = crate::server::socket_paths::client_socket_path();
        assert!(path.starts_with(&root));
        let result = run_remote_client_bridge(
            super::super::bridge_allows_start(&["--connect-only".into()]).unwrap(),
        );
        if let Some(original) = original {
            std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, original);
        } else {
            std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        }
        assert!(result.is_err());
        assert!(!path.exists());
        assert!(super::super::bridge_allows_start(&[]).unwrap());
        assert!(
            super::super::bridge_allows_start(&["--connect-only".into(), "unknown".into()])
                .is_err()
        );
    }
    use super::*;

    #[test]
    fn windows_pwsh_remote_bridge_invokes_explicit_binary_for_default_session() {
        assert_eq!(
            remote_bridge_command(crate::session::DEFAULT_SESSION_NAME),
            "$herdr = 'herdr.exe'; & $herdr 'remote-client-bridge'; exit $LASTEXITCODE"
        );
    }

    #[test]
    fn windows_pwsh_remote_bridge_quotes_named_session() {
        assert_eq!(
            remote_bridge_command("agent's work"),
            "$herdr = 'herdr.exe'; & $herdr '--session' 'agent''s work' 'remote-client-bridge'; exit $LASTEXITCODE"
        );
    }

    #[test]
    fn windows_cmd_remote_bridge_invokes_the_binary_without_powershell() {
        let command = streaming_herdr_command(
            r"C:\Users\Can D\herdr.exe",
            &[
                "--session".into(),
                "work".into(),
                "remote-client-bridge".into(),
            ],
            true,
            &WindowsSshShell::Cmd,
        )
        .unwrap();

        assert_eq!(
            command,
            r#"set HERDR_REMOTE_SIDECAR_V1=1&&set HERDR_ENV=&&"C:\Users\Can D\herdr.exe" "--session" "work" "remote-client-bridge""#
        );
        assert!(!command.contains("powershell.exe"));
    }

    #[test]
    fn windows_powershell_5_remote_bridge_is_rejected_instead_of_buffered() {
        let err = streaming_herdr_command(
            r"C:\Users\Can\herdr.exe",
            &["remote-client-bridge".into()],
            true,
            &WindowsSshShell::WindowsPowerShell,
        )
        .unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert!(err
            .to_string()
            .contains("buffers Herdr's interactive byte stream"));
    }

    #[test]
    fn windows_openssh_default_shell_is_classified_by_executable() {
        assert_eq!(
            WindowsSshShell::from_default_shell(r"C:\Windows\System32\cmd.exe"),
            WindowsSshShell::Cmd
        );
        assert_eq!(
            WindowsSshShell::from_default_shell(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            WindowsSshShell::Pwsh
        );
        assert_eq!(
            WindowsSshShell::from_default_shell(
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
            ),
            WindowsSshShell::WindowsPowerShell
        );
    }

    #[test]
    fn windows_explicit_remote_command_quotes_every_argument_and_propagates_exit() {
        assert_eq!(
            decoded_powershell_command(&powershell_herdr_command(
                Some("C:\\Users\\A B\\herdr.exe"),
                &["status".into(), "client".into(), "--json".into()],
                true,
            )),
            "$herdr = 'C:\\Users\\A B\\herdr.exe'; $env:HERDR_REMOTE_SIDECAR_V1 = '1'; Remove-Item Env:HERDR_ENV -ErrorAction SilentlyContinue; & $herdr 'status' 'client' '--json'; exit $LASTEXITCODE"
        );
    }

    #[cfg(windows)]
    #[test]
    fn remote_sidecar_payload_self_validation_rejects_an_incomplete_runtime() {
        use sha2::Digest as _;

        let root = std::env::temp_dir().join(format!(
            "herdr-sidecar-payload-validation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for relative in [
            "herdr.exe",
            "LICENSE.txt",
            "conpty/conpty.dll",
            "conpty/x64/OpenConsole.exe",
            "conpty/arm64/OpenConsole.exe",
            "THIRD-PARTY-NOTICES/Microsoft.Windows.Console.ConPTY-LICENSE.txt",
            "THIRD-PARTY-NOTICES/Microsoft.Windows.Console.ConPTY-NOTICE.md",
            ".lease",
        ] {
            let path = relative
                .split('/')
                .fold(root.clone(), |path, component| path.join(component));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("payload:{relative}")).unwrap();
        }
        let sha256 = |path: &std::path::Path| {
            sha2::Sha256::digest(std::fs::read(path).unwrap())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let hashes = [
            "conpty/conpty.dll",
            "conpty/x64/OpenConsole.exe",
            "conpty/arm64/OpenConsole.exe",
        ]
        .into_iter()
        .map(|relative| {
            let path = relative
                .split('/')
                .fold(root.clone(), |path, component| path.join(component));
            (relative, sha256(&path))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
        std::fs::write(
            root.join("conpty").join("herdr-conpty.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                "package": "Microsoft.Windows.Console.ConPTY",
                "version": "test",
                "architecture": "x86_64",
                "files": hashes,
            }))
            .unwrap(),
        )
        .unwrap();
        let executable = root.join("herdr.exe");
        let executable_sha256 = sha256(&executable);

        run_remote_sidecar_payload_validation(&root, &executable, Some(&executable_sha256))
            .unwrap();
        std::fs::remove_file(root.join("conpty").join("conpty.dll")).unwrap();
        assert_eq!(
            run_remote_sidecar_payload_validation(&root, &executable, Some(&executable_sha256))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_attach_probe_combines_platform_binary_and_server_inspection() {
        let expected_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let expected_runtime = "2026.09.08.2359Z+0123456789ab.0123456789ab";
        let session = "a".repeat(64);
        let command = powershell_attach_probe_command(
            expected_runtime,
            20,
            Some(expected_sha256),
            true,
            Some(&session),
            true,
            true,
        );
        let script = decoded_powershell_command(&command);

        assert!(
            command.encode_utf16().count() < 8191,
            "encoded attach probe has {} UTF-16 code units",
            command.encode_utf16().count()
        );
        assert!(script.contains(&format!("$Runtime = '{expected_runtime}'")));
        assert!(script.contains("$Protocol = 20"));
        assert!(script.contains(&format!("$Hash = '{expected_sha256}'")));
        assert!(script.contains(&format!("$V = '{REMOTE_SIDECAR_VALIDATE_ARG}'")));
        assert!(script.contains("$AllowPath = $true"));
        assert!(script.contains(&format!("$Session = @('--session', '{session}')")));
        assert!(script.contains("PROCESSOR_ARCHITECTURE"));
        assert!(script.contains("default_shell"));
        assert!(script.contains("OpenSSH"));
        assert!(script.contains("Get-Command -Name 'herdr.exe'"));
        assert!(!script.contains("Get-FileHash"));
        assert!(script.contains("'remote',\n'herdr.exe'"));
        assert!(script.contains("'status' 'client' '--json'"));
        assert!(script.contains("@('status', 'server', '--json')"));
        assert!(script.contains("matches_current"));
        assert!(script.contains("candidate = $candidate"));
        assert!(!script.contains("selected ="));
        assert!(script.contains("ConvertTo-Json -Compress"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_bootstrap_file_transport_executes_with_text_diagnostics_and_deletes_script() {
        use std::io::Write as _;
        use std::os::windows::process::CommandExt as _;

        let path = std::env::temp_dir().join(format!(
            "herdr-bootstrap-test-{}-{}.ps1",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let script = "$ErrorActionPreference='Stop';Write-Progress -Activity 'bootstrap-progress' -PercentComplete 50;[Console]::Out.Write('bootstrap-ok');throw 'bootstrap-diagnostic'";
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(&powershell_script_file_bytes(script))
            .unwrap();
        drop(file);
        let command = powershell_script_file_command(path.to_str().unwrap());
        let job = crate::platform::ChildProcessJob::new_kill_on_close().unwrap();
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/d", "/s", "/c"])
            .raw_arg(&command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        job.assign(&child).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let completed = loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        if !completed {
            let _ = job.terminate_and_wait(&mut child, Duration::from_secs(5));
            let output = child.wait_with_output().unwrap();
            let _ = std::fs::remove_file(&path);
            panic!(
                "bootstrap file receiver did not finish: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let output = child.wait_with_output().unwrap();
        let script_was_deleted = !path.exists();
        let _ = std::fs::remove_file(path);

        assert!(
            command.encode_utf16().count() < 8191,
            "bootstrap receiver has {} UTF-16 code units",
            command.encode_utf16().count()
        );
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"bootstrap-ok");
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostics.contains("bootstrap-diagnostic"),
            "{diagnostics}"
        );
        assert!(!diagnostics.contains("#< CLIXML"), "{diagnostics}");
        assert!(!diagnostics.contains("<Objs "), "{diagnostics}");
        assert!(script_was_deleted);
    }

    #[cfg(windows)]
    #[test]
    fn remote_sidecar_removal_waits_for_runtime_files_to_unlock() {
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::process::CommandExt as _;

        let root = std::env::temp_dir().join(format!(
            "herdr-sidecar-removal-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("payload.txt"), b"payload").unwrap();
        let locked_executable = root.join("locked.exe");
        std::fs::copy(
            std::path::Path::new(&std::env::var("SystemRoot").unwrap())
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe"),
            &locked_executable,
        )
        .unwrap();
        let mut locked_process = std::process::Command::new(&locked_executable)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write(root.join(".lease"), b"").unwrap();
        let lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(1 | 2)
            .open(root.join(".lease"))
            .unwrap();

        let invocation = format!(
            "Remove-HerdrRemoteSidecar -Path {} -ReleaseWaitMilliseconds 5000",
            powershell_quote(root.to_str().unwrap())
        );
        let script_path = root.with_extension("ps1");
        std::fs::write(
            &script_path,
            powershell_script_file_bytes(&powershell_bootstrap_script(&invocation)),
        )
        .unwrap();
        let command = powershell_script_file_command(script_path.to_str().unwrap());
        let job = crate::platform::ChildProcessJob::new_kill_on_close().unwrap();
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/d", "/s", "/c"])
            .raw_arg(&command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        job.assign(&child).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(500));
        drop(lease);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            let _ = locked_process.kill();
            let _ = locked_process.wait();
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_file(&script_path);
            panic!(
                "sidecar removal did not wait for the locked runtime: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(locked_process.try_wait().unwrap().is_none());
        locked_process.kill().unwrap();
        locked_process.wait().unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(7);
        while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if child.try_wait().unwrap().is_none() {
            let _ = job.terminate_and_wait(&mut child, Duration::from_secs(5));
        }
        let output = child.wait_with_output().unwrap();
        let root_removed = !root.exists();
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&script_path);

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(root_removed);
    }

    fn decoded_powershell_command(command: &str) -> String {
        let encoded = command.split('\'').nth(1).unwrap();
        decode_powershell_script(encoded)
    }

    fn decode_powershell_script(encoded: &str) -> String {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let utf16 = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&utf16).unwrap()
    }
}
