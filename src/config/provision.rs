//! One client configuration, with explicitly machine-local values left on the target.

use std::io::{self, Read};
use std::path::Path;

use base64::Engine as _;

// This is the complete exclusion list, not an allowlist of supported settings.
// Excluded values never leave the client and existing target values survive.
pub(crate) const EXCLUDED_KEYS: &[&str] = &[
    "onboarding",
    "terminal.default_shell",
    "terminal.new_cwd",
    "worktrees.directory",
    "agent.args",
    "keys.command",
    "ui.tab_bar_right",
    "ui.sound.path",
    "ui.sound.done_path",
    "ui.sound.request_path",
];

const MAX_TRANSFER_BYTES: u64 = 1024 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn parse(content: &str) -> io::Result<toml::Table> {
    content.parse().map_err(|_| {
        invalid("cannot provision invalid configuration TOML; existing configuration is unchanged")
    })
}

fn remove(table: &mut toml::Table, path: &str) -> Option<toml::Value> {
    match path.split_once('.') {
        Some((section, key)) => remove(table.get_mut(section)?.as_table_mut()?, key),
        None => table.remove(path),
    }
}

fn insert(table: &mut toml::Table, path: &str, value: toml::Value) -> io::Result<()> {
    if let Some((section, key)) = path.split_once('.') {
        let section = table
            .entry(section.to_owned())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| invalid("provisioned configuration section is not a table"))?;
        insert(section, key, value)
    } else {
        table.insert(path.to_owned(), value);
        Ok(())
    }
}

fn validated_text(table: &toml::Table) -> io::Result<String> {
    let text = toml::to_string_pretty(table)
        .map_err(|_| invalid("cannot encode provisioned configuration"))?;
    let loaded = super::io::load_live_config_from_str(&text)
        .map_err(|_| invalid("invalid provisioned configuration; run herdr config check"))?;
    if !loaded.diagnostics.is_empty() || !loaded.invalid_sections.is_empty() {
        return Err(invalid(
            "invalid provisioned configuration; run herdr config check",
        ));
    }
    Ok(text)
}

fn filtered(content: &str) -> io::Result<toml::Table> {
    let mut table = parse(content)?;
    for key in EXCLUDED_KEYS {
        remove(&mut table, key);
    }
    Ok(table)
}

/// Missing client files do not reset an independently configured target.
pub(crate) fn export() -> io::Result<Option<String>> {
    let Some(content) = super::io::read_optional_config(&super::config_path())? else {
        return Ok(None);
    };
    let text = validated_text(&filtered(&content)?)?;
    // ASCII stdin avoids PowerShell's native-pipeline text encoding differences.
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    if encoded.len() as u64 > MAX_TRANSFER_BYTES {
        return Err(invalid(
            "provisioned configuration exceeds the 1 MiB transfer limit",
        ));
    }
    Ok(Some(encoded))
}

pub(crate) fn import(reader: impl Read, path: &Path) -> io::Result<()> {
    let mut encoded = String::new();
    reader
        .take(MAX_TRANSFER_BYTES + 1)
        .read_to_string(&mut encoded)?;
    if encoded.len() as u64 > MAX_TRANSFER_BYTES {
        return Err(invalid(
            "provisioned configuration exceeds the 1 MiB transfer limit",
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| invalid("invalid provisioned configuration encoding"))?;
    let content = std::str::from_utf8(&bytes)
        .map_err(|_| invalid("provisioned configuration is not UTF-8"))?;
    let mut incoming = filtered(content)?;
    let current = super::io::read_optional_config(path)?.unwrap_or_default();
    let mut existing = parse(&current)?;
    for key in EXCLUDED_KEYS {
        if let Some(value) = remove(&mut existing, key) {
            insert(&mut incoming, key, value)?;
        }
    }
    let text = validated_text(&incoming)?;
    if text == current {
        return Ok(());
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    // Reuse the protected user-config writer, including permissions and link checks.
    crate::integration::config_file::write_config(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_transfers_settings_but_preserves_target_exclusions() {
        let root =
            std::env::temp_dir().join(format!("herdr-provision-preserve-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("config.toml");
        let sound = root.join("sound.mp3");
        std::fs::write(&sound, b"test sound").unwrap();
        let mut existing = parse(
            r#"
onboarding = false
[terminal]
default_shell = "/bin/bash"
new_cwd = "/srv/projects"
[worktrees]
directory = "/srv/worktrees"
[agent]
args = ["--model", "remote-model"]
[ui.sound]
path = "sound.mp3"
[keys]
command = [{ key = "ctrl+x", command = "remote-command" }]
[theme]
name = "nord"
[session]
startup_per_agent_delay_ms = 42
"#,
        )
        .unwrap();
        existing["ui"]["sound"]["path"] = toml::Value::String(sound.to_string_lossy().into_owned());
        std::fs::write(&path, toml::to_string_pretty(&existing).unwrap()).unwrap();
        let local = r#"
onboarding = true
[terminal]
default_shell = "pwsh.exe"
new_cwd = 'C:\Projects'
[worktrees]
directory = 'C:\Worktrees'
[agent]
kind = "opencode"
args = ["--model", "local-model"]
[ui.sound]
enabled = false
path = 'C:\sound.wav'
[keys]
command = [{ key = "ctrl+y", command = "local-command" }]
[theme]
name = "dracula"
[session]
auto_start_agent = "opencode"
"#;
        let outgoing = validated_text(&filtered(local).unwrap()).unwrap();
        assert!(!outgoing.contains("pwsh.exe"));
        assert!(!outgoing.contains("local-command"));
        let encoded = base64::engine::general_purpose::STANDARD.encode(outgoing);
        import(encoded.as_bytes(), &path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        let actual = parse(&saved).unwrap();
        assert_eq!(
            actual["terminal"]["default_shell"].as_str(),
            Some("/bin/bash")
        );
        assert_eq!(
            actual["worktrees"]["directory"].as_str(),
            Some("/srv/worktrees")
        );
        assert_eq!(actual["agent"]["args"][0].as_str(), Some("--model"));
        assert_eq!(actual["agent"]["args"][1].as_str(), Some("remote-model"));
        assert_eq!(actual["theme"]["name"].as_str(), Some("dracula"));
        assert_eq!(
            actual["session"]["auto_start_agent"].as_str(),
            Some("opencode")
        );
        assert!(actual["session"]
            .get("startup_per_agent_delay_ms")
            .is_none());
        assert_eq!(actual["ui"]["sound"]["enabled"].as_bool(), Some(false));
        assert_eq!(actual["ui"]["sound"]["path"].as_str(), sound.to_str());
        assert!(saved.contains("remote-command"));
        import(encoded.as_bytes(), &path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provision_rejects_invalid_input_without_replacing_target() {
        let root =
            std::env::temp_dir().join(format!("herdr-provision-invalid-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("config.toml");
        let original = "[theme]\nname = 'dracula'\n";
        std::fs::write(&path, original).unwrap();
        for content in ["[broken", "[server]\nheadless_cols = 0", "unknown = true"] {
            let encoded = base64::engine::general_purpose::STANDARD.encode(content);
            assert!(import(encoded.as_bytes(), &path).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        assert!(import(b"not base64".as_slice(), &path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        std::fs::remove_dir_all(root).unwrap();
    }
}
