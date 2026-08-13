//! Build identity helpers.

pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn channel() -> &'static str {
    crate::distribution::UPDATE_CHANNEL
}

pub fn build_id() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_ID"))
}

pub fn release_version() -> Option<&'static str> {
    non_empty(option_env!("HERDR_RELEASE_VERSION"))
}

pub fn build_freshness() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_FRESHNESS"))
}

pub fn release_identity() -> &'static str {
    release_version().unwrap_or("local")
}

/// Exact runtime identity used by status, handoff, and matching remote binaries.
pub fn version() -> String {
    version_for(release_version(), build_freshness(), build_id())
}

/// User-facing version identity. Published builds expose only their CalVer;
/// unpublished builds retain the detailed local runtime identity.
pub fn display_version() -> String {
    display_version_for(release_version(), build_freshness(), build_id())
}

fn display_version_for(
    release_version: Option<&str>,
    build_freshness: Option<&str>,
    build_id: Option<&str>,
) -> String {
    release_version
        .map(str::to_owned)
        .unwrap_or_else(|| version_for(None, build_freshness, build_id))
}

fn version_for(
    release_version: Option<&str>,
    build_freshness: Option<&str>,
    build_id: Option<&str>,
) -> String {
    match (release_version, build_freshness, build_id) {
        (Some(release), _, Some(build)) => format!("{release}+{build}"),
        (Some(release), _, None) => release.to_string(),
        (None, Some(freshness), Some(build)) => format!("{freshness}+{build}"),
        (None, Some(freshness), None) => freshness.to_string(),
        (None, None, Some(build)) => format!("local+{build}"),
        (None, None, None) => "local".to_string(),
    }
}

pub fn cli_version() -> String {
    cli_version_for(
        release_version(),
        build_freshness(),
        BASE_VERSION,
        build_id(),
    )
}

fn cli_version_for(
    release_version: Option<&str>,
    build_freshness: Option<&str>,
    herdr_version: &str,
    build_id: Option<&str>,
) -> String {
    let name = crate::distribution::CLI_VERSION_NAME;
    match (release_version, build_freshness, build_id) {
        (Some(release), _, _) => format!("{name} {release} (Herdr {herdr_version})"),
        (None, Some(freshness), Some(build)) => {
            format!("{name} {freshness} (local, Herdr {herdr_version}, build {build})")
        }
        (None, Some(freshness), None) => {
            format!("{name} {freshness} (local, Herdr {herdr_version})")
        }
        (None, None, Some(build)) => {
            format!("{name} local (Herdr {herdr_version}, build {build})")
        }
        (None, None, None) => format!("{name} local (Herdr {herdr_version})"),
    }
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_identity_separates_release_order_from_exact_build() {
        assert_eq!(
            super::version_for(
                Some("2026.08.11.1"),
                None,
                Some("0123456789ab.cdef01234567")
            ),
            "2026.08.11.1+0123456789ab.cdef01234567"
        );
        assert_eq!(
            super::version_for(
                None,
                Some("2026.08.28.1045Z"),
                Some("0123456789ab.cdef01234567")
            ),
            "2026.08.28.1045Z+0123456789ab.cdef01234567"
        );
    }

    #[test]
    fn display_identity_keeps_public_calver_exact_and_local_provenance_detailed() {
        assert_eq!(
            super::display_version_for(
                Some("2026.08.11.1"),
                None,
                Some("0123456789ab.cdef01234567")
            ),
            "2026.08.11.1"
        );
        assert_eq!(
            super::display_version_for(
                None,
                Some("2026.08.28.1045Z"),
                Some("0123456789ab.cdef01234567")
            ),
            "2026.08.28.1045Z+0123456789ab.cdef01234567"
        );
    }

    #[test]
    fn published_cli_version_leads_with_calver() {
        assert_eq!(
            super::cli_version_for(
                Some("2026.08.11.1"),
                None,
                "0.8.0",
                Some("0123456789ab.cdef01234567")
            ),
            "herdr-win 2026.08.11.1 (Herdr 0.8.0)"
        );
    }

    #[test]
    fn local_cli_version_retains_build_provenance() {
        assert_eq!(
            super::cli_version_for(
                None,
                Some("2026.08.28.1045Z"),
                "0.8.0",
                Some("0123456789ab.cdef01234567")
            ),
            "herdr-win 2026.08.28.1045Z (local, Herdr 0.8.0, build 0123456789ab.cdef01234567)"
        );
    }
}
