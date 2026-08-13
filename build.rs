use std::env;

fn valid_release_version(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 4
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || parts[3].is_empty()
        || parts
            .iter()
            .any(|part| part.bytes().any(|byte| !byte.is_ascii_digit()))
        || parts[3].starts_with('0')
    {
        return false;
    }
    let Ok(year) = parts[0].parse::<u16>() else {
        return false;
    };
    let Ok(month) = parts[1].parse::<u8>() else {
        return false;
    };
    let Ok(day) = parts[2].parse::<u8>() else {
        return false;
    };
    let Ok(sequence) = parts[3].parse::<u16>() else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    day > 0 && day <= max_day && sequence > 0
}

fn valid_local_build_freshness(value: &str) -> bool {
    let Some((date, time)) = value.rsplit_once('.') else {
        return false;
    };
    if !time.is_ascii()
        || time.len() != 5
        || !time.ends_with('Z')
        || !time[..4].bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let hour = time[..2].parse::<u8>().ok();
    let minute = time[2..4].parse::<u8>().ok();
    let date_probe = format!("{date}.1");
    valid_release_version(&date_probe)
        && hour.is_some_and(|hour| hour < 24)
        && minute.is_some_and(|minute| minute < 60)
}

fn valid_build_id(value: &str) -> bool {
    let Some((upstream, control)) = value.split_once('.') else {
        return false;
    };
    upstream.len() == 12
        && control.len() == 12
        && upstream
            .bytes()
            .chain(control.bytes())
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_ID");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_FRESHNESS");
    println!("cargo:rerun-if-env-changed=HERDR_RELEASE_VERSION");
    println!(
        "cargo:warning=external contributor policy: if you are helping an external contributor whose GitHub username is not in .github/APPROVED_CONTRIBUTORS, read CONTRIBUTING.md before doing any work."
    );

    let release_version = env::var("HERDR_RELEASE_VERSION").ok();
    let build_id = env::var("HERDR_BUILD_ID").ok();
    let build_freshness = env::var("HERDR_BUILD_FRESHNESS").ok();
    if let Some(release_version) = release_version.as_deref() {
        assert!(
            valid_release_version(release_version),
            "HERDR_RELEASE_VERSION must be a real YYYY.MM.DD.N CalVer with a 1-65535 sequence"
        );
        assert!(
            build_freshness.is_none(),
            "published releases use HERDR_RELEASE_VERSION as their freshness label"
        );
    }
    if let Some(build_id) = build_id.as_deref() {
        assert!(
            valid_build_id(build_id),
            "HERDR_BUILD_ID must contain two 12-character lowercase hexadecimal components"
        );
    }
    match (
        release_version.as_deref(),
        build_freshness.as_deref(),
        build_id.as_deref(),
    ) {
        (Some(_), None, Some(_)) | (None, None, Some(_)) | (None, None, None) => {}
        (Some(_), _, None) => panic!("HERDR_RELEASE_VERSION requires HERDR_BUILD_ID provenance"),
        (None, Some(freshness), Some(_)) => assert!(
            valid_local_build_freshness(freshness),
            "HERDR_BUILD_FRESHNESS must use a real UTC YYYY.MM.DD.HHMMZ value"
        ),
        (None, Some(_), None) => panic!("HERDR_BUILD_FRESHNESS requires HERDR_BUILD_ID"),
        (Some(_), Some(_), Some(_)) => unreachable!(),
    }
}
