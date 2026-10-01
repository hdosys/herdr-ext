use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn zig_target(target: &str) -> &str {
    match target {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "x86_64-apple-darwin" => "x86_64-macos",
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-pc-windows-msvc" => "x86_64-windows-msvc",
        "aarch64-pc-windows-msvc" => "aarch64-windows-msvc",
        other => panic!("unsupported target for libghostty-vt build: {other}"),
    }
}

fn env_bool(name: &str) -> Option<bool> {
    match env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            other => panic!("invalid boolean value for {name}: {other}"),
        },
        Err(env::VarError::NotPresent) => None,
        Err(err) => panic!("failed to read {name}: {err}"),
    }
}

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
    println!("cargo:rerun-if-changed=vendor/libghostty-vt.vendor.json");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/build.zig");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/build.zig.zon");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/include");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/pkg");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/src");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/VERSION");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_OPTIMIZE");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_SIMD");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_ZIG_SYSTEM_DIR");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_ID");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_FRESHNESS");
    println!("cargo:rerun-if-env-changed=HERDR_RELEASE_VERSION");
    println!("cargo:rerun-if-env-changed=ZIG");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_WINDOWS_LIBC");
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

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendored_dir = manifest_dir.join("vendor/libghostty-vt");
    let optimize = env::var("LIBGHOSTTY_VT_OPTIMIZE").unwrap_or_else(|_| "ReleaseFast".into());
    let simd = env_bool("LIBGHOSTTY_VT_SIMD").unwrap_or(true);
    let target = env::var("TARGET").expect("TARGET");
    let zig_target = zig_target(&target);
    let version_string = fs::read_to_string(vendored_dir.join("VERSION"))
        .expect("failed to read vendored libghostty-vt VERSION")
        .trim()
        .to_string();

    let zig = env::var("ZIG").unwrap_or_else(|_| "zig".into());
    let zig_cache = vendored_dir.join(".zig-cache");
    let mut command = Command::new(&zig);
    command
        .arg("build")
        .arg("--cache-dir")
        .arg(zig_cache.join("local"))
        .arg("--global-cache-dir")
        .arg(zig_cache.join("global"))
        .arg("-Demit-lib-vt")
        .arg(format!("-Doptimize={optimize}"))
        .arg(format!("-Dsimd={simd}"))
        .arg(format!("-Dtarget={zig_target}"))
        .arg(format!("-Dversion-string={version_string}"))
        .arg("-Demit-xcframework=false");
    if target.ends_with("windows-msvc") {
        if let Some(libc_file) = env::var_os("LIBGHOSTTY_VT_WINDOWS_LIBC") {
            println!(
                "cargo:rerun-if-changed={}",
                PathBuf::from(&libc_file).display()
            );
            command.arg("--libc").arg(libc_file);
        }
    }
    if let Ok(system_dir) = env::var("LIBGHOSTTY_VT_ZIG_SYSTEM_DIR") {
        command.arg("--system").arg(system_dir);
    }

    let status = command
        .current_dir(&vendored_dir)
        .status()
        .unwrap_or_else(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                panic!(
                    "zig executable not found (looked for {zig:?}; set the ZIG \
                     environment variable to point at the zig binary). Building \
                     the vendored libghostty-vt requires Zig 0.16.0: install it from \
                     https://ziglang.org/download/, then retry the build"
                );
            }
            panic!("failed to execute zig build for vendored libghostty-vt: {err}");
        });
    assert!(
        status.success(),
        "zig build for vendored libghostty-vt failed: {status}"
    );

    let lib_dir = vendored_dir.join("zig-out/lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    if target.contains("apple-darwin") {
        let static_lib = lib_dir.join("libghostty-vt.a");
        println!("cargo:rustc-link-arg={}", static_lib.display());
    } else if target.contains("windows-msvc") {
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }
}
