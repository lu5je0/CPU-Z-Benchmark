//! `-u` / `--update`: replace this binary with the newest GitHub release.
//!
//! Networking goes through the system `curl` (no HTTP dependency in the crate).
//! The downloaded asset is written next to the running binary, checked twice
//! (native-executable magic, then `-V` must report the tag's version) and only
//! then renamed over it, so a truncated download or an HTML error page can
//! never clobber a working binary.
//!
//! Environment overrides (mostly for testing):
//!   CPUZ_BENCH_TAG            update to this tag instead of asking for `latest`
//!   CPUZ_BENCH_URL_PREFIX     download prefix (default: GitHub release of the tag)
//!   CPUZ_BENCH_LATEST_URL     URL probed for the newest tag

use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "lu5je0/CPU-Z-Benchmark";
const CHECK_TIMEOUT: &str = "10";
const DOWNLOAD_TIMEOUT: &str = "300";

/// Platforms the release workflow builds assets for.
const SUPPORTED: [&str; 4] = [
    "linux-x86_64",
    "linux-aarch64",
    "macos-aarch64",
    "android-aarch64",
];

/// Run `-u`: returns the process exit code.
pub fn run(current_version: &str) -> i32 {
    match update(current_version) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("cpuz-bench: update failed: {err}");
            1
        }
    }
}

fn update(current_version: &str) -> Result<(), String> {
    let key = platform_key().ok_or_else(|| {
        format!(
            "no release asset for {}/{}",
            env::consts::OS,
            env::consts::ARCH
        )
    })?;
    if !SUPPORTED.contains(&key.as_str()) {
        return Err(format!(
            "no release asset for {key} (built for {})",
            SUPPORTED.join(", ")
        ));
    }
    let exe = env::current_exe().map_err(|e| format!("cannot locate this binary: {e}"))?;
    let asset = format!("cpuz-bench-{key}");

    let tag = match env_opt("CPUZ_BENCH_TAG") {
        Some(tag) => tag,
        None => latest_tag()?,
    };
    let version = tag.trim_start_matches('v').to_string();
    if version == current_version {
        println!("cpuz-bench {current_version} is up to date (latest {tag})");
        return Ok(());
    }

    let url = format!("{}/{asset}", url_prefix(&tag));
    println!("update: {current_version} -> {version} ({url})");
    let tmp = temp_path(&exe);
    let _ = fs::remove_file(&tmp);
    // Fail early (and clearly) when the binary sits in a read-only location,
    // instead of surfacing a confusing curl write error.
    fs::write(&tmp, b"").map_err(|e| {
        format!(
            "cannot write next to {}: {e}\n  (if this binary lives in a read-only location such as a \
             nix store, a package manager tree or /usr/local, install it somewhere writable)",
            exe.display()
        )
    })?;
    download(&url, &tmp)?;
    if let Err(err) = verify(&tmp, &version) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = install(&tmp, &exe) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    println!("update: {} is now {version}", exe.display());
    Ok(())
}

fn env_opt(name: &str) -> Option<String> {
    let value = env::var(name).ok()?;
    let value = value.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// `linux-x86_64` style platform key, matching the release asset names.
fn platform_key() -> Option<String> {
    let os = match env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        "android" => "android",
        "windows" => "windows",
        _ => return None,
    };
    let arch = match env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return None,
    };
    Some(format!("{os}-{arch}"))
}

fn url_prefix(tag: &str) -> String {
    env_opt("CPUZ_BENCH_URL_PREFIX")
        .map(|p| p.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("https://github.com/{REPO}/releases/{tag}/download"))
}

/// Newest release tag, read from the redirect of `.../releases/latest`.
fn latest_tag() -> Result<String, String> {
    let url = env_opt("CPUZ_BENCH_LATEST_URL")
        .unwrap_or_else(|| format!("https://github.com/{REPO}/releases/latest"));
    let out = curl(&[
        "-sSIL",
        "-o",
        dev_null(),
        "-w",
        "%{url_effective}",
        "--connect-timeout",
        CHECK_TIMEOUT,
        "--max-time",
        CHECK_TIMEOUT,
        &url,
    ])
    .output()
    .map_err(|e| format!("cannot run curl ({e}); install curl or download the asset manually"))?;
    if !out.status.success() {
        return Err(format!(
            "cannot reach {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let effective = String::from_utf8_lossy(&out.stdout);
    effective
        .split("/tag/")
        .nth(1)
        .map(|tag| tag.trim().trim_end_matches('/').to_string())
        .filter(|tag| !tag.is_empty())
        .ok_or_else(|| format!("no release tag found at {url}"))
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    let mut args = vec!["-fL", "--connect-timeout", CHECK_TIMEOUT, "--max-time", DOWNLOAD_TIMEOUT];
    if std::io::stderr().is_terminal() {
        args.push("--progress-bar");
    } else {
        args.push("-sS");
    }
    args.push("-o");
    let dest_str = dest.to_string_lossy().to_string();
    args.push(&dest_str);
    args.push(url);
    let out = curl(&args)
        .output()
        .map_err(|e| format!("cannot run curl ({e}); install curl or download the asset manually"))?;
    if !out.status.success() {
        return Err(format!(
            "download failed: {url}\n  {}\n  (check the network, or pick a version with CPUZ_BENCH_TAG=<tag>)",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dest, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot chmod {}: {e}", dest.display()))?;
    }
    Ok(())
}

/// Two cheap sanity checks so a broken download never replaces a good binary.
fn verify(path: &Path, version: &str) -> Result<(), String> {
    let data = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !has_native_magic(&data) {
        return Err(format!(
            "{} is not a native executable ({} bytes) — refusing to install it",
            path.display(),
            data.len()
        ));
    }
    let out = Command::new(path)
        .arg("-V")
        .output()
        .map_err(|e| format!("cannot run the downloaded binary: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let reported = stdout.split_whitespace().last().unwrap_or("");
    if reported != version {
        return Err(format!(
            "downloaded binary reports {reported:?}, expected {version:?}"
        ));
    }
    Ok(())
}

fn has_native_magic(data: &[u8]) -> bool {
    const MAGIC: [[u8; 4]; 7] = [
        [0x7f, b'E', b'L', b'F'],
        [b'M', b'Z', 0, 0], // PE
        [0xcf, 0xfa, 0xed, 0xfe], // Mach-O 64 LE
        [0xce, 0xfa, 0xed, 0xfe], // Mach-O 32 LE
        [0xfe, 0xed, 0xfa, 0xcf], // Mach-O 64 BE
        [0xfe, 0xed, 0xfa, 0xce], // Mach-O 32 BE
        [0xca, 0xfe, 0xba, 0xbe], // Mach-O universal
    ];
    data.len() > 4 && MAGIC.iter().any(|m| m[..] == data[..4])
}

fn temp_path(exe: &Path) -> PathBuf {
    let name = exe
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "cpuz-bench".to_string());
    exe.with_file_name(format!(".{name}.new.{}", std::process::id()))
}

/// Put the verified download in place of the running binary.
///
/// Unix: the rename is atomic and works while the old binary is still running.
#[cfg(unix)]
fn install(tmp: &Path, exe: &Path) -> Result<(), String> {
    fs::rename(tmp, exe).map_err(|e| {
        format!(
            "cannot replace {}: {e}\n  (if this binary lives in a read-only location such as a \
             nix store or /usr/local, install it somewhere writable)",
            exe.display()
        )
    })
}

/// Windows: a running .exe cannot be overwritten, so move it aside first.
#[cfg(windows)]
fn install(tmp: &Path, exe: &Path) -> Result<(), String> {
    let name = exe
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "cpuz-bench.exe".to_string());
    let old = exe.with_file_name(format!("{name}.old"));
    let _ = fs::remove_file(&old);
    fs::rename(exe, &old).map_err(|e| format!("cannot move {} aside: {e}", exe.display()))?;
    match fs::rename(tmp, exe) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::rename(&old, exe);
            Err(format!("cannot replace {}: {e}", exe.display()))
        }
    }
}

fn dev_null() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

fn curl(args: &[&str]) -> Command {
    let mut cmd = Command::new("curl");
    cmd.args(args).stdin(Stdio::null());
    cmd
}
