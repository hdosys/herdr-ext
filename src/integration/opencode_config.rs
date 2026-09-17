use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use jsonc_parser::cst::{CstInputValue, CstRootNode};
use jsonc_parser::ParseOptions;
use serde_json::Value;

const TUI_CONFIG_NAME: &str = "tui.jsonc";
static NEXT_CONFIG_WRITE: AtomicU64 = AtomicU64::new(0);

fn write_config(path: &Path, content: &str) -> io::Result<()> {
    // Resolve an existing file link so replacement preserves the user's link.
    // A dangling link is an error, not permission to replace it with a file.
    let target = match fs::symlink_metadata(path) {
        Ok(_) => fs::canonicalize(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(error),
    };
    let existing = match fs::metadata(&target) {
        Ok(metadata) => {
            if metadata.permissions().readonly() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "OpenCode TUI config is read-only",
                ));
            }
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "OpenCode TUI config has no parent directory",
        )
    })?;
    let sequence = NEXT_CONFIG_WRITE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".herdr-opencode-{}-{sequence}.tmp",
        std::process::id()
    ));
    drop(crate::platform::create_private_state_file(&temporary)?);
    let result = (|| {
        if existing {
            // Copy the existing permissions, including Windows security metadata,
            // before truncating only our own stage, never the user's config.
            fs::copy(&target, &temporary)?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        crate::platform::replace_file(&temporary, &target)?;
        crate::platform::sync_parent_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error: io::Error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to replace OpenCode TUI config at {}: {error}",
                path.display()
            ),
        )
    })
}

pub(crate) fn tui_config_path(config_dir: &Path) -> PathBuf {
    let json = config_dir.join("tui.json");
    let jsonc = config_dir.join(TUI_CONFIG_NAME);
    if !jsonc.exists() && json.exists() {
        json
    } else {
        jsonc
    }
}

pub(crate) fn validate_tui_plugin_config(config_dir: &Path) -> io::Result<()> {
    for name in ["tui.json", TUI_CONFIG_NAME] {
        validate_tui_config(&config_dir.join(name))?;
    }
    Ok(())
}

fn validate_tui_config(config_path: &Path) -> io::Result<()> {
    if !config_path.is_file() {
        return Ok(());
    }

    let content = fs::read_to_string(config_path)?;
    let root = parse_root(&content, config_path)?;
    let object = root_object(&root, config_path)?;
    if object
        .get("plugin")
        .is_some_and(|property| property.array_value().is_none())
    {
        return Err(invalid_plugin_list(config_path));
    }
    Ok(())
}

pub(crate) fn add_tui_plugin(config_dir: &Path, plugin_spec: &str) -> io::Result<PathBuf> {
    validate_tui_plugin_config(config_dir)?;
    for name in ["tui.json", TUI_CONFIG_NAME] {
        let path = config_dir.join(name);
        if tui_plugin_is_configured_at(&path, plugin_spec) {
            return Ok(path);
        }
    }
    let config_path = tui_config_path(config_dir);
    let content = if config_path.is_file() {
        fs::read_to_string(&config_path)?
    } else {
        "{}\n".to_string()
    };
    let root = parse_root(&content, &config_path)?;
    let object = root_object(&root, &config_path)?;

    match object.get("plugin") {
        Some(property) => {
            let plugins = property
                .array_value()
                .ok_or_else(|| invalid_plugin_list(&config_path))?;
            plugins.append(CstInputValue::String(plugin_spec.to_string()));
        }
        None => {
            object.append(
                "plugin",
                CstInputValue::Array(vec![CstInputValue::String(plugin_spec.to_string())]),
            );
        }
    }

    write_config(&config_path, &root.to_string())?;
    Ok(config_path)
}

pub(crate) fn remove_tui_plugin(config_dir: &Path, plugin_spec: &str) -> io::Result<bool> {
    validate_tui_plugin_config(config_dir)?;
    let mut removed = false;
    for name in ["tui.json", TUI_CONFIG_NAME] {
        removed |= remove_tui_plugin_at(&config_dir.join(name), plugin_spec)?;
    }
    Ok(removed)
}

fn remove_tui_plugin_at(config_path: &Path, plugin_spec: &str) -> io::Result<bool> {
    if !config_path.is_file() {
        return Ok(false);
    }

    let content = fs::read_to_string(config_path)?;
    let root = parse_root(&content, config_path)?;
    let object = root_object(&root, config_path)?;
    let Some(property) = object.get("plugin") else {
        return Ok(false);
    };
    let plugins = property
        .array_value()
        .ok_or_else(|| invalid_plugin_list(config_path))?;
    let mut removed = false;
    for entry in plugins.elements() {
        if entry
            .to_serde_value()
            .is_some_and(|entry| plugin_entry_matches(&entry, plugin_spec))
        {
            entry.remove();
            removed = true;
        }
    }
    if !removed {
        return Ok(false);
    }
    if plugins.elements().is_empty() {
        property.remove();
    }

    let remaining = root.to_string();
    // Remove an integration-only document, but retain comments, preferences,
    // and unrelated plugin entries even when no Herdr registration remains.
    if remaining
        .chars()
        .filter(|c| !c.is_whitespace())
        .eq("{}".chars())
    {
        fs::remove_file(config_path)?;
    } else {
        write_config(config_path, &remaining)?;
    }
    Ok(true)
}

pub(crate) fn tui_plugin_is_configured(config_dir: &Path, plugin_spec: &str) -> bool {
    ["tui.json", TUI_CONFIG_NAME]
        .iter()
        .any(|name| tui_plugin_is_configured_at(&config_dir.join(name), plugin_spec))
}

fn tui_plugin_is_configured_at(config_path: &Path, plugin_spec: &str) -> bool {
    let Ok(content) = fs::read_to_string(config_path) else {
        return false;
    };
    let Ok(root) = parse_root(&content, config_path) else {
        return false;
    };
    let Ok(object) = root_object(&root, config_path) else {
        return false;
    };
    object
        .get("plugin")
        .and_then(|property| property.array_value())
        .is_some_and(|plugins| {
            plugins.elements().iter().any(|entry| {
                entry
                    .to_serde_value()
                    .is_some_and(|entry| plugin_entry_matches(&entry, plugin_spec))
            })
        })
}

fn parse_root(content: &str, path: &Path) -> io::Result<CstRootNode> {
    CstRootNode::parse(content, &jsonc_parse_options()).map_err(|err| {
        io::Error::other(format!(
            "failed to parse OpenCode TUI config at {}: {err}",
            path.display()
        ))
    })
}

fn root_object(root: &CstRootNode, path: &Path) -> io::Result<jsonc_parser::cst::CstObject> {
    root.value()
        .and_then(|value| value.as_object())
        .ok_or_else(|| invalid_root(path))
}

fn jsonc_parse_options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    }
}

fn plugin_entry_matches(entry: &Value, plugin_spec: &str) -> bool {
    entry.as_str() == Some(plugin_spec)
        || entry
            .as_array()
            .and_then(|parts| parts.first())
            .and_then(Value::as_str)
            == Some(plugin_spec)
}

fn invalid_root(path: &Path) -> io::Error {
    io::Error::other(format!(
        "OpenCode TUI config at {} must be a JSON object",
        path.display()
    ))
}

fn invalid_plugin_list(path: &Path) -> io::Error {
    io::Error::other(format!(
        "OpenCode TUI config plugin list at {} must be an array",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn unique_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-opencode-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("temporary config directory should be created");
        dir
    }

    fn parse_config(path: &Path) -> Value {
        let content = fs::read_to_string(path).unwrap();
        parse_root(&content, path)
            .unwrap()
            .value()
            .and_then(|value| value.to_serde_value())
            .unwrap()
    }

    #[test]
    fn add_and_remove_tui_plugin_preserves_jsonc_config() {
        let dir = unique_dir();
        let config_path = dir.join(TUI_CONFIG_NAME);
        fs::write(
            &config_path,
            concat!(
                "{\n",
                "  // Keep this comment.\n",
                "  \"theme\": \"system\",\n",
                "  \"plugin\": [\"example\", [\"configured\", {\"enabled\": true}]],\n",
                "}\n",
            ),
        )
        .unwrap();

        add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap();
        add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap();
        let installed_content = fs::read_to_string(&config_path).unwrap();
        assert!(installed_content.contains("// Keep this comment."));
        let installed = parse_config(&config_path);
        assert_eq!(installed["theme"], "system");
        assert_eq!(
            installed["plugin"],
            json!([
                "example",
                ["configured", {"enabled": true}],
                "./herdr-tui-state.js"
            ])
        );

        assert!(remove_tui_plugin(&dir, "./herdr-tui-state.js").unwrap());
        let removed_content = fs::read_to_string(&config_path).unwrap();
        assert!(removed_content.contains("// Keep this comment."));
        let removed = parse_config(&config_path);
        assert_eq!(removed["theme"], "system");
        assert_eq!(
            removed["plugin"],
            json!(["example", ["configured", {"enabled": true}]])
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn rejected_config_replacement_preserves_user_bytes_and_cleans_stage() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let dir = unique_dir();
        let path = dir.join(TUI_CONFIG_NAME);
        let original = "{\n// Keep my settings.\n\"theme\":\"system\"\n}\n";
        fs::write(&path, original).unwrap();
        // An editor permits ordinary writes but holds replacement/deletion closed.
        // The old in-place writer would alter the file under this same handle.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
            .unwrap();
        assert!(add_tui_plugin(&dir, "./herdr-tui-session.js").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        drop(held);
        add_tui_plugin(&dir, "./herdr-tui-session.js").unwrap();
        assert_eq!(parse_config(&path)["theme"], "system");
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("// Keep my settings."));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn managed_jsonc_leaves_opencode_migration_target_absent() {
        let dir = unique_dir();
        let legacy_config_path = dir.join("opencode.json");
        let legacy_config = "{\n  \"theme\": \"system\"\n}\n";
        fs::write(&legacy_config_path, legacy_config).unwrap();

        let config_path = add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap();

        assert_eq!(config_path, dir.join("tui.jsonc"));
        assert!(!dir.join("tui.json").exists());
        assert_eq!(
            fs::read_to_string(legacy_config_path).unwrap(),
            legacy_config
        );
        assert_eq!(
            parse_config(&config_path),
            json!({ "plugin": ["./herdr-tui-state.js"] })
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remove_tui_plugin_removes_empty_managed_config() {
        let dir = unique_dir();
        let config_path = add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap();

        assert!(remove_tui_plugin(&dir, "./herdr-tui-state.js").unwrap());
        assert!(!config_path.exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn configured_tui_plugin_accepts_option_tuple() {
        let dir = unique_dir();
        fs::write(
            dir.join(TUI_CONFIG_NAME),
            r#"{"plugin":[["./herdr-tui-state.js",{"enabled":true}]]}"#,
        )
        .unwrap();

        assert!(tui_plugin_is_configured(&dir, "./herdr-tui-state.js"));
        assert!(remove_tui_plugin(&dir, "./herdr-tui-state.js").unwrap());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tui_json_registration_is_reused_without_creating_jsonc() {
        let dir = unique_dir();
        let path = dir.join("tui.json");
        let content = r#"{"theme":"system","plugin":[["./herdr-tui-state.js",{"enabled":true}]]}"#;
        fs::write(&path, content).unwrap();
        assert!(tui_plugin_is_configured(&dir, "./herdr-tui-state.js"));
        assert_eq!(add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap(), path);
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        assert!(!dir.join(TUI_CONFIG_NAME).exists());
        assert!(remove_tui_plugin(&dir, "./herdr-tui-state.js").unwrap());
        assert_eq!(parse_config(&path), json!({"theme":"system"}));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tui_registration_spans_both_files_without_duplication_or_comment_loss() {
        let dir = unique_dir();
        let json = dir.join("tui.json");
        let jsonc = dir.join(TUI_CONFIG_NAME);
        fs::write(&json, r#"{"plugin":["./herdr-tui-state.js"]}"#).unwrap();
        fs::write(&jsonc, "{\n// Keep this comment.\n}\n").unwrap();
        assert_eq!(add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap(), json);
        fs::write(
            &jsonc,
            "{\n// Keep this comment.\n\"plugin\":[\"./herdr-tui-state.js\"]\n}\n",
        )
        .unwrap();
        assert!(remove_tui_plugin(&dir, "./herdr-tui-state.js").unwrap());
        assert!(!json.exists());
        assert!(fs::read_to_string(&jsonc)
            .unwrap()
            .contains("// Keep this comment."));
        assert!(!tui_plugin_is_configured(&dir, "./herdr-tui-state.js"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn existing_tui_json_receives_registration_and_invalid_json_blocks_install() {
        let dir = unique_dir();
        let path = dir.join("tui.json");
        fs::write(&path, r#"{"theme":"system"}"#).unwrap();
        assert_eq!(add_tui_plugin(&dir, "./herdr-tui-state.js").unwrap(), path);
        assert_eq!(
            parse_config(&path),
            json!({"theme":"system","plugin":["./herdr-tui-state.js"]})
        );
        assert!(!dir.join(TUI_CONFIG_NAME).exists());
        fs::write(&path, r#"{"plugin":{}}"#).unwrap();
        assert!(validate_tui_plugin_config(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
