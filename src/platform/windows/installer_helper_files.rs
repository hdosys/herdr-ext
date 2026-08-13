use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, OpenOptionsExt as _},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use sha2::{Digest as _, Sha256};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    Globalization::{CompareStringOrdinal, CSTR_EQUAL},
    Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_WRITE_THROUGH,
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, REPLACEFILE_WRITE_THROUGH, SYNCHRONIZE,
    },
    System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION},
};

pub(crate) use crate::managed_install::{MANAGED_BIN_MARKER, NATIVE_HELPER_NAME};
use crate::{
    managed_install::{
        BuildId, LAUNCHER_BUILD_ID_QUERY_ARG, PENDING_LAUNCHER_PREFIX, PENDING_LAUNCHER_SUFFIX,
        POINTER_RECORD_HEADER, RUNTIME_RECORD_HEADER,
    },
    windows_process_job::ChildProcessJob,
};

pub(crate) const INSTALL_MANIFEST_HEADER: &str = "herdr-install-manifest-v1";
pub(crate) const RUNTIME_MANIFEST_HEADER: &str = "herdr-runtime-manifest-v1";
pub(crate) const PATH_ADD_PENDING_EXISTING_VALUE: &[u8] =
    b"herdr-path-add-pending-v1\nvalue_created=0\n";
pub(crate) const PATH_ADD_PENDING_CREATED_VALUE: &[u8] =
    b"herdr-path-add-pending-v1\nvalue_created=1\n";
pub(crate) const UNINSTALL_MARKER: &[u8] = b"herdr-uninstall-v1\n";
pub(crate) const QUIET_UNINSTALL_PENDING: &[u8] = b"herdr-quiet-uninstall-v1\nstatus=pending\n";
pub(crate) const QUIET_UNINSTALL_SUCCESS: &[u8] = b"herdr-quiet-uninstall-v1\nexit_code=0\n";
pub(crate) const QUIET_UNINSTALL_FAILURE: &[u8] = b"herdr-quiet-uninstall-v1\nexit_code=1\n";
pub(crate) const LAUNCHER_REPLACEMENT_NAME: &str = "herdr.exe.new";

static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstallManifest {
    pub(crate) bootstrap_sha256: String,
    pub(crate) display_version: String,
    pub(crate) numeric_version: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingLauncher {
    pub(crate) path: PathBuf,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TreeEntry {
    pub(crate) path: PathBuf,
    pub(crate) directory: bool,
}

pub(crate) fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn incompatible_installation() -> io::Error {
    invalid_data(
        "An existing Herdr installation was created by an older setup and cannot be updated directly. Uninstall Herdr or Herdr Win from Windows Settings > Apps > Installed apps, then run this setup again. Your existing installation was not changed.",
    )
}

pub(crate) fn contextual(err: io::Error, context: impl Into<String>) -> io::Error {
    io::Error::new(err.kind(), format!("{}: {err}", context.into()))
}

pub(crate) fn full_path(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(invalid_data("a required filesystem path is empty"));
    }
    let full = std::path::absolute(path).map_err(|err| {
        contextual(
            err,
            format!("failed to resolve full path for {}", path.display()),
        )
    })?;
    if full.parent().is_none() {
        return Err(invalid_data(format!(
            "refusing to use a volume root as a Herdr path: {}",
            full.display()
        )));
    }
    Ok(full)
}

fn wide_without_nul(value: &OsStr) -> Vec<u16> {
    value.encode_wide().collect()
}

fn wide_with_nul(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide = wide_without_nul(value);
    if wide.contains(&0) {
        return Err(invalid_data("Windows path contains an embedded NUL"));
    }
    wide.push(0);
    Ok(wide)
}

pub(crate) fn os_eq_ignore_case(left: &OsStr, right: &OsStr) -> bool {
    let left = wide_without_nul(left);
    let right = wide_without_nul(right);
    if left.len() > i32::MAX as usize || right.len() > i32::MAX as usize {
        return false;
    }
    // SAFETY: both pointers remain valid for their explicit lengths and the API
    // performs an ordinal comparison without requiring NUL termination.
    unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        ) == CSTR_EQUAL
    }
}

pub(crate) fn path_eq(left: &Path, right: &Path) -> io::Result<bool> {
    let left = full_path(left)?;
    let right = full_path(right)?;
    Ok(os_eq_ignore_case(left.as_os_str(), right.as_os_str()))
}

pub(crate) fn path_within(path: &Path, root: &Path) -> io::Result<bool> {
    let path = full_path(path)?;
    let root = full_path(root)?;
    let path_components = path.components().collect::<Vec<_>>();
    let root_components = root.components().collect::<Vec<_>>();
    if root_components.len() > path_components.len() {
        return Ok(false);
    }
    Ok(root_components
        .iter()
        .zip(path_components.iter())
        .all(|(left, right)| os_eq_ignore_case(left.as_os_str(), right.as_os_str())))
}

pub(crate) fn is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

pub(crate) fn assert_regular_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        contextual(
            err,
            format!("required regular file is missing: {}", path.display()),
        )
    })?;
    if !metadata.is_file() || is_reparse(&metadata) {
        return Err(invalid_data(format!(
            "required path is not a regular non-reparse file: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn assert_regular_dir(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        contextual(
            err,
            format!("required regular directory is missing: {}", path.display()),
        )
    })?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err(invalid_data(format!(
            "required path is not a regular non-reparse directory: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn path_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(contextual(
            err,
            format!("failed to inspect path {}", path.display()),
        )),
    }
}

pub(crate) fn safe_tree_entries(root: &Path) -> io::Result<Vec<TreeEntry>> {
    assert_regular_dir(root)?;
    let mut pending = vec![root.to_path_buf()];
    let mut output = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|err| {
            contextual(
                err,
                format!(
                    "failed to enumerate managed directory {}",
                    directory.display()
                ),
            )
        })? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if is_reparse(&metadata) {
                return Err(invalid_data(format!(
                    "refusing a reparse point inside managed content: {}",
                    path.display()
                )));
            }
            if !metadata.is_file() && !metadata.is_dir() {
                return Err(invalid_data(format!(
                    "managed content is neither a file nor directory: {}",
                    path.display()
                )));
            }
            let directory = metadata.is_dir();
            output.push(TreeEntry {
                path: path.clone(),
                directory,
            });
            if directory {
                pending.push(path);
            }
        }
    }
    Ok(output)
}

pub(crate) fn relative_path(root: &Path, path: &Path) -> io::Result<PathBuf> {
    if !path_within(path, root)? || path_eq(path, root)? {
        return Err(invalid_data(format!(
            "path escaped its expected root: {}",
            path.display()
        )));
    }
    let root = full_path(root)?;
    let path = full_path(path)?;
    let count = root.components().count();
    Ok(path.components().skip(count).collect())
}

pub(crate) fn read_strict_utf8(path: &Path) -> io::Result<String> {
    assert_regular_file(path)?;
    let bytes = fs::read(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|err| {
        invalid_data(format!(
            "managed text file is not strict UTF-8 {}: {err}",
            path.display()
        ))
    })?;
    Ok(text.to_string())
}

pub(crate) fn sha256(path: &Path) -> io::Result<String> {
    assert_regular_file(path)?;
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

pub(crate) fn normalized_text_sha256(path: &Path) -> io::Result<String> {
    let text = read_strict_utf8(path)?.replace("\r\n", "\n");
    if text.contains('\r') {
        return Err(invalid_data(format!(
            "managed text contains unsupported carriage returns: {}",
            path.display()
        )));
    }
    Ok(format!("{:x}", Sha256::digest(text.as_bytes())))
}

pub(crate) fn unique_hex() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
    format!(
        "{:032x}",
        now ^ (counter << 64) ^ std::process::id() as u128
    )
}

pub(crate) fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_WRITE_THROUGH);
    let mut file = options.open(path).map_err(|err| {
        contextual(
            err,
            format!("failed to create durable file {}", path.display()),
        )
    })?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn copy_durable_file(source: &Path, destination: &Path) -> io::Result<()> {
    assert_regular_file(source)?;
    let mut input = File::open(source)?;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_WRITE_THROUGH);
    let mut output = options.open(destination).map_err(|err| {
        contextual(
            err,
            format!("failed to create copied file {}", destination.display()),
        )
    })?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(())
}

pub(crate) fn copy_durable_tree(source: &Path, destination: &Path) -> io::Result<()> {
    let mut entries = safe_tree_entries(source)?;
    fs::create_dir(destination)?;
    entries.sort_by_key(|entry| entry.path.components().count());
    for entry in entries.iter().filter(|entry| entry.directory) {
        fs::create_dir(destination.join(relative_path(source, &entry.path)?))?;
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    for entry in entries.iter().filter(|entry| !entry.directory) {
        copy_durable_file(
            &entry.path,
            &destination.join(relative_path(source, &entry.path)?),
        )?;
    }
    Ok(())
}

pub(crate) fn remove_validated_directory(path: &Path) -> io::Result<()> {
    let mut entries = safe_tree_entries(path)?;
    for entry in entries.iter().filter(|entry| !entry.directory) {
        fs::remove_file(&entry.path)?;
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.path.components().count()));
    for entry in entries.iter().filter(|entry| entry.directory) {
        fs::remove_dir(&entry.path)?;
    }
    fs::remove_dir(path)
}

pub(crate) fn publish_file(source: &Path, destination: &Path, backup_dir: &Path) -> io::Result<()> {
    assert_regular_file(source)?;
    assert_regular_dir(backup_dir)?;
    if path_exists(destination)? {
        assert_regular_file(destination)?;
        let backup = backup_dir.join(format!(
            "{}.backup.{}",
            destination
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("managed-file"),
            unique_hex()
        ));
        replace_file(destination, source, Some(&backup))?;
        fs::remove_file(backup)?;
    } else {
        fs::rename(source, destination)?;
    }
    Ok(())
}

pub(crate) fn replace_file(
    destination: &Path,
    replacement: &Path,
    backup: Option<&Path>,
) -> io::Result<()> {
    let destination_wide = wide_with_nul(destination.as_os_str())?;
    let replacement_wide = wide_with_nul(replacement.as_os_str())?;
    let backup_wide = backup
        .map(|value| wide_with_nul(value.as_os_str()))
        .transpose()?;
    // SAFETY: all buffers are valid NUL-terminated UTF-16 for this call.
    let result = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            replacement_wide.as_ptr(),
            backup_wide
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if result == 0 {
        return Err(contextual(
            io::Error::last_os_error(),
            format!(
                "failed to replace {} with {}",
                destination.display(),
                replacement.display()
            ),
        ));
    }
    Ok(())
}

pub(crate) fn move_replace(source: &Path, destination: &Path) -> io::Result<()> {
    let source_wide = wide_with_nul(source.as_os_str())?;
    let destination_wide = wide_with_nul(destination.as_os_str())?;
    // SAFETY: both buffers are valid NUL-terminated UTF-16 for this call.
    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(contextual(
            io::Error::last_os_error(),
            format!(
                "failed to move {} to {}",
                source.display(),
                destination.display()
            ),
        ));
    }
    Ok(())
}

pub(crate) fn pointer_text(build_id: &BuildId) -> String {
    format!("{POINTER_RECORD_HEADER}\nbuild_id={}\n", build_id.as_str())
}

pub(crate) fn runtime_ready_text(build_id: &BuildId) -> String {
    format!("{RUNTIME_RECORD_HEADER}\nbuild_id={}\n", build_id.as_str())
}

pub(crate) fn runtime_manifest_text(runtime_root: &Path) -> io::Result<String> {
    let mut paths = Vec::new();
    for entry in safe_tree_entries(runtime_root)? {
        if entry.directory || entry.path.file_name() == Some(OsStr::new("runtime.manifest")) {
            continue;
        }
        let relative = relative_path(runtime_root, &entry.path)?
            .to_string_lossy()
            .replace('\\', "/");
        validate_manifest_relative_path(&relative)?;
        paths.push(relative);
    }
    paths.sort();
    let mut output = format!("{RUNTIME_MANIFEST_HEADER}\n");
    for relative in paths {
        let hash = sha256(&runtime_root.join(relative.replace('/', "\\")))?;
        output.push_str(&format!("{hash}  {relative}\n"));
    }
    Ok(output)
}

fn validate_manifest_relative_path(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains("../")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
    {
        return Err(invalid_data(format!(
            "runtime manifest cannot represent path {value:?}"
        )));
    }
    Ok(())
}

pub(crate) fn read_runtime_manifest(runtime_root: &Path) -> io::Result<BTreeMap<String, String>> {
    let path = runtime_root.join("runtime.manifest");
    let text = read_strict_utf8(&path)?;
    if !text.ends_with('\n') {
        return Err(invalid_data(format!(
            "runtime ownership manifest lacks its final newline: {}",
            path.display()
        )));
    }
    let mut lines = text[..text.len() - 1].split('\n');
    if lines.next() != Some(RUNTIME_MANIFEST_HEADER) {
        return Err(invalid_data(format!(
            "invalid runtime ownership manifest header: {}",
            path.display()
        )));
    }
    let mut output = BTreeMap::new();
    for line in lines {
        let (hash, relative) = line.split_once("  ").ok_or_else(|| {
            invalid_data(format!(
                "invalid runtime ownership entry in {}",
                path.display()
            ))
        })?;
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(invalid_data(format!(
                "invalid runtime ownership hash in {}",
                path.display()
            )));
        }
        validate_manifest_relative_path(relative)?;
        if relative == "runtime.manifest"
            || output
                .insert(relative.to_string(), hash.to_string())
                .is_some()
        {
            return Err(invalid_data(format!(
                "duplicate or self-referential runtime ownership path in {}",
                path.display()
            )));
        }
    }
    if output.is_empty() {
        return Err(invalid_data(format!(
            "runtime ownership manifest has no files: {}",
            path.display()
        )));
    }
    Ok(output)
}

pub(crate) fn validate_runtime_directory(path: &Path, expected: &BuildId) -> io::Result<()> {
    let entries = safe_tree_entries(path)?;
    let marker = crate::managed_install::parse_record(
        &fs::read(path.join("runtime.ready"))?,
        RUNTIME_RECORD_HEADER,
        &path.join("runtime.ready"),
    )?;
    if marker != *expected {
        return Err(invalid_data(format!(
            "runtime marker names {}, expected {}",
            marker.as_str(),
            expected.as_str()
        )));
    }
    assert_regular_file(&path.join("herdr.exe"))?;
    let manifest = read_runtime_manifest(path)?;
    if manifest
        .keys()
        .any(|relative| Path::new(relative).file_name() == Some(OsStr::new("herdr-launcher.exe")))
    {
        return Err(invalid_data(format!(
            "runtime contains the obsolete launcher hop: {}",
            path.display()
        )));
    }
    let actual_files = entries
        .iter()
        .filter(|entry| !entry.directory)
        .map(|entry| {
            relative_path(path, &entry.path).map(|value| value.to_string_lossy().replace('\\', "/"))
        })
        .collect::<io::Result<BTreeSet<_>>>()?;
    let mut expected_files = manifest.keys().cloned().collect::<BTreeSet<_>>();
    expected_files.insert("runtime.manifest".to_string());
    if actual_files != expected_files {
        return Err(invalid_data(format!(
            "runtime owned-file set differs from its manifest: {}",
            path.display()
        )));
    }
    let mut expected_dirs = BTreeSet::new();
    for relative in manifest.keys() {
        let mut parent = Path::new(relative).parent();
        while let Some(value) = parent {
            if value.as_os_str().is_empty() {
                break;
            }
            expected_dirs.insert(value.to_string_lossy().replace('\\', "/"));
            parent = value.parent();
        }
    }
    let actual_dirs = entries
        .iter()
        .filter(|entry| entry.directory)
        .map(|entry| {
            relative_path(path, &entry.path).map(|value| value.to_string_lossy().replace('\\', "/"))
        })
        .collect::<io::Result<BTreeSet<_>>>()?;
    if actual_dirs != expected_dirs {
        return Err(invalid_data(format!(
            "runtime directory set differs from its manifest: {}",
            path.display()
        )));
    }
    for (relative, expected_hash) in manifest {
        let actual = sha256(&path.join(relative.replace('/', "\\")))?;
        if actual != expected_hash {
            return Err(invalid_data(format!(
                "runtime owned-file hash mismatch for {relative} in {}",
                path.display()
            )));
        }
    }
    Ok(())
}

pub(crate) fn create_runtime_tree(
    destination: &Path,
    stage: &Path,
    build_id: &BuildId,
) -> io::Result<()> {
    if path_exists(destination)? {
        return Err(invalid_data(format!(
            "runtime staging destination already exists: {}",
            destination.display()
        )));
    }
    copy_durable_tree(stage, destination)?;
    write_durable(
        &destination.join("runtime.ready"),
        runtime_ready_text(build_id).as_bytes(),
    )?;
    let manifest = runtime_manifest_text(destination)?;
    write_durable(&destination.join("runtime.manifest"), manifest.as_bytes())?;
    validate_runtime_directory(destination, build_id)
}

pub(crate) fn install_manifest_text(
    bootstrap_sha256: &str,
    display_version: &str,
    numeric_version: &str,
) -> io::Result<String> {
    if bootstrap_sha256.len() != 64
        || !bootstrap_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid_data("invalid managed launcher SHA-256"));
    }
    validate_version_identity(display_version, numeric_version)?;
    Ok(format!(
        "{INSTALL_MANIFEST_HEADER}\nbootstrap_sha256={bootstrap_sha256}\ndisplay_version={display_version}\nnumeric_version={numeric_version}\n"
    ))
}

pub(crate) fn read_install_manifest(path: &Path) -> io::Result<InstallManifest> {
    let text = read_strict_utf8(path)?;
    let mut lines = text.split_terminator('\n');
    if !text.ends_with('\n') || lines.next() != Some(INSTALL_MANIFEST_HEADER) {
        return Err(invalid_data(format!(
            "invalid managed install ownership manifest: {}",
            path.display()
        )));
    }
    let bootstrap_sha256 = lines
        .next()
        .and_then(|line| line.strip_prefix("bootstrap_sha256="))
        .ok_or_else(|| invalid_data("managed install manifest lacks bootstrap hash"))?;
    let display_version = lines
        .next()
        .and_then(|line| line.strip_prefix("display_version="))
        .ok_or_else(|| invalid_data("managed install manifest lacks display version"))?;
    let numeric_version = lines
        .next()
        .and_then(|line| line.strip_prefix("numeric_version="))
        .ok_or_else(|| invalid_data("managed install manifest lacks numeric version"))?;
    if lines.next().is_some()
        || bootstrap_sha256.len() != 64
        || !bootstrap_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid_data(format!(
            "invalid managed install ownership manifest: {}",
            path.display()
        )));
    }
    validate_version_identity(display_version, numeric_version)?;
    Ok(InstallManifest {
        bootstrap_sha256: bootstrap_sha256.to_string(),
        display_version: display_version.to_string(),
        numeric_version: numeric_version.to_string(),
    })
}

fn parse_version_components<const N: usize>(
    value: &str,
    description: &str,
) -> io::Result<[u16; N]> {
    let components = value.split('.').collect::<Vec<_>>();
    if components.len() != N {
        return Err(invalid_data(format!("invalid {description}")));
    }
    let mut parsed = [0u16; N];
    for (slot, component) in parsed.iter_mut().zip(components) {
        if component.is_empty()
            || component.bytes().any(|byte| !byte.is_ascii_digit())
            || (component.len() > 1 && component.starts_with('0'))
        {
            return Err(invalid_data(format!("invalid {description}")));
        }
        *slot = component
            .parse::<u16>()
            .map_err(|_| invalid_data(format!("invalid {description}")))?;
    }
    Ok(parsed)
}

fn parse_release_calver(display: &str) -> io::Result<[u16; 4]> {
    let components = display.split('.').collect::<Vec<_>>();
    if components.len() != 4
        || components[0].len() != 4
        || components[1].len() != 2
        || components[2].len() != 2
        || components[3].is_empty()
        || components
            .iter()
            .any(|component| component.bytes().any(|byte| !byte.is_ascii_digit()))
        || components[3].starts_with('0')
    {
        return Err(invalid_data(format!(
            "invalid herdr-win release version {display:?}"
        )));
    }
    let year = components[0]
        .parse::<u16>()
        .map_err(|_| invalid_data("invalid herdr-win release year"))?;
    let month = components[1]
        .parse::<u16>()
        .map_err(|_| invalid_data("invalid herdr-win release month"))?;
    let day = components[2]
        .parse::<u16>()
        .map_err(|_| invalid_data("invalid herdr-win release day"))?;
    let sequence = components[3]
        .parse::<u16>()
        .map_err(|_| invalid_data("invalid herdr-win release sequence"))?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if year == 0 || day == 0 || day > max_day || sequence == 0 {
        return Err(invalid_data(format!(
            "invalid herdr-win release version {display:?}"
        )));
    }
    Ok([year, month, day, sequence])
}

pub(crate) fn parse_display_version(display: &str) -> io::Result<[u16; 4]> {
    if let Some(build) = display.strip_prefix("local.") {
        BuildId::parse(build)?;
        return Ok([0, 0, 0, 0]);
    }
    if let Some((freshness, build)) = display
        .strip_suffix(')')
        .and_then(|display| display.split_once(" (local, build "))
    {
        BuildId::parse(build)?;
        let parts = freshness.split('.').collect::<Vec<_>>();
        if parts.len() != 4
            || parts[0].len() != 4
            || parts[1].len() != 2
            || parts[2].len() != 2
            || parts[3].len() != 5
            || !parts[3].is_ascii()
            || !parts[3].ends_with('Z')
            || parts[..3]
                .iter()
                .any(|part| part.bytes().any(|byte| !byte.is_ascii_digit()))
            || parts[3][..4].bytes().any(|byte| !byte.is_ascii_digit())
        {
            return Err(invalid_data("invalid local build freshness"));
        }
        let year = parts[0]
            .parse::<u16>()
            .map_err(|_| invalid_data("invalid local build year"))?;
        let month = parts[1]
            .parse::<u16>()
            .map_err(|_| invalid_data("invalid local build month"))?;
        let day = parts[2]
            .parse::<u16>()
            .map_err(|_| invalid_data("invalid local build day"))?;
        let hour = parts[3][..2]
            .parse::<u16>()
            .map_err(|_| invalid_data("invalid local build hour"))?;
        let minute = parts[3][2..4]
            .parse::<u16>()
            .map_err(|_| invalid_data("invalid local build minute"))?;
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let max_day = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 0,
        };
        if year == 0 || day == 0 || day > max_day || hour >= 24 || minute >= 60 {
            return Err(invalid_data("invalid local build freshness"));
        }
        return Ok([year, month, day, hour * 100 + minute]);
    }
    if let Some((semver, build)) = display.split_once("-preview.") {
        BuildId::parse(build)?;
        let parts = parse_version_components::<3>(semver, "display semantic version")?;
        return Ok([parts[0], parts[1], parts[2], 0]);
    }
    parse_release_calver(display)
}

pub(crate) fn validate_version_identity(display: &str, numeric: &str) -> io::Result<()> {
    let display_parts = parse_display_version(display)?;
    let numeric_parts = parse_version_components::<4>(numeric, "numeric version")?;
    if display_parts != numeric_parts {
        return Err(invalid_data(format!(
            "numeric version {numeric:?} does not match display version {display:?}"
        )));
    }
    Ok(())
}

pub(crate) fn query_launcher_build_id(path: &Path, timeout: Duration) -> io::Result<BuildId> {
    assert_regular_file(path)?;
    let job = ChildProcessJob::new_kill_on_close()?;
    let mut child = Command::new(path)
        .arg(LAUNCHER_BUILD_ID_QUERY_ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| contextual(err, format!("failed to query launcher {}", path.display())))?;
    if let Err(err) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(contextual(
            err,
            format!("failed to contain launcher query {}", path.display()),
        ));
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            job.terminate_and_wait(&mut child, Duration::from_secs(5))?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("launcher build-ID query timed out: {}", path.display()),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_end(&mut stdout)?;
    }
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_end(&mut stderr)?;
    }
    if !status.success() || !stderr.is_empty() {
        return Err(invalid_data(format!(
            "launcher build-ID query failed for {} (exit {:?}): {}",
            path.display(),
            status.code(),
            String::from_utf8_lossy(&stderr)
        )));
    }
    let output = std::str::from_utf8(&stdout)
        .map_err(|_| invalid_data("launcher build-ID output is not UTF-8"))?
        .trim_end_matches(['\r', '\n']);
    BuildId::parse(output)
}

pub(crate) fn pending_launcher(state_dir: &Path) -> io::Result<Option<PendingLauncher>> {
    assert_regular_dir(state_dir)?;
    let mut found = None;
    for entry in fs::read_dir(state_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| invalid_data("managed state entry name is not UTF-8"))?;
        let Some(candidate) = name.strip_prefix(PENDING_LAUNCHER_PREFIX) else {
            continue;
        };
        let Some(hash) = candidate.strip_suffix(PENDING_LAUNCHER_SUFFIX) else {
            return Err(invalid_data(format!(
                "invalid pending launcher filename: {}",
                entry.path().display()
            )));
        };
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(invalid_data(format!(
                "invalid pending launcher filename: {}",
                entry.path().display()
            )));
        }
        if found.is_some() {
            return Err(invalid_data(
                "managed Herdr state contains more than one pending launcher",
            ));
        }
        assert_regular_file(&entry.path())?;
        if sha256(&entry.path())? != hash {
            return Err(invalid_data(format!(
                "pending launcher hash does not match filename: {}",
                entry.path().display()
            )));
        }
        found = Some(PendingLauncher {
            path: entry.path(),
            sha256: hash.to_string(),
        });
    }
    Ok(found)
}

pub(crate) fn wait_for_process(pid: u32, timeout: Duration) -> io::Result<bool> {
    if pid == 0 {
        return Ok(true);
    }
    // SAFETY: no handle inheritance is requested and only synchronization/query
    // rights are used for the supplied process identifier.
    let handle = unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
            return Ok(true);
        }
        // A process can exit between the caller recording its ID and this open.
        if err.raw_os_error() == Some(87) {
            return Ok(true);
        }
        return Err(contextual(
            err,
            format!("failed to open parent process {pid}"),
        ));
    }
    let guard = OwnedHandle(handle);
    let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
    // SAFETY: `guard` retains a valid process handle for this wait.
    match unsafe { WaitForSingleObject(guard.0, millis) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(contextual(
            io::Error::last_os_error(),
            format!("failed while waiting for parent process {pid}"),
        )),
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: this guard uniquely owns the process handle.
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub(crate) fn sorted_names(path: &Path) -> io::Result<Vec<OsString>> {
    assert_regular_dir(path)?;
    let mut names = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_identity_accepts_release_local_and_predecessor_forms() {
        validate_version_identity("2026.08.11.1", "2026.8.11.1").unwrap();
        validate_version_identity("local.0123456789ab.cdef01234567", "0.0.0.0").unwrap();
        validate_version_identity(
            "2026.08.28.1045Z (local, build 0123456789ab.cdef01234567)",
            "2026.8.28.1045",
        )
        .unwrap();
        validate_version_identity("0.8.0-preview.0123456789ab.cdef01234567", "0.8.0.0").unwrap();
        assert!(
            validate_version_identity("0.8.1-preview.0123456789ab.cdef01234567", "0.8.0.0")
                .is_err()
        );
        for invalid in [
            "2026.8.11.1",
            "2026.02.29.1",
            "2026.08.11.0",
            "2026.02.29.1045Z (local, build 0123456789ab.cdef01234567)",
            "2026.08.28.2460Z (local, build 0123456789ab.cdef01234567)",
            "2026.08.28.1045Z (local, build 0123456789ab.cdef01234567.extra)",
            "00.8.0-preview.0123456789ab.cdef01234567",
            "0.8.0-preview.0123456789ab.cdef01234567.extra",
            "0.8-preview.0123456789ab.cdef01234567",
        ] {
            assert!(
                parse_display_version(invalid).is_err(),
                "accepted {invalid}"
            );
        }
        assert!(
            validate_version_identity("0.8.0-preview.0123456789ab.cdef01234567", "0.08.0.0")
                .is_err()
        );
        assert!(validate_version_identity("2026.08.11.1", "2026.8.11.2").is_err());
    }

    #[test]
    fn malformed_pending_launcher_name_is_not_ignored() {
        let state = std::env::temp_dir().join(format!("herdr-pending-name-{}", unique_hex()));
        fs::create_dir(&state).unwrap();
        let malformed = state.join("launcher.pending-not-a-hash.exe");
        fs::write(&malformed, b"fixture").unwrap();
        let result = pending_launcher(&state);
        fs::remove_file(malformed).unwrap();
        fs::remove_dir(state).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn manifest_paths_are_narrow() {
        for valid in ["herdr.exe", "conpty/x64/OpenConsole.exe", "LICENSE.txt"] {
            validate_manifest_relative_path(valid).unwrap();
        }
        for invalid in ["", "/root", "../escape", "a/../b", "space name"] {
            assert!(validate_manifest_relative_path(invalid).is_err());
        }
    }

    #[test]
    fn incompatible_installation_error_gives_the_next_action() {
        let message = incompatible_installation().to_string();
        assert!(message.contains("Windows Settings > Apps > Installed apps"));
        assert!(message.contains("then run this setup again"));
        assert!(message.contains("was not changed"));
    }
}
