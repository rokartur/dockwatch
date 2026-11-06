use crate::docker::Docker;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::thread;

#[derive(Clone, Debug, PartialEq)]
pub struct Runtime {
    pub name: String,
    pub socket: String,
    /// Why the last ping failed; None while the runtime answers.
    pub error: Option<String>,
    /// Picked by `DOCKER_HOST` or the current Docker context.
    pub current: bool,
}

impl Runtime {
    pub fn running(&self) -> bool {
        self.error.is_none()
    }
}

const KNOWN_SOCKETS: [(&str, &str); 8] = [
    ("Docker Desktop", "~/.docker/run/docker.sock"),
    ("OrbStack", "~/.orbstack/run/docker.sock"),
    ("Lighter", "~/.lighter/docker.sock"),
    ("Colima", "~/.colima/default/docker.sock"),
    ("Colima", "~/.config/colima/default/docker.sock"),
    ("Rancher Desktop", "~/.rd/docker.sock"),
    ("Lima", "~/.lima/docker/sock/docker.sock"),
    ("Docker", "/var/run/docker.sock"),
];

/// Runtimes from `DOCKER_HOST`, every Docker context and the known socket paths, each pinged.
pub fn discover() -> Vec<Runtime> {
    let home = std::env::var("HOME").unwrap_or_default();
    let docker_host = std::env::var("DOCKER_HOST").ok();
    let (contexts, current) = read_contexts(&home);
    let mut runtimes = candidates(&home, docker_host.as_deref(), &contexts, current.as_deref(), |path| Path::new(path).exists());
    thread::scope(|scope| {
        for runtime in &mut runtimes {
            scope.spawn(|| runtime.error = Docker { socket: runtime.socket.clone() }.ping().err().map(|error| error.message));
        }
    });
    runtimes
}

/// The preferred socket when it runs, else the current Docker context, else the first running runtime.
pub fn launch_runtime<'a>(runtimes: &'a [Runtime], preferred: Option<&str>) -> Option<&'a Runtime> {
    runtimes
        .iter()
        .find(|runtime| Some(runtime.socket.as_str()) == preferred && runtime.running())
        .or_else(|| runtimes.iter().find(|runtime| runtime.current && runtime.running()))
        .or_else(|| runtimes.iter().find(|runtime| runtime.running()))
}

/// Context name and endpoint host from `$DOCKER_CONFIG/contexts/meta/*/meta.json`, plus the current
/// context, which `DOCKER_CONTEXT` overrides as it does for the docker CLI.
fn read_contexts(home: &str) -> (Vec<(String, String)>, Option<String>) {
    let docker_dir = std::env::var("DOCKER_CONFIG").map(PathBuf::from).unwrap_or_else(|_| Path::new(home).join(".docker"));
    let mut contexts = Vec::new();
    for entry in std::fs::read_dir(docker_dir.join("contexts/meta")).into_iter().flatten().flatten() {
        let Some(meta) = read_json(&entry.path().join("meta.json")) else { continue };
        if let (Some(name), Some(host)) = (meta["Name"].as_str(), meta["Endpoints"]["docker"]["Host"].as_str()) {
            contexts.push((name.to_string(), host.to_string()));
        }
    }
    let current = read_json(&docker_dir.join("config.json")).and_then(|config| config["currentContext"].as_str().map(String::from));
    let current = std::env::var("DOCKER_CONTEXT").ok().or(current);
    (contexts, current)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Ordered and deduplicated by real socket path: `DOCKER_HOST`, contexts by name, then known paths
/// that exist. The `default` context is skipped, it only repeats `DOCKER_HOST` or `/var/run/docker.sock`.
/// Remote hosts (`ssh://`, `tcp://`) are skipped too.
fn candidates(home: &str, docker_host: Option<&str>, contexts: &[(String, String)], current_context: Option<&str>, exists: impl Fn(&str) -> bool) -> Vec<Runtime> {
    let known: Vec<(&str, String)> = KNOWN_SOCKETS.iter().map(|(name, path)| (*name, expand_home(path, home))).collect();
    let known_name = |socket: &str| known.iter().find(|(_, path)| real_path(path) == real_path(socket)).map(|(name, _)| name.to_string());
    let runtime = |name: String, socket: &str, current: bool| Runtime { name, socket: socket.to_string(), error: None, current };

    let mut found = Vec::new();
    if let Some(socket) = docker_host.and_then(unix_socket) {
        found.push(runtime(known_name(socket).unwrap_or("DOCKER_HOST".into()), socket, true));
    }
    let mut contexts: Vec<_> = contexts.iter().filter(|(name, _)| name != "default").collect();
    contexts.sort();
    for (name, host) in contexts {
        let Some(socket) = unix_socket(host) else { continue };
        let current = docker_host.is_none() && current_context == Some(name.as_str());
        found.push(runtime(known_name(socket).unwrap_or(name.clone()), socket, current));
    }
    for (name, path) in &known {
        if exists(path) {
            found.push(runtime(name.to_string(), path, false));
        }
    }

    let mut runtimes: Vec<Runtime> = Vec::new();
    for runtime in found {
        match runtimes.iter_mut().find(|seen| real_path(&seen.socket) == real_path(&runtime.socket)) {
            Some(seen) => seen.current |= runtime.current,
            None => runtimes.push(runtime),
        }
    }
    runtimes
}

fn unix_socket(host: &str) -> Option<&str> {
    host.strip_prefix("unix://")
}

fn expand_home(path: &str, home: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{home}/{rest}"),
        None => path.to_string(),
    }
}

fn real_path(path: &str) -> String {
    std::fs::canonicalize(path).map(|real| real.to_string_lossy().into_owned()).unwrap_or(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_and_names_candidates() {
        let contexts = vec![
            ("orbstack".to_string(), "unix:///h/.orbstack/run/docker.sock".to_string()),
            ("default".to_string(), "unix:///var/run/docker.sock".to_string()),
            ("remote".to_string(), "ssh://box".to_string()),
            ("colima".to_string(), "unix:///h/.colima/default/docker.sock".to_string()),
        ];
        let exists = |path: &str| path == "/h/.orbstack/run/docker.sock" || path == "/var/run/docker.sock";
        let runtimes = candidates("/h", None, &contexts, Some("orbstack"), exists);
        let names: Vec<_> = runtimes.iter().map(|runtime| (runtime.name.as_str(), runtime.current)).collect();
        assert_eq!(names, [("Colima", false), ("OrbStack", true), ("Docker", false)]);
    }

    #[test]
    fn docker_host_comes_first_and_wins_current() {
        let contexts = vec![("lighter".to_string(), "unix:///h/.lighter/docker.sock".to_string())];
        let runtimes = candidates("/h", Some("unix:///tmp/my.sock"), &contexts, Some("lighter"), |_| false);
        assert_eq!(runtimes[0].name, "DOCKER_HOST");
        assert!(runtimes[0].current && !runtimes[1].current);
    }

    #[test]
    fn launches_the_preferred_then_current_then_first_running() {
        let runtime = |socket: &str, running: bool, current| Runtime { name: socket.into(), socket: socket.into(), error: (!running).then(|| "stopped".into()), current };
        let runtimes = [runtime("a", true, false), runtime("b", true, true), runtime("c", false, false)];
        assert_eq!(launch_runtime(&runtimes, Some("a")).unwrap().socket, "a");
        assert_eq!(launch_runtime(&runtimes, Some("c")).unwrap().socket, "b");
        assert_eq!(launch_runtime(&runtimes[..1], None).unwrap().socket, "a");
    }
}
