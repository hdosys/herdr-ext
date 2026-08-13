//! Native owner for the managed Windows install lifecycle.

use std::{collections::BTreeMap, ffi::OsString, io, path::PathBuf};

use crate::managed_install::BuildId;

#[path = "installer_helper_files.rs"]
mod installer_helper_files;
#[path = "installer_helper_lifecycle.rs"]
mod installer_helper_lifecycle;
#[path = "installer_helper_registry.rs"]
mod installer_helper_registry;
#[path = "installer_helper_skills.rs"]
mod installer_helper_skills;

use installer_helper_files::invalid_data;
use installer_helper_lifecycle::{
    InstallManager, InstallOptions, MaintenanceOptions, QuietRunnerOptions, QuietUninstallOptions,
    SettingsDisposition, SkillDefaultOptions, UninstallOptions,
};
use installer_helper_skills::SkillDisposition;

pub(crate) fn run() -> io::Result<String> {
    let mut args = std::env::args_os().skip(1);
    let action = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| {
            invalid_data(
                "missing installer helper action; expected install, uninstall, quiet-uninstall, skill-removal-default, or complete-maintenance",
            )
        })?;
    let values = parse_named_arguments(args.collect())?;
    match action.as_str() {
        "install" => {
            require_only(
                &values,
                &[
                    "--install-root",
                    "--user-profile-root",
                    "--package-root",
                    "--build-id",
                    "--display-version",
                    "--numeric-version",
                    "--install-manager",
                    "--install-fault",
                    "--fault-marker-prefix",
                ],
            )?;
            installer_helper_lifecycle::install(InstallOptions {
                install_root: required_path(&values, "--install-root")?,
                user_profile_root: required_path(&values, "--user-profile-root")?,
                package_root: required_path(&values, "--package-root")?,
                build_id: BuildId::parse(&required_utf8(&values, "--build-id")?)?,
                display_version: required_utf8(&values, "--display-version")?,
                numeric_version: required_utf8(&values, "--numeric-version")?,
                install_manager: match required_utf8(&values, "--install-manager")?.as_str() {
                    "Direct" => InstallManager::Direct,
                    "WinGet" => InstallManager::WinGet,
                    value => {
                        return Err(invalid_data(format!(
                            "invalid --install-manager value {value:?}"
                        )))
                    }
                },
                fault: optional_utf8(&values, "--install-fault")?,
                fault_marker_prefix: optional_utf8(&values, "--fault-marker-prefix")?
                    .unwrap_or_else(|| "herdr".to_string()),
            })
        }
        "uninstall" => {
            require_only(
                &values,
                &[
                    "--install-root",
                    "--user-profile-root",
                    "--skill-hash-manifest",
                    "--settings-disposition",
                    "--skill-disposition",
                    "--uninstall-fault",
                    "--fault-marker-prefix",
                    "--quiet-runner-process-id",
                    "--quiet-token",
                ],
            )?;
            installer_helper_lifecycle::uninstall(UninstallOptions {
                install_root: required_path(&values, "--install-root")?,
                user_profile_root: required_path(&values, "--user-profile-root")?,
                skill_hash_manifest: required_path(&values, "--skill-hash-manifest")?,
                settings_disposition: match required_utf8(&values, "--settings-disposition")?
                    .as_str()
                {
                    "Keep" => SettingsDisposition::Keep,
                    "Remove" => SettingsDisposition::Remove,
                    value => {
                        return Err(invalid_data(format!(
                            "invalid --settings-disposition value {value:?}"
                        )))
                    }
                },
                skill_disposition: parse_skill_disposition(&required_utf8(
                    &values,
                    "--skill-disposition",
                )?)?,
                fault: optional_utf8(&values, "--uninstall-fault")?,
                fault_marker_prefix: optional_utf8(&values, "--fault-marker-prefix")?
                    .unwrap_or_else(|| "herdr".to_string()),
                quiet_runner: parse_quiet_runner(&values)?,
            })
        }
        "quiet-uninstall" => {
            require_only(&values, &["--install-root"])?;
            installer_helper_lifecycle::quiet_uninstall(QuietUninstallOptions {
                install_root: required_path(&values, "--install-root")?,
            })
        }
        "skill-removal-default" => {
            require_only(&values, &["--user-profile-root", "--skill-hash-manifest"])?;
            installer_helper_lifecycle::skill_removal_default(SkillDefaultOptions {
                user_profile_root: required_path(&values, "--user-profile-root")?,
                skill_hash_manifest: required_path(&values, "--skill-hash-manifest")?,
            })
        }
        "complete-maintenance" => {
            require_only(&values, &["--install-root", "--parent-process-id"])?;
            installer_helper_lifecycle::complete_maintenance(MaintenanceOptions {
                install_root: required_path(&values, "--install-root")?,
                parent_process_id: optional_utf8(&values, "--parent-process-id")?
                    .map(|value| {
                        value.parse::<u32>().map_err(|_| {
                            invalid_data(format!("invalid parent process ID {value:?}"))
                        })
                    })
                    .transpose()?
                    .unwrap_or(0),
            })
        }
        _ => Err(invalid_data(format!(
            "unknown installer helper action {action:?}"
        ))),
    }
}

fn parse_quiet_runner(
    values: &BTreeMap<String, OsString>,
) -> io::Result<Option<QuietRunnerOptions>> {
    let process_id = optional_utf8(values, "--quiet-runner-process-id")?;
    let token = optional_utf8(values, "--quiet-token")?;
    match (process_id, token) {
        (None, None) => Ok(None),
        (Some(process_id), Some(token)) => Ok(Some(QuietRunnerOptions {
            process_id: process_id.parse::<u32>().map_err(|_| {
                invalid_data(format!("invalid quiet-uninstall process ID {process_id:?}"))
            })?,
            token,
        })),
        _ => Err(invalid_data(
            "quiet-uninstall process ID and token must be provided together",
        )),
    }
}

fn parse_named_arguments(args: Vec<OsString>) -> io::Result<BTreeMap<String, OsString>> {
    if !args.len().is_multiple_of(2) {
        return Err(invalid_data(
            "installer helper arguments must be --name value pairs",
        ));
    }
    let mut output = BTreeMap::new();
    for pair in args.chunks_exact(2) {
        let name = pair[0]
            .to_str()
            .ok_or_else(|| invalid_data("installer helper option name is not UTF-8"))?;
        if !name.starts_with("--") || name.len() < 3 {
            return Err(invalid_data(format!(
                "invalid installer helper option {name:?}"
            )));
        }
        if output.insert(name.to_string(), pair[1].clone()).is_some() {
            return Err(invalid_data(format!(
                "duplicate installer helper option {name:?}"
            )));
        }
    }
    Ok(output)
}

fn require_only(values: &BTreeMap<String, OsString>, allowed: &[&str]) -> io::Result<()> {
    if let Some(name) = values.keys().find(|name| !allowed.contains(&name.as_str())) {
        return Err(invalid_data(format!(
            "installer helper action does not accept option {name}"
        )));
    }
    Ok(())
}

fn required_path(values: &BTreeMap<String, OsString>, name: &str) -> io::Result<PathBuf> {
    values
        .get(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| invalid_data(format!("missing required installer helper option {name}")))
}

fn required_utf8(values: &BTreeMap<String, OsString>, name: &str) -> io::Result<String> {
    optional_utf8(values, name)?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_data(format!("missing required installer helper option {name}")))
}

fn optional_utf8(values: &BTreeMap<String, OsString>, name: &str) -> io::Result<Option<String>> {
    values
        .get(name)
        .map(|value| {
            value
                .clone()
                .into_string()
                .map_err(|_| invalid_data(format!("installer helper option {name} is not UTF-8")))
        })
        .transpose()
}

fn parse_skill_disposition(value: &str) -> io::Result<SkillDisposition> {
    match value {
        "Keep" => Ok(SkillDisposition::Keep),
        "Auto" => Ok(SkillDisposition::Auto),
        "Remove" => Ok(SkillDisposition::Remove),
        _ => Err(invalid_data(format!(
            "invalid --skill-disposition value {value:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_rejects_duplicates_and_unpaired_values() {
        assert!(parse_named_arguments(vec![OsString::from("--one")]).is_err());
        assert!(parse_named_arguments(vec![
            OsString::from("--one"),
            OsString::from("a"),
            OsString::from("--one"),
            OsString::from("b"),
        ])
        .is_err());
    }

    #[test]
    fn quiet_runner_requires_a_complete_pair() {
        let mut values = BTreeMap::new();
        values.insert(
            "--quiet-runner-process-id".to_string(),
            OsString::from("42"),
        );
        assert!(parse_quiet_runner(&values).is_err());
        values.insert("--quiet-token".to_string(), OsString::from("a".repeat(32)));
        let parsed = parse_quiet_runner(&values).unwrap().unwrap();
        assert_eq!(parsed.process_id, 42);
        assert_eq!(parsed.token, "a".repeat(32));
    }

    #[test]
    fn action_options_are_exact() {
        let mut values = BTreeMap::new();
        values.insert("--install-root".to_string(), OsString::from(r"C:\Herdr"));
        values.insert("--quiet-token".to_string(), OsString::from("token"));
        assert!(require_only(&values, &["--install-root"]).is_err());
    }
}
