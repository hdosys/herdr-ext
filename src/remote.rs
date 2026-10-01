mod args;
mod attach;
#[cfg(unix)]
mod host;
mod process;
mod restart_policy;
mod saved;
#[cfg(unix)]
mod ssh_agent;
mod windows;

pub(crate) use args::*;
pub(crate) use attach::*;
#[cfg(unix)]
pub(crate) use host::run_remote_client_bridge;
pub(crate) use saved::*;

pub(crate) fn bridge_allows_start(args: &[String]) -> std::io::Result<bool> {
    let mut allow_start = true;
    let mut idle_timeout = false;
    for argument in args {
        match argument.as_str() {
            "--connect-only" if allow_start => allow_start = false,
            "--idle-timeout-v1"
                if !idle_timeout && crate::platform::REMOTE_BRIDGE_IDLE_TIMEOUT_SUPPORTED =>
            {
                idle_timeout = true;
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid remote bridge arguments",
                ));
            }
        }
    }
    Ok(allow_start)
}

pub(crate) fn run_remote_api_bridge(args: &[String]) -> std::io::Result<()> {
    match args {
        [] => {
            let path = crate::api::socket_path();
            let stream = crate::ipc::connect_local_stream(&path).map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "failed to connect to remote Herdr API socket {}: {error}",
                        path.display()
                    ),
                )
            })?;
            crate::platform::forward_remote_bridge_stdio(stream, false)
        }
        [flag] if flag == "--check" => {
            println!("herdr-api-bridge-v1");
            Ok(())
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "usage: herdr remote-api-bridge [--check]",
        )),
    }
}

#[cfg(windows)]
pub(crate) fn run_remote_client_bridge(args: &[String]) -> std::io::Result<()> {
    windows::run_remote_client_bridge(bridge_allows_start(args)?)
}
pub(crate) use windows::{
    adopt_remote_sidecar_lease, configure_remote_sidecar_child, remote_sidecar_active,
    validate_remote_sidecar_payload, WindowsSshShell, REMOTE_SIDECAR_VALIDATE_ARG,
};

pub(crate) fn reject_remote_sidecar_update_command(args: &[String]) -> Result<(), String> {
    if remote_sidecar_active() && update_command_requested(args) {
        return Err(
            "self-update is disabled for a remote Windows sidecar; update it from the attaching Herdr client"
                .into(),
        );
    }
    Ok(())
}

fn update_command_requested(args: &[String]) -> bool {
    args.get(1).is_some_and(|command| command == "update")
        || args
            .get(1..3)
            .is_some_and(|commands| commands == ["channel", "set"])
}

pub(crate) fn print_saved_ssh_error_hint(err: &std::io::Error, target: &str) {
    if is_remote_host_key_error(err) {
        eprintln!(
            "hint: saved machines use strict host-key checking; add the host key to the configured known_hosts file, then retry."
        );
    } else {
        print_remote_error_hint(err, target);
    }
}

pub(crate) fn print_remote_error_hint(err: &std::io::Error, target: &str) {
    if is_remote_auth_error(err) {
        eprintln!(
            "hint: verify SSH access first with `{}`.",
            ssh_check_command(target)
        );
        eprintln!(
            "hint: if your SSH key has a passphrase, load it into ssh-agent with `ssh-add` before running `herdr --remote`."
        );
    }
}

fn is_remote_host_key_error(err: &std::io::Error) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    message.contains("host key verification failed")
        || message.contains("remote host identification has changed")
}

fn is_remote_auth_error(err: &std::io::Error) -> bool {
    let message = err.to_string();
    message.contains("Permission denied")
        && (message.contains("(publickey")
            || message.contains("(keyboard-interactive")
            || message.contains("(password"))
}

fn ssh_check_command(target: &str) -> String {
    format!("ssh {}", shell_quote(target))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_host_key_error_matches_ssh_diagnostics() {
        for message in [
            "Host key verification failed.",
            "REMOTE HOST IDENTIFICATION HAS CHANGED!",
        ] {
            assert!(is_remote_host_key_error(&std::io::Error::other(message)));
        }
        assert!(!is_remote_host_key_error(&std::io::Error::other(
            "server closed connection"
        )));
    }

    #[test]
    fn remote_auth_error_matches_ssh_auth_denied() {
        let err = std::io::Error::other(
            "remote platform detection failed: user@host: Permission denied (publickey).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_matches_keyboard_interactive_denied() {
        let err = std::io::Error::other(
            "remote server status failed: user@host: Permission denied (keyboard-interactive).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_ignores_non_auth_errors() {
        let err = std::io::Error::other("remote platform detection failed: unsupported platform");

        assert!(!is_remote_auth_error(&err));
    }

    #[test]
    fn ssh_check_command_quotes_remote_target() {
        assert_eq!(ssh_check_command("host name"), "ssh 'host name'");
    }

    #[test]
    fn remote_sidecar_update_gate_matches_every_self_update_entry() {
        assert!(update_command_requested(&["herdr".into(), "update".into()]));
        assert!(update_command_requested(&[
            "herdr".into(),
            "channel".into(),
            "set".into(),
        ]));
        assert!(!update_command_requested(&[
            "herdr".into(),
            "status".into()
        ]));
    }
}
