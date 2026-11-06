use std::path::Path;
use std::process::{Command, Stdio};

const REPO: &str = "https://github.com/rokartur/dockwatch";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Only CI release builds update themselves, so a local `cargo build` never gets replaced.
pub fn enabled() -> bool {
    option_env!("DOCKWATCH_RELEASE").is_some()
}

/// The latest release when it is newer than this build; Err when GitHub cannot be reached.
pub fn newer_release() -> Result<Option<String>, String> {
    // The web redirect to the latest tag is not the REST API, so it has no 60 calls an hour limit.
    let location = run(Command::new("curl").args(["-fsSI", "-o", "/dev/null", "-w", "%{redirect_url}", "--max-time", "10", &format!("{REPO}/releases/latest")]))
        .map_err(|e| format!("Cannot check for updates: {e}"))?;
    Ok(released_version(&location).filter(|latest| is_newer(latest, CURRENT)).map(str::to_owned))
}

/// Replaces the running binary with release `version`, once the download answers `--version` with it.
pub fn install(version: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("Cannot find the dockwatch binary: {e}"))?;
    let staged = exe.with_file_name(format!(".dockwatch-update-{}", std::process::id()));
    let result = download(version, &staged).and_then(|()| verify(&staged, version)).and_then(|()| std::fs::rename(&staged, &exe).map_err(|e| format!("Cannot replace {}: {e}", exe.display())));
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// A repo without releases redirects to `/releases` instead of `/releases/tag/v0.1.0`.
fn released_version(location: &str) -> Option<&str> {
    location.trim().rsplit_once("/tag/v").map(|(_, version)| version)
}

fn download(version: &str, path: &Path) -> Result<(), String> {
    let url = format!("{REPO}/releases/download/v{version}/dockwatch-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    // Created first, so an unwritable binary folder gets this message instead of curl's.
    std::fs::File::create(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            format!("No write access to {}, where dockwatch is installed. Reinstall it to a folder you own.", path.parent().unwrap_or(path).display())
        }
        _ => format!("Cannot write {}: {e}", path.display()),
    })?;
    run(Command::new("curl").args(["-fsSL", "--max-time", "300", "-o"]).arg(path).arg(&url)).map_err(|e| format!("Cannot download {url}: {e}"))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("Cannot make {} executable: {e}", path.display()))
}

/// A truncated or wrong download must never replace a working binary.
fn verify(path: &Path, version: &str) -> Result<(), String> {
    let reported = run(Command::new(path).arg("--version"))?;
    if reported.trim() != format!("dockwatch {version}") {
        return Err(format!("The downloaded dockwatch reports {:?}, expected {version}.", reported.trim()));
    }
    Ok(())
}

fn run(command: &mut Command) -> Result<String, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command.stdin(Stdio::null()).output().map_err(|e| format!("Cannot run {program}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{program} failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A pre-release such as `0.2.0-beta.1` does not parse, so it never counts as newer.
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |version: &str| version.split('.').map(|n| n.parse::<u64>().ok()).collect::<Option<Vec<_>>>();
    matches!((parse(latest), parse(current)), (Some(latest), Some(current)) if latest > current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_higher_release_counts_as_newer() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.2.0-beta.1", "0.1.0"));
    }

    #[test]
    fn version_comes_from_the_latest_release_redirect() {
        assert_eq!(released_version("https://github.com/rokartur/dockwatch/releases/tag/v0.2.0\n"), Some("0.2.0"));
        assert_eq!(released_version("https://github.com/rokartur/dockwatch/releases"), None);
    }
}
