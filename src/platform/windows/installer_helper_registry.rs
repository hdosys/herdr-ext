use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    io,
    os::windows::ffi::OsStrExt as _,
    path::{Path, PathBuf},
};

use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::{
        Environment::ExpandEnvironmentStringsW,
        Registry::{
            RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegEnumValueW,
            RegOpenKeyExW, RegQueryInfoKeyW, RegQueryValueExW, RegSetValueExW, HKEY,
            HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_CREATED_NEW_KEY, REG_DWORD, REG_EXPAND_SZ,
            REG_OPTION_NON_VOLATILE, REG_SZ,
        },
    },
};

use super::installer_helper_files::{
    assert_regular_file, full_path, invalid_data, os_eq_ignore_case, parse_display_version,
    path_eq, path_exists, write_durable, NATIVE_HELPER_NAME, PATH_ADD_PENDING_CREATED_VALUE,
    PATH_ADD_PENDING_EXISTING_VALUE,
};

const PRODUCT_NAME: &str = "Herdr Win";
const PUBLISHER: &str = "herdr-win";
const ARP_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Herdr Win";
const ENVIRONMENT_SUBKEY: &str = "Environment";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PathUpdate {
    pub(crate) changed: bool,
    pub(crate) owned: bool,
    pub(crate) value_created: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PathOwnership {
    pub(crate) owned: bool,
    pub(crate) value_created: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArpPathOwnership {
    pub(crate) ownership: PathOwnership,
    pub(crate) value_created_present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RegistryValue {
    String { value: String, kind: u32 },
    Dword(u32),
}

#[derive(Clone, Debug)]
pub(crate) struct PathRollback {
    before: String,
    after: Option<String>,
    kind: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct ArpSnapshot {
    install_root: PathBuf,
    values: BTreeMap<String, RegistryValue>,
}

struct RegistryKey(HKEY);

impl RegistryKey {
    fn open_optional(root: HKEY, subkey: &str, access: u32) -> io::Result<Option<Self>> {
        let subkey = wide(subkey)?;
        let mut handle = std::ptr::null_mut();
        // SAFETY: subkey is NUL-terminated and the output pointer is valid.
        let result = unsafe { RegOpenKeyExW(root, subkey.as_ptr(), 0, access, &mut handle) };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check_registry(result, "open registry key")?;
        Ok(Some(Self(handle)))
    }

    fn create(root: HKEY, subkey: &str) -> io::Result<Self> {
        Self::create_checked(root, subkey, false)
    }

    fn create_new(root: HKEY, subkey: &str) -> io::Result<Self> {
        Self::create_checked(root, subkey, true)
    }

    fn create_checked(root: HKEY, subkey: &str, require_new: bool) -> io::Result<Self> {
        let subkey = wide(subkey)?;
        let mut handle = std::ptr::null_mut();
        let mut disposition = 0;
        // SAFETY: all pointers are valid for the immediate create call.
        let result = unsafe {
            RegCreateKeyExW(
                root,
                subkey.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                std::ptr::null(),
                &mut handle,
                &mut disposition,
            )
        };
        check_registry(result, "create registry key")?;
        let key = Self(handle);
        if require_new && disposition != REG_CREATED_NEW_KEY {
            return Err(invalid_data(
                "registry key appeared before installer rollback",
            ));
        }
        Ok(key)
    }

    fn query(&self, name: &str) -> io::Result<Option<RegistryValue>> {
        let name = wide(name)?;
        let mut kind = 0;
        let mut size = 0;
        // SAFETY: this first call requests only the value type and size.
        let result = unsafe {
            RegQueryValueExW(
                self.0,
                name.as_ptr(),
                std::ptr::null(),
                &mut kind,
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check_registry(result, "query registry value size")?;
        let mut data = vec![0u8; size as usize];
        // SAFETY: data is writable for the size returned by the preceding call.
        check_registry(
            unsafe {
                RegQueryValueExW(
                    self.0,
                    name.as_ptr(),
                    std::ptr::null(),
                    &mut kind,
                    data.as_mut_ptr(),
                    &mut size,
                )
            },
            "read registry value",
        )?;
        data.truncate(size as usize);
        match kind {
            REG_SZ | REG_EXPAND_SZ => {
                if !data.len().is_multiple_of(2) {
                    return Err(invalid_data("registry string has an odd byte length"));
                }
                let mut words = data
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>();
                if words.last() == Some(&0) {
                    words.pop();
                }
                if words.contains(&0) {
                    return Err(invalid_data("registry string contains an embedded NUL"));
                }
                Ok(Some(RegistryValue::String {
                    value: String::from_utf16(&words)
                        .map_err(|_| invalid_data("registry string is not valid UTF-16"))?,
                    kind,
                }))
            }
            REG_DWORD => {
                if data.len() != 4 {
                    return Err(invalid_data("registry DWORD has the wrong size"));
                }
                Ok(Some(RegistryValue::Dword(u32::from_le_bytes([
                    data[0], data[1], data[2], data[3],
                ]))))
            }
            other => Err(invalid_data(format!(
                "registry value has unsupported type {other}"
            ))),
        }
    }

    fn set_string(&self, name: &str, value: &str, kind: u32) -> io::Result<()> {
        if kind != REG_SZ && kind != REG_EXPAND_SZ {
            return Err(invalid_data(
                "registry string kind must be REG_SZ or REG_EXPAND_SZ",
            ));
        }
        let name = wide(name)?;
        let value = wide(value)?;
        // SAFETY: both buffers are valid and value length is expressed in bytes.
        check_registry(
            unsafe {
                RegSetValueExW(
                    self.0,
                    name.as_ptr(),
                    0,
                    kind,
                    value.as_ptr().cast(),
                    (value.len() * 2) as u32,
                )
            },
            "write registry string",
        )
    }

    fn set_dword(&self, name: &str, value: u32) -> io::Result<()> {
        let name = wide(name)?;
        let bytes = value.to_le_bytes();
        // SAFETY: name and the four-byte DWORD buffer are valid.
        check_registry(
            unsafe {
                RegSetValueExW(
                    self.0,
                    name.as_ptr(),
                    0,
                    REG_DWORD,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                )
            },
            "write registry DWORD",
        )
    }

    fn set_value(&self, name: &str, value: &RegistryValue) -> io::Result<()> {
        match value {
            RegistryValue::String { value, kind } => self.set_string(name, value, *kind),
            RegistryValue::Dword(value) => self.set_dword(name, *value),
        }
    }

    fn delete_value(&self, name: &str) -> io::Result<()> {
        let name = wide(name)?;
        // SAFETY: name is a valid NUL-terminated registry value name.
        check_registry(
            unsafe { RegDeleteValueW(self.0, name.as_ptr()) },
            "delete registry value",
        )
    }

    fn value_names(&self) -> io::Result<BTreeSet<String>> {
        let (values, max_name, subkeys) = self.info()?;
        if subkeys != 0 {
            return Err(invalid_data("managed registry key contains subkeys"));
        }
        let mut output = BTreeSet::new();
        for index in 0..values {
            let mut buffer = vec![0u16; max_name as usize + 2];
            let mut length = buffer.len() as u32;
            // SAFETY: buffer and length are valid writable outputs.
            check_registry(
                unsafe {
                    RegEnumValueW(
                        self.0,
                        index,
                        buffer.as_mut_ptr(),
                        &mut length,
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                "enumerate registry value",
            )?;
            output.insert(
                String::from_utf16(&buffer[..length as usize])
                    .map_err(|_| invalid_data("registry value name is not valid UTF-16"))?,
            );
        }
        Ok(output)
    }

    fn info(&self) -> io::Result<(u32, u32, u32)> {
        let mut subkeys = 0;
        let mut values = 0;
        let mut max_name = 0;
        // SAFETY: only the documented count outputs used below are non-null.
        check_registry(
            unsafe {
                RegQueryInfoKeyW(
                    self.0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &mut subkeys,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut values,
                    &mut max_name,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            "inspect registry key",
        )?;
        Ok((values, max_name, subkeys))
    }
}

impl Drop for RegistryKey {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this wrapper uniquely owns the opened registry handle.
            unsafe { RegCloseKey(self.0) };
        }
    }
}

fn wide(value: &str) -> io::Result<Vec<u16>> {
    let mut output = OsStr::new(value).encode_wide().collect::<Vec<_>>();
    if output.contains(&0) {
        return Err(invalid_data("registry text contains an embedded NUL"));
    }
    output.push(0);
    Ok(output)
}

fn check_registry(result: u32, operation: &str) -> io::Result<()> {
    if result == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{operation} failed with Windows error {result}"
        )))
    }
}

fn expand_environment(value: &str) -> io::Result<String> {
    let source = wide(value)?;
    // SAFETY: source is NUL-terminated; a null destination asks for size.
    let required = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), std::ptr::null_mut(), 0) };
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut output = vec![0u16; required as usize];
    // SAFETY: output has the capacity returned by the first call.
    let written = unsafe {
        ExpandEnvironmentStringsW(source.as_ptr(), output.as_mut_ptr(), output.len() as u32)
    };
    if written == 0 || written > output.len() as u32 {
        return Err(io::Error::last_os_error());
    }
    output.truncate(written.saturating_sub(1) as usize);
    String::from_utf16(&output).map_err(|_| invalid_data("expanded PATH entry is not UTF-16"))
}

fn comparable_path(value: &str, expand: bool) -> String {
    let trimmed = value.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(trimmed);
    let expanded = if expand {
        expand_environment(unquoted).unwrap_or_else(|_| unquoted.to_string())
    } else {
        unquoted.to_string()
    };
    full_path(Path::new(&expanded))
        .map(|value| value.to_string_lossy().trim_end_matches('\\').to_string())
        .unwrap_or_else(|_| expanded.trim_end_matches('\\').to_string())
}

fn path_entry_equal(left: &str, right: &str, expand: bool) -> bool {
    os_eq_ignore_case(
        OsStr::new(&comparable_path(left, expand)),
        OsStr::new(&comparable_path(right, expand)),
    )
}

fn raw_owned_path_entry_equal(candidate: &str, owned_entry: &str) -> bool {
    let trimmed = candidate.trim();
    if trimmed.contains('%') && !owned_entry.contains('%') {
        return false;
    }
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(trimmed);
    if !Path::new(unquoted).is_absolute() {
        return false;
    }
    let normalize = |value: &str| value.replace('/', "\\").trim_end_matches('\\').to_string();
    os_eq_ignore_case(
        OsStr::new(&normalize(unquoted)),
        OsStr::new(&normalize(owned_entry)),
    )
}

fn remove_raw_owned_path_entries(segments: &mut Vec<String>, owned_entry: &str) -> usize {
    let before = segments.len();
    segments.retain(|segment| !raw_owned_path_entry_equal(segment, owned_entry));
    before - segments.len()
}

pub(crate) fn add_user_path(
    bin_dir: &Path,
    previous_ownership: PathOwnership,
    pending_marker: &Path,
) -> io::Result<PathUpdate> {
    Ok(update_user_path(bin_dir, true, previous_ownership, Some(pending_marker))?.0)
}

pub(crate) fn remove_user_path(
    bin_dir: &Path,
    ownership: PathOwnership,
) -> io::Result<(PathUpdate, Option<PathRollback>)> {
    update_user_path(bin_dir, false, ownership, None)
}

pub(crate) fn path_add_pending_value_created(path: &Path) -> io::Result<Option<bool>> {
    if !path_exists(path)? {
        return Ok(None);
    }
    assert_regular_file(path)?;
    match std::fs::read(path)?.as_slice() {
        PATH_ADD_PENDING_EXISTING_VALUE => Ok(Some(false)),
        PATH_ADD_PENDING_CREATED_VALUE => Ok(Some(true)),
        _ => Err(invalid_data("PATH ownership-intent marker is invalid")),
    }
}

pub(crate) fn exact_user_path_entry_exists(bin_dir: &Path) -> io::Result<bool> {
    let Some(key) = RegistryKey::open_optional(HKEY_CURRENT_USER, ENVIRONMENT_SUBKEY, KEY_READ)?
    else {
        return Ok(false);
    };
    let entry = full_path(bin_dir)?.to_string_lossy().to_string();
    match key.query("Path")? {
        Some(RegistryValue::String { value, .. }) => {
            Ok(value.split(';').any(|segment| segment == entry))
        }
        Some(_) => Err(invalid_data("current-user Path is not a string")),
        None => Ok(false),
    }
}

pub(crate) fn restore_user_path(rollback: &PathRollback) -> io::Result<()> {
    let Some(key) =
        RegistryKey::open_optional(HKEY_CURRENT_USER, ENVIRONMENT_SUBKEY, KEY_READ | KEY_WRITE)?
    else {
        return Err(invalid_data(
            "current-user Environment key disappeared during PATH rollback",
        ));
    };
    let current = key.query("Path")?;
    let expected = rollback.after.as_ref().map(|value| RegistryValue::String {
        value: value.clone(),
        kind: rollback.kind,
    });
    if current != expected {
        return Err(invalid_data(
            "current-user PATH changed before installer rollback",
        ));
    }
    key.set_string("Path", &rollback.before, rollback.kind)
}

fn update_user_path(
    bin_dir: &Path,
    add: bool,
    ownership: PathOwnership,
    pending_marker: Option<&Path>,
) -> io::Result<(PathUpdate, Option<PathRollback>)> {
    if ownership.value_created && !ownership.owned {
        return Err(invalid_data(
            "PATH value ownership requires installer entry ownership",
        ));
    }
    if !add && !ownership.owned {
        return Ok((
            PathUpdate {
                changed: false,
                owned: false,
                value_created: false,
            },
            None,
        ));
    }
    let key = if add {
        match RegistryKey::open_optional(
            HKEY_CURRENT_USER,
            ENVIRONMENT_SUBKEY,
            KEY_READ | KEY_WRITE,
        )? {
            Some(key) => key,
            None => RegistryKey::create(HKEY_CURRENT_USER, ENVIRONMENT_SUBKEY)?,
        }
    } else {
        let Some(key) = RegistryKey::open_optional(
            HKEY_CURRENT_USER,
            ENVIRONMENT_SUBKEY,
            KEY_READ | KEY_WRITE,
        )?
        else {
            return Ok((
                PathUpdate {
                    changed: false,
                    owned: false,
                    value_created: false,
                },
                None,
            ));
        };
        key
    };
    let entry = full_path(bin_dir)?.to_string_lossy().to_string();
    let current = key.query("Path")?;
    let value_missing = current.is_none();
    let (value, kind) = match current {
        Some(RegistryValue::String { value, kind }) => (value, kind),
        Some(_) => return Err(invalid_data("current-user Path is not a string")),
        None if add => (String::new(), REG_EXPAND_SZ),
        None => {
            return Ok((
                PathUpdate {
                    changed: false,
                    owned: false,
                    value_created: false,
                },
                None,
            ))
        }
    };
    let mut segments = value.split(';').map(str::to_string).collect::<Vec<_>>();
    if add {
        if segments
            .iter()
            .any(|segment| path_entry_equal(segment, &entry, kind == REG_EXPAND_SZ))
        {
            let exact = segments.iter().any(|segment| segment == &entry);
            if ownership.owned && ownership.value_created && exact && kind != REG_EXPAND_SZ {
                return Err(invalid_data(
                    "installer-created PATH value changed type before setup recovery",
                ));
            }
            return Ok((
                PathUpdate {
                    changed: false,
                    owned: ownership.owned && exact,
                    value_created: ownership.owned && exact && ownership.value_created,
                },
                None,
            ));
        }
        let updated = if value.is_empty() {
            entry
        } else {
            format!("{entry};{value}")
        };
        let pending_marker = pending_marker.ok_or_else(|| {
            invalid_data("PATH addition requires a durable ownership-intent marker")
        })?;
        let value_created = ownership.value_created || value_missing;
        if let Some(pending_value_created) = path_add_pending_value_created(pending_marker)? {
            if pending_value_created != value_created
                || (pending_value_created && !value_missing)
                || (!pending_value_created && value_missing)
            {
                return Err(invalid_data(
                    "current-user PATH changed after installer ownership intent",
                ));
            }
        } else {
            write_durable(
                pending_marker,
                if value_created {
                    PATH_ADD_PENDING_CREATED_VALUE
                } else {
                    PATH_ADD_PENDING_EXISTING_VALUE
                },
            )?;
        }
        key.set_string("Path", &updated, kind)?;
        return Ok((
            PathUpdate {
                changed: true,
                owned: true,
                value_created,
            },
            None,
        ));
    }
    if remove_raw_owned_path_entries(&mut segments, &entry) == 0 {
        return Ok((
            PathUpdate {
                changed: false,
                owned: false,
                value_created: false,
            },
            None,
        ));
    }
    let after = segments.join(";");
    let after = if ownership.value_created && after.is_empty() {
        if kind != REG_EXPAND_SZ {
            return Err(invalid_data(
                "installer-created PATH value changed type before removal",
            ));
        }
        key.delete_value("Path")?;
        None
    } else {
        key.set_string("Path", &after, kind)?;
        Some(after)
    };
    Ok((
        PathUpdate {
            changed: true,
            owned: false,
            value_created: false,
        },
        Some(PathRollback {
            before: value,
            after,
            kind,
        }),
    ))
}

pub(crate) fn arp_exists() -> io::Result<bool> {
    Ok(RegistryKey::open_optional(HKEY_CURRENT_USER, ARP_SUBKEY, KEY_READ)?.is_some())
}

pub(crate) fn snapshot_arp_registration(install_root: &Path) -> io::Result<Option<ArpSnapshot>> {
    assert_arp_ownership(install_root)?;
    let Some(key) = RegistryKey::open_optional(HKEY_CURRENT_USER, ARP_SUBKEY, KEY_READ)? else {
        return Ok(None);
    };
    let mut values = BTreeMap::new();
    for name in key.value_names()? {
        let value = key.query(&name)?.ok_or_else(|| {
            invalid_data("ARP value disappeared while its ownership snapshot was captured")
        })?;
        values.insert(name, value);
    }
    Ok(Some(ArpSnapshot {
        install_root: install_root.to_path_buf(),
        values,
    }))
}

fn registry_values_match(
    key: &RegistryKey,
    expected: &BTreeMap<String, RegistryValue>,
) -> io::Result<bool> {
    if key.value_names()? != expected.keys().cloned().collect() {
        return Ok(false);
    }
    for (name, value) in expected {
        if key.query(name)?.as_ref() != Some(value) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn restore_arp_registration(snapshot: &ArpSnapshot) -> io::Result<()> {
    if RegistryKey::open_optional(HKEY_CURRENT_USER, ARP_SUBKEY, KEY_READ)?.is_some() {
        return Err(invalid_data(
            "ARP registration appeared before installer rollback",
        ));
    }
    let key = RegistryKey::create_new(HKEY_CURRENT_USER, ARP_SUBKEY)?;
    let mut restored = BTreeMap::new();
    for (name, value) in &snapshot.values {
        if !registry_values_match(&key, &restored)? || key.query(name)?.is_some() {
            return Err(invalid_data(
                "ARP registration changed before installer rollback",
            ));
        }
        key.set_value(name, value)?;
        restored.insert(name.clone(), value.clone());
    }
    if !registry_values_match(&key, &snapshot.values)? {
        return Err(invalid_data(
            "ARP registration changed during installer rollback",
        ));
    }
    drop(key);
    assert_arp_ownership(&snapshot.install_root)
}

pub(crate) fn quiet_uninstall_string(install_root: &Path) -> io::Result<String> {
    let helper = install_root.join("state").join(NATIVE_HELPER_NAME);
    Ok(format!(
        "\"{}\" quiet-uninstall --install-root \"{}\"",
        helper.display(),
        install_root.display()
    ))
}

pub(crate) fn assert_arp_ownership(install_root: &Path) -> io::Result<()> {
    let Some(key) = RegistryKey::open_optional(HKEY_CURRENT_USER, ARP_SUBKEY, KEY_READ)? else {
        return Ok(());
    };
    let names = key.value_names()?;
    let required = [
        "DisplayName",
        "DisplayVersion",
        "Publisher",
        "InstallLocation",
        "DisplayIcon",
        "UninstallString",
        "QuietUninstallString",
        "VersionMajor",
        "VersionMinor",
        "NoModify",
        "NoRepair",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    let mut with_path_ownership = required.clone();
    with_path_ownership.insert("PathAdded".to_string());
    let mut current = with_path_ownership.clone();
    current.insert("PathValueCreated".to_string());
    if names != required && names != with_path_ownership && names != current {
        return Err(invalid_data(
            "the Herdr ARP registration contains unknown or incomplete state",
        ));
    }
    let string = |name| match key.query(name)? {
        Some(RegistryValue::String {
            value,
            kind: REG_SZ,
        }) => Ok(value),
        _ => Err(invalid_data(format!("Herdr ARP {name} must be REG_SZ"))),
    };
    let dword = |name| match key.query(name)? {
        Some(RegistryValue::Dword(value)) => Ok(value),
        _ => Err(invalid_data(format!("Herdr ARP {name} must be REG_DWORD"))),
    };
    let display_name = string("DisplayName")?;
    let display_version = string("DisplayVersion")?;
    let publisher = string("Publisher")?;
    let registered_root = string("InstallLocation")?;
    let icon = string("DisplayIcon")?;
    let uninstall = string("UninstallString")?;
    let quiet = string("QuietUninstallString")?;
    let expected_uninstaller = install_root.join("uninstall.exe");
    let expected_launcher = install_root.join("bin").join("herdr.exe");
    let expected_quiet = quiet_uninstall_string(install_root)?;
    let display_parts = parse_display_version(&display_version)?;
    let versions_match = u32::from(display_parts[0]) == dword("VersionMajor")?
        && u32::from(display_parts[1]) == dword("VersionMinor")?;
    if display_name != PRODUCT_NAME
        || publisher != PUBLISHER
        || !path_eq(Path::new(&registered_root), install_root)?
        || icon != format!("{},0", expected_launcher.display())
        || uninstall != format!("\"{}\"", expected_uninstaller.display())
        || quiet != expected_quiet
        || !versions_match
        || dword("NoModify")? != 1
        || dword("NoRepair")? != 1
    {
        return Err(super::installer_helper_files::incompatible_installation());
    }
    let optional_path_dword = |name| match key.query(name)? {
        None => Ok(0),
        Some(RegistryValue::Dword(value)) => Ok(value),
        _ => Err(invalid_data(format!("Herdr ARP {name} must be REG_DWORD"))),
    };
    let path_added = optional_path_dword("PathAdded")?;
    // The immediately preceding current registration has PathAdded but predates
    // PathValueCreated. Treat the missing fact as unowned rather than blocking
    // setup or claiming authority to delete the user's PATH value.
    let path_value_created = optional_path_dword("PathValueCreated")?;
    if path_added > 1 || path_value_created > 1 || (path_value_created == 1 && path_added == 0) {
        return Err(invalid_data("Herdr ARP PATH ownership is invalid"));
    }
    Ok(())
}

pub(crate) fn arp_path_ownership(install_root: &Path) -> io::Result<ArpPathOwnership> {
    assert_arp_ownership(install_root)?;
    let Some(key) = RegistryKey::open_optional(HKEY_CURRENT_USER, ARP_SUBKEY, KEY_READ)? else {
        return Ok(ArpPathOwnership::default());
    };
    let dword = |name| match key.query(name)? {
        None => Ok(None),
        Some(RegistryValue::Dword(value)) if value <= 1 => Ok(Some(value == 1)),
        _ => Err(invalid_data(format!("Herdr ARP {name} is invalid"))),
    };
    let path_added = dword("PathAdded")?;
    let path_value_created = dword("PathValueCreated")?;
    Ok(ArpPathOwnership {
        ownership: PathOwnership {
            owned: path_added.unwrap_or(false),
            value_created: path_value_created.unwrap_or(false),
        },
        value_created_present: path_value_created.is_some(),
    })
}

pub(crate) fn set_arp_registration<F>(
    install_root: &Path,
    display_version: &str,
    numeric_version: &str,
    path_added: bool,
    path_value_created: bool,
    after_path_added: F,
) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    if path_value_created && !path_added {
        return Err(invalid_data(
            "PATH value ownership requires installer entry ownership",
        ));
    }
    assert_arp_ownership(install_root)?;
    assert_regular_file(&install_root.join("state").join(NATIVE_HELPER_NAME))?;
    let key = RegistryKey::create(HKEY_CURRENT_USER, ARP_SUBKEY)?;
    let uninstaller = install_root.join("uninstall.exe");
    let launcher = install_root.join("bin").join("herdr.exe");
    let numeric = numeric_version.split('.').collect::<Vec<_>>();
    if numeric.len() != 4 {
        return Err(invalid_data("numeric version must have four parts"));
    }
    for (name, value) in [
        ("DisplayName", PRODUCT_NAME.to_string()),
        ("DisplayVersion", display_version.to_string()),
        ("Publisher", PUBLISHER.to_string()),
        ("InstallLocation", install_root.display().to_string()),
        ("DisplayIcon", format!("{},0", launcher.display())),
        ("UninstallString", format!("\"{}\"", uninstaller.display())),
        (
            "QuietUninstallString",
            quiet_uninstall_string(install_root)?,
        ),
    ] {
        key.set_string(name, &value, REG_SZ)?;
    }
    key.set_dword(
        "VersionMajor",
        numeric[0]
            .parse()
            .map_err(|_| invalid_data("invalid major version"))?,
    )?;
    key.set_dword(
        "VersionMinor",
        numeric[1]
            .parse()
            .map_err(|_| invalid_data("invalid minor version"))?,
    )?;
    key.set_dword("NoModify", 1)?;
    key.set_dword("NoRepair", 1)?;
    key.set_dword("PathAdded", u32::from(path_added))?;
    after_path_added()?;
    key.set_dword("PathValueCreated", u32::from(path_value_created))?;
    drop(key);
    assert_arp_ownership(install_root)
}

pub(crate) fn remove_arp_registration(install_root: &Path) -> io::Result<bool> {
    assert_arp_ownership(install_root)?;
    if !arp_exists()? {
        return Ok(false);
    }
    let subkey = wide(ARP_SUBKEY)?;
    // SAFETY: subkey is NUL-terminated and HKCU is a predefined handle.
    check_registry(
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, subkey.as_ptr()) },
        "remove Herdr ARP registration",
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparable_entries_ignore_case_quotes_and_trailing_separator() {
        assert!(path_entry_equal(
            r#""C:\Users\Example\Herdr\bin\""#,
            r"c:\users\example\herdr\bin",
            false,
        ));
    }

    #[test]
    fn raw_path_removal_accepts_literal_percent_and_rejects_expression_tokens() {
        let literal = r"C:\Users\Percent%Name\AppData\Local\Programs\Herdr\bin";
        assert!(raw_owned_path_entry_equal(literal, literal));
        assert!(raw_owned_path_entry_equal(
            r#""c:/users/percent%name/appdata/local/programs/herdr/bin/""#,
            literal
        ));
        assert!(!raw_owned_path_entry_equal(
            r"%LOCALAPPDATA%\Programs\Herdr\bin",
            r"C:\Users\Example\AppData\Local\Programs\Herdr\bin"
        ));
    }

    #[test]
    fn raw_path_removal_removes_every_literal_spelling_and_preserves_expressions() {
        let owned = r"C:\Users\Example\AppData\Local\Programs\Herdr\bin";
        let mut segments = vec![
            owned.to_string(),
            r#""c:/users/example/appdata/local/programs/herdr/bin/""#.to_string(),
            r"C:\USERS\EXAMPLE\APPDATA\LOCAL\PROGRAMS\HERDR\BIN\\".to_string(),
            r"%LOCALAPPDATA%\Programs\Herdr\bin".to_string(),
            r"C:\Unrelated".to_string(),
        ];

        assert_eq!(remove_raw_owned_path_entries(&mut segments, owned), 3);
        assert_eq!(
            segments,
            [
                r"%LOCALAPPDATA%\Programs\Herdr\bin".to_string(),
                r"C:\Unrelated".to_string(),
            ]
        );
    }

    #[test]
    fn quiet_uninstall_command_invokes_only_the_native_helper() {
        let root = Path::new(r"C:\Users\Example\AppData\Local\Programs\Herdr");
        assert_eq!(
            quiet_uninstall_string(root).unwrap(),
            r#""C:\Users\Example\AppData\Local\Programs\Herdr\state\installer-helper.exe" quiet-uninstall --install-root "C:\Users\Example\AppData\Local\Programs\Herdr""#
        );
    }
}
