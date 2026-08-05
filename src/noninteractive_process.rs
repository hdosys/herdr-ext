use std::{ffi::OsStr, process::Command};

/// Builds a subprocess whose stdio is controlled by the caller and which never opens a Windows console.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    crate::platform::configure_background_command(&mut command);
    command
}

fn curl_command_with_protocols(url: impl AsRef<OsStr>, protocols: &str) -> Command {
    let mut command = command("curl");
    command.args([
        "-q",
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--globoff",
        "--proto",
        protocols,
        "--proto-redir",
        protocols,
        "--tlsv1.2",
        "--max-redirs",
        "5",
        "--url",
    ]);
    command.arg(url);
    command
}

pub(crate) fn curl_command(url: impl AsRef<OsStr>) -> Command {
    curl_command_with_protocols(url, "=https")
}

#[cfg(test)]
pub(crate) fn curl_command_for_test_file(url: impl AsRef<OsStr>) -> Command {
    curl_command_with_protocols(url, "=https,file")
}

#[cfg(test)]
mod tests {
    #[test]
    fn curl_ignores_config_and_restricts_transfers_to_https() {
        let command = super::curl_command("https://example.com/update.json");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(args.first().map(String::as_str), Some("-q"));
        assert_eq!(
            args,
            [
                "-q",
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--globoff",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--tlsv1.2",
                "--max-redirs",
                "5",
                "--url",
                "https://example.com/update.json",
            ]
        );
    }
}
