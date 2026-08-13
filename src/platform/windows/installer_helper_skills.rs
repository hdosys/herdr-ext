use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
};

use super::installer_helper_files::{
    assert_regular_dir, assert_regular_file, full_path, normalized_text_sha256, path_eq,
    path_exists, path_within, read_strict_utf8, remove_validated_directory, safe_tree_entries,
    sha256,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SkillDisposition {
    Keep,
    Auto,
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SkillState {
    Absent,
    Known,
    Unknown,
    Unsafe,
}

#[derive(Clone, Debug)]
struct SkillFileState {
    state: SkillState,
    path: PathBuf,
}

pub(crate) fn user_profile_root(value: &Path) -> io::Result<PathBuf> {
    let root = full_path(value)?;
    assert_regular_dir(&root)?;
    Ok(root)
}

pub(crate) fn agent_skills_root(profile: &Path) -> io::Result<PathBuf> {
    Ok(user_profile_root(profile)?.join(".agents").join("skills"))
}

fn claude_config_root(profile: &Path, configured: Option<&str>) -> io::Result<PathBuf> {
    let profile = user_profile_root(profile)?;
    let value = match configured.map(str::trim).filter(|value| !value.is_empty()) {
        None => profile.join(".claude"),
        Some("~") => profile,
        Some(value) if value.starts_with("~\\") || value.starts_with("~/") => {
            profile.join(&value[2..])
        }
        Some(value) => full_path(Path::new(value))?,
    };
    full_path(&value)
}

pub(crate) fn claude_skills_root(profile: &Path, configured: Option<&str>) -> io::Result<PathBuf> {
    Ok(claude_config_root(profile, configured)?.join("skills"))
}

fn command_exists(name: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    let extensions = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    for directory in env::split_paths(&path) {
        for extension in extensions.split(';') {
            let candidate = directory.join(format!("{name}{extension}"));
            if fs::symlink_metadata(candidate)
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

pub(crate) fn claude_installed(profile: &Path) -> io::Result<bool> {
    if env::var("CLAUDE_CONFIG_DIR")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        return Ok(true);
    }
    let default = claude_config_root(profile, None)?;
    if path_exists(&default)? {
        return Ok(fs::symlink_metadata(&default)?.is_dir());
    }
    Ok(command_exists("claude"))
}

pub(crate) fn claude_roots_for_removal(profile: &Path) -> io::Result<Vec<PathBuf>> {
    let default = claude_skills_root(profile, None)?;
    let mut roots = vec![default.clone()];
    if let Ok(value) = env::var("CLAUDE_CONFIG_DIR") {
        if !value.trim().is_empty() {
            let configured = claude_skills_root(profile, Some(&value))?;
            if !path_eq(&configured, &default)? {
                roots.push(configured);
            }
        }
    }
    Ok(roots)
}

pub(crate) fn read_managed_skill_hashes(
    path: &Path,
    current_skill: Option<&Path>,
) -> io::Result<BTreeSet<String>> {
    let text = read_strict_utf8(path)?;
    if !text.ends_with('\n') {
        return Err(super::installer_helper_files::invalid_data(format!(
            "managed skill hash manifest lacks final newline: {}",
            path.display()
        )));
    }
    let mut lines = text[..text.len() - 1].split('\n');
    if lines.next() != Some("herdr-managed-skill-hashes-v1") {
        return Err(super::installer_helper_files::invalid_data(format!(
            "invalid managed skill hash manifest: {}",
            path.display()
        )));
    }
    let mut hashes = BTreeSet::new();
    for line in lines {
        if line.len() != 64
            || !line
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || !hashes.insert(line.to_string())
        {
            return Err(super::installer_helper_files::invalid_data(format!(
                "invalid or duplicate managed skill hash in {}",
                path.display()
            )));
        }
    }
    if hashes.is_empty() {
        return Err(super::installer_helper_files::invalid_data(
            "managed skill hash manifest is empty",
        ));
    }
    if let Some(skill) = current_skill {
        let hash = normalized_text_sha256(skill)?;
        if !hashes.contains(&hash) {
            return Err(super::installer_helper_files::invalid_data(format!(
                "embedded skill is absent from managed hash manifest: {}",
                skill.display()
            )));
        }
    }
    Ok(hashes)
}

fn initialize_skills_root(root: &Path) -> io::Result<()> {
    let root = full_path(root)?;
    let parent = root
        .parent()
        .ok_or_else(|| super::installer_helper_files::invalid_data("skills root has no parent"))?;
    let grandparent = parent.parent().ok_or_else(|| {
        super::installer_helper_files::invalid_data("skills root has no grandparent")
    })?;
    assert_regular_dir(grandparent)?;
    if !path_exists(parent)? {
        fs::create_dir(parent)?;
    }
    assert_regular_dir(parent)?;
    if !path_exists(&root)? {
        fs::create_dir(&root)?;
    }
    assert_regular_dir(&root)
}

fn assert_skill_target(root: &Path) -> io::Result<()> {
    initialize_skills_root(root)?;
    let target = root.join("herdr");
    if path_exists(&target)? {
        assert_regular_dir(&target)?;
        let skill = target.join("SKILL.md");
        if path_exists(&skill)? {
            assert_regular_file(&skill)?;
        }
    }
    Ok(())
}

fn skill_file_state(root: &Path, known: &BTreeSet<String>) -> io::Result<SkillFileState> {
    let root = full_path(root)?;
    let target = root.join("herdr");
    let skill = target.join("SKILL.md");
    let parent = root
        .parent()
        .ok_or_else(|| super::installer_helper_files::invalid_data("skills root has no parent"))?;
    let grandparent = parent.parent().ok_or_else(|| {
        super::installer_helper_files::invalid_data("skills root has no grandparent")
    })?;
    for component in [grandparent, parent, root.as_path(), target.as_path()] {
        match fs::symlink_metadata(component) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(SkillFileState {
                    state: SkillState::Absent,
                    path: skill,
                });
            }
            Err(err) => return Err(err),
            Ok(metadata)
                if !metadata.is_dir() || super::installer_helper_files::is_reparse(&metadata) =>
            {
                return Ok(SkillFileState {
                    state: SkillState::Unsafe,
                    path: skill,
                });
            }
            Ok(_) => {}
        }
    }
    match fs::symlink_metadata(&skill) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(SkillFileState {
            state: SkillState::Absent,
            path: skill,
        }),
        Err(err) => Err(err),
        Ok(metadata)
            if !metadata.is_file() || super::installer_helper_files::is_reparse(&metadata) =>
        {
            Ok(SkillFileState {
                state: SkillState::Unsafe,
                path: skill,
            })
        }
        Ok(_) => {
            let state = if known.contains(&sha256(&skill)?) {
                SkillState::Known
            } else {
                SkillState::Unknown
            };
            Ok(SkillFileState { state, path: skill })
        }
    }
}

fn install_skill(
    source: &Path,
    root: &Path,
    known: &BTreeSet<String>,
) -> io::Result<Option<PathBuf>> {
    let expected = normalized_text_sha256(source)?;
    if !known.contains(&expected) {
        return Err(super::installer_helper_files::invalid_data(format!(
            "embedded skill is absent from managed hash manifest: {}",
            source.display()
        )));
    }
    assert_skill_target(root)?;
    let target = root.join("herdr");
    if !path_exists(&target)? {
        fs::create_dir(&target)?;
    }
    assert_regular_dir(&target)?;
    let destination = target.join("SKILL.md");
    if path_exists(&destination)? {
        assert_regular_file(&destination)?;
        if !known.contains(&sha256(&destination)?) {
            return Ok(Some(destination));
        }
        fs::remove_file(&destination)?;
    }
    fs::copy(source, &destination)?;
    if normalized_text_sha256(&destination)? != expected {
        return Err(super::installer_helper_files::invalid_data(format!(
            "installed skill differs from embedded source: {}",
            destination.display()
        )));
    }
    Ok(None)
}

pub(crate) fn install_skill_copies(
    source: &Path,
    agent_root: &Path,
    claude_root: Option<&Path>,
    known: &BTreeSet<String>,
) -> io::Result<Vec<PathBuf>> {
    let mut preserved = Vec::new();
    if let Some(path) = install_skill(source, agent_root, known)? {
        preserved.push(path);
    }
    if let Some(root) = claude_root {
        if !path_eq(root, agent_root)? {
            if let Some(path) = install_skill(source, root, known)? {
                preserved.push(path);
            }
        }
    }
    Ok(preserved)
}

fn remove_skill(
    root: &Path,
    known: &BTreeSet<String>,
    disposition: SkillDisposition,
) -> io::Result<Option<PathBuf>> {
    let state = skill_file_state(root, known)?;
    if state.state == SkillState::Absent {
        return Ok(None);
    }
    if state.state == SkillState::Unsafe || disposition == SkillDisposition::Keep {
        return Ok(Some(state.path));
    }
    assert_regular_file(&state.path)?;
    let is_known = known.contains(&sha256(&state.path)?);
    if !is_known && disposition != SkillDisposition::Remove {
        return Ok(Some(state.path));
    }
    fs::remove_file(&state.path)?;
    let directory = state.path.parent().ok_or_else(|| {
        super::installer_helper_files::invalid_data("managed skill path has no parent")
    })?;
    if fs::read_dir(directory)?.next().is_none() {
        fs::remove_dir(directory)?;
    }
    Ok(None)
}

pub(crate) fn remove_skill_copies_best_effort(
    agent_root: &Path,
    claude_roots: &[PathBuf],
    known: &BTreeSet<String>,
    disposition: SkillDisposition,
) -> (Vec<PathBuf>, Option<String>) {
    let mut roots = vec![agent_root.to_path_buf()];
    roots.extend_from_slice(claude_roots);
    let mut unique = Vec::<PathBuf>::new();
    for root in roots {
        if unique
            .iter()
            .all(|existing| !path_eq(existing, &root).unwrap_or(false))
        {
            unique.push(root);
        }
    }
    let mut preserved = Vec::new();
    for root in unique {
        match remove_skill(&root, known, disposition) {
            Ok(Some(path)) => preserved.push(path),
            Ok(None) => {}
            Err(err) => {
                return (
                    Vec::new(),
                    Some(format!(
                        "Warning: Herdr skill cleanup was incomplete and residual state was preserved. {err}"
                    )),
                );
            }
        }
    }
    (preserved, None)
}

pub(crate) fn skill_removal_default(
    agent_root: &Path,
    claude_roots: &[PathBuf],
    known: &BTreeSet<String>,
) -> io::Result<&'static str> {
    let mut roots = vec![agent_root.to_path_buf()];
    roots.extend_from_slice(claude_roots);
    let mut seen = Vec::<PathBuf>::new();
    for root in roots {
        if seen
            .iter()
            .any(|existing| path_eq(existing, &root).unwrap_or(false))
        {
            continue;
        }
        seen.push(root.clone());
        let state = skill_file_state(&root, known)?;
        if state.state != SkillState::Absent && state.state != SkillState::Known {
            return Ok("Keep");
        }
    }
    Ok("Remove")
}

pub(crate) fn remove_user_settings(profile: &Path) -> io::Result<()> {
    let profile = user_profile_root(profile)?;
    let settings = profile.join(".herdr");
    if !path_within(&settings, &profile)? {
        return Err(super::installer_helper_files::invalid_data(format!(
            "settings directory escaped user profile: {}",
            settings.display()
        )));
    }
    if !path_exists(&settings)? {
        return Ok(());
    }
    assert_regular_dir(&settings)?;
    let _ = safe_tree_entries(&settings)?;
    remove_validated_directory(&settings)?;
    if path_exists(&settings)? {
        return Err(super::installer_helper_files::invalid_data(format!(
            "settings cleanup did not reach terminal state: {}",
            settings.display()
        )));
    }
    Ok(())
}
