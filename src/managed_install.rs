//! Shared managed-install identity and path contract.

#[cfg(windows)]
use std::fs;
use std::{io, path::PathBuf};

#[cfg(windows)]
pub(crate) const RUNTIME_RECORD_HEADER: &str = "herdr-runtime-v1";
// Used by the dedicated launcher crate; the product crate only adopts leases.
#[allow(dead_code)]
pub(crate) const POINTER_RECORD_HEADER: &str = "herdr-pointer-v1";
#[cfg(windows)]
pub(crate) const MANAGED_BIN_MARKER: &[u8] = b"herdr-managed-bin-v1\n";
#[cfg(windows)]
pub(crate) const NATIVE_HELPER_NAME: &str = "installer-helper.exe";
// These names are consumed by the dedicated launcher and installer-helper binaries.
#[allow(dead_code)]
pub(crate) const LAUNCHER_BUILD_ID_QUERY_ARG: &str = "--herdr-private-launcher-build-id-v1";
#[allow(dead_code)]
pub(crate) const PENDING_LAUNCHER_PREFIX: &str = "launcher.pending-";
#[allow(dead_code)]
pub(crate) const PENDING_LAUNCHER_SUFFIX: &str = ".exe";
#[allow(dead_code)]
pub(crate) const UNINSTALL_PENDING_MARKER: &str = "uninstall.pending";
#[cfg(windows)]
// Shared with the launcher binary, which never inspects updater ownership.
#[allow(dead_code)]
pub(crate) const WINGET_PACKAGE_MANAGER_RECORD: &[u8] =
    b"herdr-package-manager-v1\nmanager=winget\n";
#[cfg(windows)]
// Shared with the product and installer-helper binaries; the launcher does not invoke it.
#[allow(dead_code)]
pub(crate) const INSTALLER_STOP_SESSIONS_COMMAND: &str = "__installer-stop-sessions";

#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BuildId(String);

#[cfg(windows)]
impl BuildId {
    pub(crate) fn parse(value: &str) -> io::Result<Self> {
        let bytes = value.as_bytes();
        if bytes.len() != 25
            || bytes[12] != b'.'
            || !bytes[..12].iter().all(|byte| is_lower_hex(*byte))
            || !bytes[13..].iter().all(|byte| is_lower_hex(*byte))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid managed Herdr build ID {value:?}; expected 12 lowercase hex digits, a dot, and 12 lowercase hex digits"
                ),
            ));
        }
        Ok(Self(value.to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(windows)]
fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

#[cfg(windows)]
pub(crate) fn parse_record(
    bytes: &[u8],
    expected_header: &str,
    source: &std::path::Path,
) -> io::Result<BuildId> {
    let text = std::str::from_utf8(bytes).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "managed install record {} is not strict UTF-8: {err}",
                source.display()
            ),
        )
    })?;
    let prefix = format!("{expected_header}\nbuild_id=");
    let build_id = text
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix('\n'))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "managed install record {} does not match the exact {expected_header} format",
                    source.display()
                ),
            )
        })?;
    BuildId::parse(build_id).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "managed install record {} has an invalid build ID: {err}",
                source.display()
            ),
        )
    })
}

#[cfg(windows)]
#[derive(Clone, Debug)]
pub(crate) struct ManagedInstall {
    root: PathBuf,
}

#[cfg(windows)]
impl ManagedInstall {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub(crate) fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    pub(crate) fn launcher_path(&self) -> PathBuf {
        self.bin_dir().join("herdr.exe")
    }

    pub(crate) fn bin_sentinel_dir(&self) -> PathBuf {
        self.bin_dir().join("managed-install-v1")
    }

    pub(crate) fn bin_marker_path(&self) -> PathBuf {
        self.bin_sentinel_dir().join("marker")
    }

    pub(crate) fn runtime_dir(&self) -> PathBuf {
        self.root.join("runtime")
    }

    pub(crate) fn build_dir(&self, build_id: &BuildId) -> PathBuf {
        self.runtime_dir().join(build_id.as_str())
    }

    pub(crate) fn payload_path(&self, build_id: &BuildId) -> PathBuf {
        self.build_dir(build_id).join("herdr.exe")
    }

    pub(crate) fn runtime_marker_path(&self, build_id: &BuildId) -> PathBuf {
        self.build_dir(build_id).join("runtime.ready")
    }

    pub(crate) fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    // Used by the dedicated launcher crate through the shared contract source.
    #[allow(dead_code)]
    pub(crate) fn pointer_path(&self, name: &str) -> PathBuf {
        self.state_dir().join(name)
    }

    pub(crate) fn leases_dir(&self) -> PathBuf {
        self.state_dir().join("leases")
    }

    pub(crate) fn installer_helper_path(&self) -> PathBuf {
        self.state_dir().join(NATIVE_HELPER_NAME)
    }

    #[cfg(windows)]
    // Shared with the launcher binary, which never inspects updater ownership.
    #[allow(dead_code)]
    pub(crate) fn package_manager_path(&self) -> PathBuf {
        self.state_dir().join("package-manager")
    }

    pub(crate) fn lease_path(&self, build_id: &BuildId) -> PathBuf {
        self.leases_dir()
            .join(format!("{}.lease", build_id.as_str()))
    }

    // Used by the dedicated launcher crate through the shared contract source.
    #[allow(dead_code)]
    pub(crate) fn coordination_lock_path(&self) -> PathBuf {
        self.state_dir().join("launcher.lock")
    }
}

/// Returns the stable command path for managed Windows payloads and the
/// physical executable path for every other installation.
// This source is also compiled into the dedicated launcher binary, where this
// product-side entry point is intentionally unused.
#[allow(dead_code)]
pub(crate) fn command_executable() -> io::Result<PathBuf> {
    let current = std::env::current_exe().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("failed to determine the physical Herdr executable path: {err}"),
        )
    })?;
    crate::platform::managed_install_command_executable(current)
}

#[cfg(windows)]
// The same contract source is compiled into the launcher binary, which never
// enters the product updater path.
#[allow(dead_code)]
pub(crate) fn current_payload_is_managed() -> io::Result<bool> {
    let current = std::env::current_exe().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("failed to determine the physical Herdr executable path: {err}"),
        )
    })?;
    Ok(crate::platform::managed_install_command_executable(current.clone())? != current)
}

#[cfg(windows)]
// The launcher compiles this module but never enters the product updater path.
#[allow(dead_code)]
pub(crate) fn current_install_is_winget() -> io::Result<bool> {
    let current = std::env::current_exe().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("failed to determine the physical Herdr executable path: {err}"),
        )
    })?;
    let command = crate::platform::managed_install_command_executable(current.clone())?;
    if command == current {
        return Ok(false);
    }
    let root = command
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "managed Herdr command path has no install root: {}",
                    command.display()
                ),
            )
        })?;
    let marker = ManagedInstall::new(root.to_path_buf()).package_manager_path();
    match fs::read(&marker) {
        Ok(bytes) => {
            validate_winget_package_manager_record(&bytes, &marker)?;
            Ok(true)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(io::Error::new(
            err.kind(),
            format!(
                "failed to read managed Herdr package-manager marker {}: {err}",
                marker.display()
            ),
        )),
    }
}

#[cfg(windows)]
// The launcher compiles this module but never validates updater ownership.
#[allow(dead_code)]
fn validate_winget_package_manager_record(
    bytes: &[u8],
    source: &std::path::Path,
) -> io::Result<()> {
    if bytes != WINGET_PACKAGE_MANAGER_RECORD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "managed Herdr package-manager marker {} is not the exact WinGet record",
                source.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    const BUILD_ID: &str = "0123456789ab.cdef01234567";

    #[test]
    fn build_ids_are_exact_lowercase_hash_pairs() {
        assert_eq!(BuildId::parse(BUILD_ID).unwrap().as_str(), BUILD_ID);
        for invalid in [
            "0123456789AB.cdef01234567",
            "0123456789ab-Cdef01234567",
            "0123456789ab.cdef0123456",
            "0123456789ab.cdef012345678",
            "0123456789ab/cdef01234567",
            "0123456789ab.cdef0123456g",
        ] {
            assert!(BuildId::parse(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn records_require_exact_utf8_header_id_and_final_newline() {
        let source = std::path::Path::new("pointer");
        let exact = format!("{POINTER_RECORD_HEADER}\nbuild_id={BUILD_ID}\n");
        assert_eq!(
            parse_record(exact.as_bytes(), POINTER_RECORD_HEADER, source)
                .unwrap()
                .as_str(),
            BUILD_ID
        );

        for invalid in [
            exact.trim_end().to_string(),
            exact.replace('\n', "\r\n"),
            format!("{exact}extra\n"),
            exact.replace(POINTER_RECORD_HEADER, RUNTIME_RECORD_HEADER),
        ] {
            assert!(
                parse_record(invalid.as_bytes(), POINTER_RECORD_HEADER, source).is_err(),
                "accepted {invalid:?}"
            );
        }
        assert!(parse_record(&[0xff], POINTER_RECORD_HEADER, source).is_err());
    }

    #[test]
    fn winget_package_manager_record_is_exact() {
        let source = std::path::Path::new("package-manager");
        validate_winget_package_manager_record(WINGET_PACKAGE_MANAGER_RECORD, source).unwrap();
        for invalid in [
            b"herdr-package-manager-v1\nmanager=winget".as_slice(),
            b"herdr-package-manager-v1\r\nmanager=winget\r\n".as_slice(),
            b"herdr-package-manager-v1\nmanager=other\n".as_slice(),
        ] {
            assert!(validate_winget_package_manager_record(invalid, source).is_err());
        }
    }
}
