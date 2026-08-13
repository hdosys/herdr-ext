#[cfg(windows)]
#[path = "../platform/windows/launcher.rs"]
mod launcher;
#[cfg(windows)]
#[path = "../managed_install.rs"]
mod managed_install;
#[cfg(windows)]
#[path = "../platform/windows/managed_install.rs"]
mod windows_managed_install;

#[cfg(windows)]
mod platform {
    use std::{io, path::PathBuf};

    // `managed_install.rs` is shared with the product binary and delegates its
    // product-side executable lookup through this platform boundary.
    #[allow(dead_code)]
    pub(crate) fn managed_install_command_executable(current: PathBuf) -> io::Result<PathBuf> {
        crate::windows_managed_install::managed_install_command_executable_platform(current)
    }
}

#[cfg(windows)]
fn main() {
    match launcher::run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr().lock(), "herdr launcher: {err}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() {}
