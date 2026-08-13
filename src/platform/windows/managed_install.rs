//! Windows managed-install filesystem and lease boundary.

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::windows::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, OpenOptionsExt as _},
        io::{AsRawHandle as _, FromRawHandle as _},
    },
    path::{Path, PathBuf},
    process::Command,
};

use windows_sys::Win32::{
    Foundation::{
        GetHandleInformation, SetHandleInformation, ERROR_SHARING_VIOLATION, HANDLE,
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{
        GetFileInformationByHandle, MoveFileExW, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    },
};

use crate::managed_install::{
    parse_record, BuildId, ManagedInstall, MANAGED_BIN_MARKER, POINTER_RECORD_HEADER,
    RUNTIME_RECORD_HEADER,
};

pub(crate) const MANAGED_LEASE_HANDLE_ENV: &str = "HERDR_INTERNAL_MANAGED_LEASE_HANDLE_V1";
const MAX_RECORD_BYTES: u64 = 128;
const ACTIVE_POINTER: &str = "active";
const PENDING_POINTER: &str = "pending";

#[derive(Debug)]
pub(crate) struct Runtime {
    pub(crate) build_id: BuildId,
    pub(crate) executable: PathBuf,
}

pub(crate) struct SharedLease {
    file: File,
}

impl SharedLease {
    pub(crate) fn configure_payload_child(&self, command: &mut Command) {
        let value = format!("{:x}", self.file.as_raw_handle() as usize);
        command.env(MANAGED_LEASE_HANDLE_ENV, value);
    }
}

#[derive(Debug)]
pub(crate) struct CoordinationLease {
    _file: File,
}

// This module is shared with the dedicated launcher binary, where the product
// startup guard is intentionally not constructed.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct ManagedRuntimeLease {
    _file: Option<File>,
}

#[allow(dead_code)]
impl ManagedRuntimeLease {
    fn unmanaged() -> Self {
        Self { _file: None }
    }

    fn managed(file: File) -> Self {
        Self { _file: Some(file) }
    }
}

pub(crate) fn managed_install_command_executable_platform(current: PathBuf) -> io::Result<PathBuf> {
    match managed_payload_context(&current)? {
        Some((install, _runtime)) => install.validate_managed_bin(),
        None => Ok(current),
    }
}

/// Adopts the dispatcher-provided lease before the product can create children.
// The same source is compiled into the dedicated launcher binary, which never
// enters the product startup path.
#[allow(dead_code)]
pub(crate) fn adopt_managed_runtime_lease_platform() -> io::Result<ManagedRuntimeLease> {
    let current = std::env::current_exe().map_err(|err| {
        contextual(
            err,
            "failed to determine the physical Herdr executable path".to_string(),
        )
    })?;
    let Some((install, runtime)) = managed_payload_context(&current)? else {
        return Ok(ManagedRuntimeLease::unmanaged());
    };

    let encoded = std::env::var_os(MANAGED_LEASE_HANDLE_ENV).ok_or_else(|| {
        invalid_data(format!(
            "managed Herdr payload {} was started without its runtime lease; launch {} instead",
            current.display(),
            install.launcher_path().display()
        ))
    })?;
    let encoded = encoded.to_str().ok_or_else(|| {
        invalid_data(format!(
            "managed Herdr payload lease handle is not valid UTF-8 for build {}",
            runtime.build_id.as_str()
        ))
    })?;
    let handle_value = usize::from_str_radix(encoded, 16).map_err(|err| {
        invalid_data(format!(
            "managed Herdr payload lease handle {encoded:?} is not lowercase hexadecimal: {err}"
        ))
    })?;
    if encoded.is_empty()
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || handle_value == 0
        || handle_value == INVALID_HANDLE_VALUE as usize
    {
        return Err(invalid_data(format!(
            "managed Herdr payload lease handle {encoded:?} is invalid"
        )));
    }
    let inherited = handle_value as HANDLE;

    // Do not take ownership of, mutate, or close the supplied value until the
    // physical managed context above and exact lease-file identity below have
    // both been proven.
    let inherited_info = file_information(inherited).map_err(|err| {
        contextual(
            err,
            format!(
                "managed Herdr payload received an invalid lease handle for build {}",
                runtime.build_id.as_str()
            ),
        )
    })?;
    let expected_path = install.lease_path(&runtime.build_id);
    let expected = install.open_existing_lease(&runtime.build_id)?;
    let expected_info = file_information(expected.as_raw_handle() as HANDLE).map_err(|err| {
        contextual(
            err,
            format!(
                "failed to identify expected managed Herdr runtime lease {}",
                expected_path.display()
            ),
        )
    })?;
    if FileIdentity::from(inherited_info) != FileIdentity::from(expected_info) {
        return Err(invalid_data(format!(
            "managed Herdr payload lease handle does not identify expected file {}",
            expected_path.display()
        )));
    }
    drop(expected);

    let mut flags = 0;
    // SAFETY: the OS validated `inherited` as the expected lease file above;
    // `flags` is a valid output pointer.
    if unsafe { GetHandleInformation(inherited, &mut flags) } == 0 {
        return Err(contextual(
            io::Error::last_os_error(),
            format!(
                "failed to inspect inherited managed Herdr runtime lease {}",
                expected_path.display()
            ),
        ));
    }
    if flags & HANDLE_FLAG_INHERIT == 0 {
        return Err(invalid_data(format!(
            "managed Herdr runtime lease was not inheritable on payload entry {}",
            expected_path.display()
        )));
    }

    // A genuine shared lease must prevent the same exclusive open used by
    // activation. This also rejects same-file handles without lease semantics.
    if install
        .try_open_exclusive_lease(&runtime.build_id)?
        .is_some()
    {
        return Err(invalid_data(format!(
            "managed Herdr payload handle does not hold the expected runtime lease {}",
            expected_path.display()
        )));
    }

    // SAFETY: identity and lease interoperability are proven above. Clearing
    // only HANDLE_FLAG_INHERIT keeps this process's valid handle open while
    // preventing unrelated descendants from inheriting it.
    if unsafe { SetHandleInformation(inherited, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(contextual(
            io::Error::last_os_error(),
            format!(
                "failed to clear inheritance on managed Herdr runtime lease {}",
                expected_path.display()
            ),
        ));
    }
    let mut cleared_flags = HANDLE_FLAG_INHERIT;
    // SAFETY: `inherited` is still valid and `cleared_flags` is writable.
    if unsafe { GetHandleInformation(inherited, &mut cleared_flags) } == 0
        || cleared_flags & HANDLE_FLAG_INHERIT != 0
    {
        return Err(io::Error::other(format!(
            "managed Herdr runtime lease remained inheritable after startup {}",
            expected_path.display()
        )));
    }
    std::env::remove_var(MANAGED_LEASE_HANDLE_ENV);

    // SAFETY: only after exact identity validation do we assume ownership of
    // the inherited handle. The returned guard retains it for product lifetime.
    let file = unsafe { File::from_raw_handle(inherited.cast()) };
    Ok(ManagedRuntimeLease::managed(file))
}

fn managed_payload_context(current: &Path) -> io::Result<Option<(ManagedInstall, Runtime)>> {
    if current.file_name() != Some(OsStr::new("herdr.exe")) {
        return Ok(None);
    }
    let Some(build_dir) = current.parent() else {
        return Ok(None);
    };
    let Some(runtime_dir) = build_dir.parent() else {
        return Ok(None);
    };
    if runtime_dir.file_name() != Some(OsStr::new("runtime")) {
        if current
            .ancestors()
            .skip(2)
            .any(|ancestor| ancestor.file_name() == Some(OsStr::new("runtime")))
        {
            return Err(invalid_data(format!(
                "managed Herdr payload {} is not a direct runtime/<build-id>/herdr.exe child",
                current.display()
            )));
        }
        return Ok(None);
    }

    let build_id = build_id_from_dir(build_dir)?;
    let install = install_from_runtime_dir(runtime_dir)?;
    install.validate_managed_bin()?;
    let runtime = install.validate_runtime(&build_id)?;
    if runtime.executable != current {
        return Err(invalid_data(format!(
            "managed Herdr payload resolved to {}, not {}",
            runtime.executable.display(),
            current.display()
        )));
    }
    Ok(Some((install, runtime)))
}

pub(crate) fn build_id_from_dir(build_dir: &Path) -> io::Result<BuildId> {
    let value = build_dir
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            invalid_data(format!(
                "managed Herdr runtime directory {} is not valid UTF-8",
                build_dir.display()
            ))
        })?;
    BuildId::parse(value)
}

pub(crate) fn install_from_runtime_dir(runtime_dir: &Path) -> io::Result<ManagedInstall> {
    let root = runtime_dir.parent().ok_or_else(|| {
        invalid_data(format!(
            "managed Herdr runtime directory {} has no install root",
            runtime_dir.display()
        ))
    })?;
    Ok(ManagedInstall::new(root.to_path_buf()))
}

impl ManagedInstall {
    pub(crate) fn validate_managed_bin(&self) -> io::Result<PathBuf> {
        validate_directory(self.root(), "managed Herdr install root")?;
        validate_directory(&self.bin_dir(), "managed Herdr bin directory")?;
        let bootstrap = self.launcher_path();
        let _ = open_regular_file(&bootstrap, "managed Herdr bootstrap")?;
        validate_directory(
            &self.bin_sentinel_dir(),
            "managed Herdr bin sentinel directory",
        )?;
        read_exact_marker(
            &self.bin_marker_path(),
            MANAGED_BIN_MARKER,
            "managed Herdr bin marker",
        )?;
        Ok(bootstrap)
    }

    pub(crate) fn validate_installer_helper(&self) -> io::Result<PathBuf> {
        let helper = self.installer_helper_path();
        let _ = open_regular_file(&helper, "managed Herdr installer helper")?;
        Ok(helper)
    }

    pub(crate) fn validate_runtime(&self, build_id: &BuildId) -> io::Result<Runtime> {
        validate_directory(self.root(), "managed Herdr install root")?;
        validate_directory(&self.runtime_dir(), "managed Herdr runtime directory")?;
        validate_directory(&self.build_dir(build_id), "managed Herdr build directory")?;

        let marker_path = self.runtime_marker_path(build_id);
        let marker_id = read_record_file(&marker_path, RUNTIME_RECORD_HEADER)?;
        if marker_id != *build_id {
            return Err(invalid_data(format!(
                "managed Herdr runtime marker {} names build {}, expected {}",
                marker_path.display(),
                marker_id.as_str(),
                build_id.as_str()
            )));
        }

        let executable = self.payload_path(build_id);
        let _ = open_regular_file(&executable, "managed Herdr payload")?;
        Ok(Runtime {
            build_id: build_id.clone(),
            executable,
        })
    }

    pub(crate) fn read_required_active_pointer(&self) -> io::Result<BuildId> {
        self.read_pointer(ACTIVE_POINTER)?.ok_or_else(|| {
            invalid_data(format!(
                "managed Herdr active pointer is missing at {}",
                self.pointer_path(ACTIVE_POINTER).display()
            ))
        })
    }

    pub(crate) fn read_pointer(&self, name: &str) -> io::Result<Option<BuildId>> {
        validate_directory(self.root(), "managed Herdr install root")?;
        validate_directory(&self.state_dir(), "managed Herdr state directory")?;
        let path = self.pointer_path(name);
        match read_record_file(&path, POINTER_RECORD_HEADER) {
            Ok(build_id) => Ok(Some(build_id)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn open_shared_lease(&self, build_id: &BuildId) -> io::Result<SharedLease> {
        self.ensure_leases_dir()?;
        let path = self.lease_path(build_id);
        let file = open_control_file(
            &path,
            "managed Herdr runtime lease",
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            true,
        )?;
        set_handle_inheritable(&file, &path)?;
        Ok(SharedLease { file })
    }

    #[allow(dead_code)]
    fn open_existing_lease(&self, build_id: &BuildId) -> io::Result<File> {
        validate_directory(self.root(), "managed Herdr install root")?;
        validate_directory(&self.state_dir(), "managed Herdr state directory")?;
        validate_directory(&self.leases_dir(), "managed Herdr leases directory")?;
        open_control_file(
            &self.lease_path(build_id),
            "managed Herdr runtime lease",
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            false,
        )
    }

    pub(crate) fn try_open_exclusive_lease(&self, build_id: &BuildId) -> io::Result<Option<File>> {
        self.ensure_leases_dir()?;
        try_open_exclusive_control_file(&self.lease_path(build_id), "managed Herdr runtime lease")
    }

    pub(crate) fn try_open_coordination_lease(&self) -> io::Result<Option<CoordinationLease>> {
        validate_directory(self.root(), "managed Herdr install root")?;
        validate_directory(&self.state_dir(), "managed Herdr state directory")?;
        try_open_exclusive_control_file(
            &self.coordination_lock_path(),
            "managed Herdr launcher coordination gate",
        )
        .map(|file| file.map(|file| CoordinationLease { _file: file }))
    }

    fn ensure_leases_dir(&self) -> io::Result<()> {
        validate_directory(self.root(), "managed Herdr install root")?;
        validate_directory(&self.state_dir(), "managed Herdr state directory")?;
        let leases = self.leases_dir();
        match fs::symlink_metadata(&leases) {
            Ok(_) => validate_directory(&leases, "managed Herdr leases directory"),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&leases).map_err(|err| {
                    contextual(
                        err,
                        format!(
                            "failed to create managed Herdr leases directory {}",
                            leases.display()
                        ),
                    )
                })?;
                validate_directory(&leases, "managed Herdr leases directory")
            }
            Err(err) => Err(contextual(
                err,
                format!(
                    "failed to inspect managed Herdr leases directory {}",
                    leases.display()
                ),
            )),
        }
    }

    pub(crate) fn replace_active_with_pending(&self, expected: &BuildId) -> io::Result<()> {
        let pending = self.pointer_path(PENDING_POINTER);
        let active = self.pointer_path(ACTIVE_POINTER);
        let pending_wide = wide_path(&pending)?;
        let active_wide = wide_path(&active)?;
        // SAFETY: both buffers are valid, NUL-terminated UTF-16 paths for the
        // duration of this immediate same-volume replacement.
        let moved = unsafe {
            MoveFileExW(
                pending_wide.as_ptr(),
                active_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            return Err(contextual(
                io::Error::last_os_error(),
                format!(
                    "failed to atomically activate pending Herdr pointer {} as {}",
                    pending.display(),
                    active.display()
                ),
            ));
        }

        let actual = self.read_required_active_pointer()?;
        if actual != *expected {
            return Err(invalid_data(format!(
                "managed Herdr active pointer names {}, expected newly activated {}",
                actual.as_str(),
                expected.as_str()
            )));
        }
        match fs::symlink_metadata(&pending) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(invalid_data(format!(
                "managed Herdr pending pointer {} remained after activation",
                pending.display()
            ))),
            Err(err) => Err(contextual(
                err,
                format!(
                    "failed to inspect pending Herdr pointer {} after activation",
                    pending.display()
                ),
            )),
        }
    }
}

fn try_open_exclusive_control_file(path: &Path, description: &str) -> io::Result<Option<File>> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(err) if err.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => return Ok(None),
        Err(err) => {
            return Err(contextual(
                err,
                format!("failed to open exclusive {description} {}", path.display()),
            ));
        }
    };
    validate_open_file(&file, path, description)?;
    Ok(Some(file))
}

fn validate_directory(path: &Path, description: &str) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        contextual(
            err,
            format!("failed to inspect {description} {}", path.display()),
        )
    })?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_data(format!(
            "{description} {} must not be a reparse point",
            path.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(invalid_data(format!(
            "{description} {} is not a directory",
            path.display()
        )));
    }
    Ok(())
}

fn open_regular_file(path: &Path, description: &str) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path).map_err(|err| {
        contextual(
            err,
            format!("failed to open {description} {}", path.display()),
        )
    })?;
    validate_open_file(&file, path, description)?;
    Ok(file)
}

fn open_control_file(
    path: &Path,
    description: &str,
    share_mode: u32,
    create: bool,
) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .share_mode(share_mode)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path).map_err(|err| {
        contextual(
            err,
            format!("failed to open {description} {}", path.display()),
        )
    })?;
    validate_open_file(&file, path, description)?;
    Ok(file)
}

fn set_handle_inheritable(file: &File, path: &Path) -> io::Result<()> {
    let handle = file.as_raw_handle() as HANDLE;
    // SAFETY: `handle` belongs to `file`; only its inheritance bit changes.
    if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
        return Err(contextual(
            io::Error::last_os_error(),
            format!(
                "failed to mark managed Herdr runtime lease inheritable {}",
                path.display()
            ),
        ));
    }
    let mut flags = 0;
    // SAFETY: `flags` is writable and `handle` remains valid.
    if unsafe { GetHandleInformation(handle, &mut flags) } == 0 || flags & HANDLE_FLAG_INHERIT == 0
    {
        return Err(io::Error::other(format!(
            "managed Herdr runtime lease did not retain inheritable handle flag {}",
            path.display()
        )));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
struct FileIdentity {
    volume_serial_number: u32,
    file_index_high: u32,
    file_index_low: u32,
}

impl From<BY_HANDLE_FILE_INFORMATION> for FileIdentity {
    fn from(info: BY_HANDLE_FILE_INFORMATION) -> Self {
        Self {
            volume_serial_number: info.dwVolumeSerialNumber,
            file_index_high: info.nFileIndexHigh,
            file_index_low: info.nFileIndexLow,
        }
    }
}

fn file_information(handle: HANDLE) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the kernel validates the opaque handle value; `information` is a
    // valid output buffer. Failure does not transfer or close ownership.
    if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(information)
    }
}

fn validate_open_file(file: &File, path: &Path, description: &str) -> io::Result<()> {
    let information = file_information(file.as_raw_handle() as HANDLE).map_err(|err| {
        contextual(
            err,
            format!("failed to inspect open {description} {}", path.display()),
        )
    })?;
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_data(format!(
            "{description} {} must not be a reparse point",
            path.display()
        )));
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(invalid_data(format!(
            "{description} {} is not a regular file",
            path.display()
        )));
    }
    if information.nNumberOfLinks != 1 {
        return Err(invalid_data(format!(
            "{description} {} must not be a hard link",
            path.display()
        )));
    }
    Ok(())
}

fn read_record_file(path: &Path, expected_header: &str) -> io::Result<BuildId> {
    let bytes = read_limited_file(path, "managed Herdr record")?;
    parse_record(&bytes, expected_header, path)
}

fn read_exact_marker(path: &Path, expected: &[u8], description: &str) -> io::Result<()> {
    let bytes = read_limited_file(path, description)?;
    if bytes != expected {
        return Err(invalid_data(format!(
            "{description} {} does not match its exact v1 format",
            path.display()
        )));
    }
    Ok(())
}

fn read_limited_file(path: &Path, description: &str) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_regular_file(path, description)?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| {
            contextual(
                err,
                format!("failed to read {description} {}", path.display()),
            )
        })?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid_data(format!(
            "{description} {} exceeds {MAX_RECORD_BYTES} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("managed Herdr path contains NUL: {}", path.display()),
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn contextual(error: io::Error, context: String) -> io::Error {
    io::Error::new(error.kind(), format!("{context}: {error}"))
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
