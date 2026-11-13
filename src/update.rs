use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

pub const RELEASES: &str = "https://github.com/rokartur/dockwatch/releases";
const RELEASES_API: &str = "https://api.github.com/repos/rokartur/dockwatch/releases?per_page=20";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// `latest` follows stable releases only, `pre-release` also takes pre-releases, `off` never checks.
pub const CHANNELS: [&str; 3] = ["latest", "pre-release", "off"];
/// GitHub allows 60 unauthenticated API calls an hour per IP, and every start would spend one.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// What an update is doing right now; `dockwatch update` draws it, the start-up check passes `&|_| ()`.
pub enum Progress<'a> {
    Checking,
    Downloading { version: &'a str, received: u64, total: Option<u64> },
    Verifying,
    Installing,
}

/// The newest release on a channel, with the size of this system's binary when the release lists it.
struct Release {
    version: String,
    size: Option<u64>,
}

/// Only CI release builds update themselves, so a local `cargo build` never gets replaced.
pub fn enabled() -> bool {
    option_env!("DOCKWATCH_RELEASE").is_some()
}

/// Replaces the running binary with the newest release on `channel`, once a day. Returns the installed version.
pub fn install_latest(stamp: &Path, channel: &str) -> Result<Option<String>, String> {
    if channel == "off" || !due(stamp, channel) {
        return Ok(None);
    }
    let latest = latest_release(channel == "pre-release");
    // Ok(None) is an offline start, which checks again next start instead of next day.
    if !matches!(latest, Ok(None)) {
        mark_checked(stamp, channel)?;
    }
    let Some(latest) = latest? else { return Ok(None) };
    install_if_newer(latest, &|_| ())
}

/// `dockwatch update`: checks now, whatever the channel and the last check, and reports an offline start as an error.
pub fn install_now(channel: &str, report: &dyn Fn(Progress)) -> Result<Option<String>, String> {
    report(Progress::Checking);
    let latest = latest_release(channel == "pre-release")?.ok_or("Cannot reach GitHub to check for updates.")?;
    install_if_newer(latest, report)
}

fn install_if_newer(latest: Release, report: &dyn Fn(Progress)) -> Result<Option<String>, String> {
    if !is_newer(&latest.version, CURRENT) {
        return Ok(None);
    }
    let exe = std::env::current_exe().map_err(|e| format!("Cannot find the dockwatch binary: {e}"))?;
    let staged = exe.with_file_name(format!(".dockwatch-update-{}", std::process::id()));
    let result = download(&latest, &staged, report)
        .and_then(|()| {
            report(Progress::Verifying);
            verify(&staged, &latest.version)
        })
        .and_then(|()| {
            report(Progress::Installing);
            std::fs::rename(&staged, &exe).map_err(|e| format!("Cannot replace {}: {e}", exe.display()))
        });
    if let Err(error) = result {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    Ok(Some(latest.version))
}

/// The stamp holds the channel last checked, so switching channels checks at once.
fn due(stamp: &Path, channel: &str) -> bool {
    let Ok(checked) = std::fs::read_to_string(stamp) else { return true };
    let age = std::fs::metadata(stamp).and_then(|meta| meta.modified()).ok().and_then(|modified| modified.elapsed().ok());
    checked != channel || age.is_none_or(|age| age >= CHECK_EVERY)
}

fn mark_checked(stamp: &Path, channel: &str) -> Result<(), String> {
    let dir = stamp.parent().expect("the stamp sits in the config folder");
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    std::fs::write(stamp, channel).map_err(|e| format!("Cannot write {}: {e}", stamp.display()))
}

/// Offline start is normal, so a network failure means no update; an HTTP error such as a 403 rate limit is reported.
fn latest_release(include_prereleases: bool) -> Result<Option<Release>, String> {
    let mut curl = Command::new("curl");
    // No -f: curl 8.7 exits 56 instead of 22 on an HTTP/2 error status, so the status comes from -w.
    curl.args(["-sSL", "--max-time", "10", "-w", "\n%{http_code}", "-H", "Accept: application/vnd.github+json", RELEASES_API]);
    // The token goes through stdin (`-H @-`), so it never shows in `ps`.
    let header = github_token().map(|token| format!("Authorization: Bearer {token}\n"));
    if header.is_some() {
        curl.args(["-H", "@-"]);
    }
    let output = run(&mut curl, header.as_deref().unwrap_or_default().as_bytes())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (body, status) = stdout.rsplit_once('\n').unwrap_or_default();
    // 000 is no answer at all: offline.
    match status {
        "200" => {}
        "000" | "" => return Ok(None),
        status => return Err(format!("Cannot check for updates: GitHub answered {status}.")),
    }
    let Ok(releases) = serde_json::from_str::<Value>(body) else { return Ok(None) };
    // A channel without releases yet counts as up to date.
    Ok(Some(newest(&releases, include_prereleases, &asset()).unwrap_or(Release { version: CURRENT.into(), size: None })))
}

/// GitHub allows a token 5000 API calls an hour instead of 60 per IP. Same order as `gh`: GH_TOKEN, GITHUB_TOKEN, its login.
fn github_token() -> Option<String> {
    let from_env = ["GH_TOKEN", "GITHUB_TOKEN"].into_iter().find_map(|name| std::env::var(name).ok().filter(|token| !token.trim().is_empty()));
    from_env.or_else(|| {
        let output = Command::new("gh").args(["auth", "token"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        let token = String::from_utf8(output.stdout).ok()?.trim().to_owned();
        (output.status.success() && !token.is_empty()).then_some(token)
    })
}

/// A token with access to the repo also lists drafts, whose assets cannot be downloaded.
fn newest(releases: &Value, include_prereleases: bool, asset: &str) -> Option<Release> {
    let (version, release) = releases
        .as_array()?
        .iter()
        .filter(|release| release["draft"] != true)
        .filter(|release| include_prereleases || release["prerelease"] == false)
        .filter_map(|release| Some((release["tag_name"].as_str()?.strip_prefix('v')?, release)))
        .max_by_key(|(version, _)| precedence(version))?;
    let size = release["assets"].as_array().into_iter().flatten().find(|found| found["name"] == asset).and_then(|found| found["size"].as_u64());
    Some(Release { version: version.to_owned(), size })
}

fn asset() -> String {
    format!("dockwatch-{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn download(release: &Release, path: &Path, report: &dyn Fn(Progress)) -> Result<(), String> {
    let (version, asset) = (release.version.as_str(), asset());
    let url = format!("{RELEASES}/download/v{version}/{asset}");
    // Created first, so an unwritable binary folder gets this message instead of curl's.
    std::fs::File::create(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            let dir = path.parent().unwrap_or(path).display();
            format!("No write access to {dir}, where dockwatch is installed. Reinstall it to a folder you own, or set update_channel to off.")
        }
        _ => format!("Cannot write {}: {e}", path.display()),
    })?;
    let mut curl = Command::new("curl").args(["-fsSL", "--max-time", "300", "-o"]).arg(path).arg(&url).stdin(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| format!("Cannot run curl: {e}"))?;
    let received = || std::fs::metadata(path).map_or(0, |meta| meta.len());
    report(Progress::Downloading { version, received: 0, total: release.size });
    let status = loop {
        if let Some(status) = curl.try_wait().map_err(|e| format!("Cannot download {url}: {e}"))? {
            break status;
        }
        report(Progress::Downloading { version, received: received(), total: release.size });
        std::thread::sleep(Duration::from_millis(100));
    };
    if !status.success() {
        let stderr = curl.stderr.take().map(std::io::read_to_string).and_then(Result::ok).unwrap_or_default();
        return Err(format!("Cannot download {url}: {}", stderr.trim()));
    }
    report(Progress::Downloading { version, received: received(), total: release.size });
    let sums_url = format!("{RELEASES}/download/v{version}/SHA256SUMS");
    let sums = run(Command::new("curl").args(["-fsSL", "--max-time", "30", &sums_url]), b"")?;
    if !sums.status.success() {
        return Err(format!("Cannot download {sums_url}: {}", String::from_utf8_lossy(&sums.stderr).trim()));
    }
    check_sum(&String::from_utf8_lossy(&sums.stdout), &asset, &sha256(path)?)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("Cannot make {} executable: {e}", path.display()))
}

/// Linux ships `sha256sum`, macOS `shasum`.
fn sha256(path: &Path) -> Result<String, String> {
    let output = run(Command::new("sha256sum").arg(path), b"").or_else(|_| run(Command::new("shasum").args(["-a", "256"]).arg(path), b""))?;
    let text = String::from_utf8_lossy(&output.stdout);
    match text.split_whitespace().next() {
        Some(digest) if output.status.success() => Ok(digest.to_lowercase()),
        _ => Err(format!("Cannot hash {}: {}", path.display(), String::from_utf8_lossy(&output.stderr).trim())),
    }
}

/// `sums` is the release's SHA256SUMS, one `<hex digest>  <name>` line per asset, as sha256sum prints.
fn check_sum(sums: &str, asset: &str, actual: &str) -> Result<(), String> {
    let line = |line: &str| {
        let mut parts = line.split_whitespace();
        let (digest, name) = (parts.next()?, parts.next()?);
        // sha256sum marks binary mode with a leading star.
        (name.strip_prefix('*').unwrap_or(name) == asset).then(|| digest.to_lowercase())
    };
    let published = sums.lines().find_map(line).ok_or(format!("SHA256SUMS of the release does not list {asset}."))?;
    if published != actual {
        return Err(format!("The downloaded {asset} does not match its published SHA-256."));
    }
    Ok(())
}

/// A truncated or wrong download must never replace a working binary.
fn verify(path: &Path, version: &str) -> Result<(), String> {
    let output = run(Command::new(path).arg("--version"), b"")?;
    let reported = String::from_utf8_lossy(&output.stdout);
    if reported.trim() != format!("dockwatch {version}") {
        return Err(format!("The downloaded dockwatch reports {:?}, expected {version}.", reported.trim()));
    }
    Ok(())
}

/// Runs to the end with `input` on stdin; only a program that cannot start is an Err.
fn run(command: &mut Command, input: &[u8]) -> Result<std::process::Output, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("Cannot run {program}: {e}"))?;
    let _ = child.stdin.take().expect("stdin is piped").write_all(input);
    child.wait_with_output().map_err(|e| format!("Cannot run {program}: {e}"))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (precedence(latest), precedence(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Identifier {
    Number(u64),
    Text(String),
}

/// Semver order: 0.3.0-beta.2 < 0.3.0-beta.10 < 0.3.0-rc.1 < 0.3.0.
fn precedence(version: &str) -> Option<(Vec<u64>, bool, Vec<Identifier>)> {
    // Build metadata (`+1`) does not take part in semver precedence.
    let version = version.split_once('+').map_or(version, |(version, _)| version);
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let core = core.split('.').map(|n| n.parse().ok()).collect::<Option<Vec<u64>>>()?;
    let identifiers = pre.into_iter().flat_map(|pre| pre.split('.')).map(|id| id.parse().map_or_else(|_| Identifier::Text(id.to_string()), Identifier::Number)).collect();
    Some((core, pre.is_none(), identifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_that_differs_from_the_published_sum_is_refused() {
        let asset = "dockwatch-linux-x86_64";
        let sums = format!("{}  dockwatch-macos-aarch64\n{}  *{asset}\n", "a".repeat(64), "B".repeat(64));
        assert_eq!(check_sum(&sums, asset, &"b".repeat(64)), Ok(()));
        assert_eq!(check_sum(&sums, asset, &"c".repeat(64)), Err("The downloaded dockwatch-linux-x86_64 does not match its published SHA-256.".into()));
        assert_eq!(check_sum("aa  dockwatch-linux-x86_64.tar\n", asset, "aa"), Err("SHA256SUMS of the release does not list dockwatch-linux-x86_64.".into()));
    }

    #[test]
    fn hashes_a_file_like_sha256sum() {
        let path = std::env::temp_dir().join(format!("dockwatch-sha-{}", std::process::id()));
        std::fs::write(&path, "abc").unwrap();
        let digest = sha256(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(digest.unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn only_a_higher_release_counts_as_newer() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.1.x", "0.1.0"));
    }

    #[test]
    fn pre_releases_sort_below_their_release() {
        assert!(is_newer("0.3.0-beta.1", "0.2.0"));
        assert!(is_newer("0.3.0-beta.10", "0.3.0-beta.2"));
        assert!(is_newer("0.3.0-rc.1", "0.3.0-beta.10"));
        assert!(is_newer("0.3.1+7", "0.3.0+9") && !is_newer("0.3.0+9", "0.3.0"));
        assert!(is_newer("0.3.0", "0.3.0-rc.1"));
        assert!(!is_newer("0.3.0-rc.1", "0.3.0"));
    }

    #[test]
    fn a_start_checks_at_most_once_a_day_per_channel() {
        let stamp = std::env::temp_dir().join(format!("dockwatch-update-check-{}/update-check", std::process::id()));
        assert!(due(&stamp, "latest"));
        mark_checked(&stamp, "latest").unwrap();
        assert!(!due(&stamp, "latest"));
        assert!(due(&stamp, "pre-release"));
        let file = std::fs::File::options().write(true).open(&stamp).unwrap();
        file.set_modified(std::time::SystemTime::now() - CHECK_EVERY).unwrap();
        assert!(due(&stamp, "latest"));
        std::fs::remove_dir_all(stamp.parent().unwrap()).unwrap();
    }

    #[test]
    fn channel_picks_the_newest_release_it_follows_with_its_binary_size() {
        let releases = serde_json::json!([
            {"tag_name": "v0.4.0", "prerelease": false, "draft": true},
            {"tag_name": "v0.3.0-beta.1", "prerelease": true},
            {"tag_name": "v0.2.1", "prerelease": false, "assets": [{"name": "dockwatch-linux-x86_64", "size": 42}]},
            {"tag_name": "v0.2.0", "prerelease": false},
        ]);
        let pick = |pre| newest(&releases, pre, "dockwatch-linux-x86_64").map(|release| (release.version, release.size));
        assert_eq!(pick(false), Some(("0.2.1".into(), Some(42))));
        assert_eq!(pick(true), Some(("0.3.0-beta.1".into(), None)));
    }
}
