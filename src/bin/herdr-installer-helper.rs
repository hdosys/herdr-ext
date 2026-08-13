#[cfg(windows)]
#[path = "../platform/windows/installer_helper.rs"]
mod installer_helper;
#[cfg(windows)]
#[allow(dead_code)]
#[path = "../managed_install.rs"]
mod managed_install;
#[cfg(windows)]
#[allow(dead_code)]
#[path = "../platform/windows/managed_install.rs"]
mod windows_managed_install;
#[cfg(windows)]
#[path = "../platform/windows/process_job.rs"]
mod windows_process_job;

#[cfg(windows)]
mod platform {
    use std::{io, path::PathBuf};

    #[allow(dead_code)]
    pub(crate) fn managed_install_command_executable(current: PathBuf) -> io::Result<PathBuf> {
        crate::windows_managed_install::managed_install_command_executable_platform(current)
    }
}

#[cfg(windows)]
fn main() {
    use std::io::Write as _;

    match installer_helper::run() {
        Ok(message) => {
            if !message.is_empty() {
                println!("{message}");
            }
        }
        Err(err) => {
            let _ = writeln!(std::io::stderr().lock(), "Herdr Win installer error: {err}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() {}
