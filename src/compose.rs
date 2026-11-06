use crate::apple;
use crate::docker::Container;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Project {
    pub name: String,
    pub containers: Vec<Container>,
    pub working_dir: Option<String>,
    pub config_files: Vec<String>,
}

impl Project {
    /// Running, paused or restarting containers, the ones `s` stops.
    pub fn running(&self) -> usize {
        self.containers.iter().filter(|container| container.up()).count()
    }
}

const COMPOSE_FILES: [&str; 4] = ["compose.yaml", "compose.yml", "docker-compose.yml", "docker-compose.yaml"];

/// Projects by name from compose labels, plus `local`, the project of the folder dockwatch runs in,
/// which shows even before its first `up`.
pub fn projects(containers: &[Container], local: Option<&Project>) -> Vec<Project> {
    let mut projects: Vec<Project> = local.into_iter().cloned().collect();
    for container in containers {
        let Some(name) = &container.project else { continue };
        let index = match projects.iter().position(|project| &project.name == name) {
            Some(index) => index,
            None => {
                projects.push(Project { name: name.clone(), ..Project::default() });
                projects.len() - 1
            }
        };
        let project = &mut projects[index];
        if project.config_files.is_empty() {
            project.working_dir = container.working_dir.clone();
            project.config_files = container.config_files.clone();
        }
        project.containers.push(container.clone());
    }
    projects.sort_by(|a, b| a.name.cmp(&b.name));
    projects
}

/// The compose file in `folder`, named the way `docker compose` names the project:
/// `COMPOSE_PROJECT_NAME`, else its top-level `name:`, else the folder name lowercased with only
/// `a-z0-9_-` kept and no leading `_` or `-`.
pub fn local_project(folder: &Path) -> Option<Project> {
    let file = COMPOSE_FILES.iter().map(|name| folder.join(name)).find(|path| path.is_file())?;
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let name = std::env::var("COMPOSE_PROJECT_NAME").ok().filter(|name| !name.is_empty()).or_else(|| declared_name(&text)).unwrap_or_else(|| folder_name(folder));
    if name.is_empty() {
        return None;
    }
    Some(Project { name, containers: Vec::new(), working_dir: Some(folder.to_string_lossy().into_owned()), config_files: vec![file.to_string_lossy().into_owned()] })
}

/// shortcut: an interpolated `name: ${VAR}` is skipped, not resolved; fine until a project needs it.
fn declared_name(text: &str) -> Option<String> {
    let value = text.lines().find_map(|line| line.strip_prefix("name:"))?;
    let value = value.split(" #").next().unwrap_or_default().trim().trim_matches(['"', '\'']);
    (!value.is_empty() && !value.contains("${")).then(|| value.to_string())
}

fn folder_name(folder: &Path) -> String {
    let name = folder.file_name().map(|name| name.to_string_lossy().to_lowercase()).unwrap_or_default();
    let kept: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    kept.trim_start_matches(['_', '-']).to_string()
}

/// Compose start order: a batch depends only on earlier batches, so each batch can run at once.
/// A dependency outside `containers` counts as met; members of a cycle go last, together.
pub fn start_batches(containers: &[Container]) -> Vec<Vec<Container>> {
    let services: Vec<&str> = containers.iter().filter_map(|container| container.service.as_deref()).collect();
    let mut left: Vec<Container> = containers.to_vec();
    let mut started: Vec<String> = Vec::new();
    let mut batches = Vec::new();
    while !left.is_empty() {
        let (ready, waiting): (Vec<Container>, Vec<Container>) =
            left.into_iter().partition(|container| container.depends_on.iter().all(|service| started.contains(service) || !services.contains(&service.as_str())));
        if ready.is_empty() {
            batches.push(waiting);
            break;
        }
        started.extend(ready.iter().filter_map(|container| container.service.clone()));
        batches.push(ready);
        left = waiting;
    }
    batches
}

/// `docker compose -p NAME --project-directory DIR -f FILE ... ARGS` against `socket`.
/// Files that are gone are left out, so compose names the real problem.
pub fn command(socket: &str, project: &Project, args: &[&str]) -> Result<Command, String> {
    let files: Vec<&String> = project.config_files.iter().filter(|file| Path::new(file).is_file()).collect();
    if files.is_empty() {
        return Err(format!("No compose file is known for {}. Run dockwatch in its folder.", project.name));
    }
    if socket == apple::SOCKET {
        return Err("Apple container has no compose. Run socktainer and pick its runtime to manage compose projects.".into());
    }
    let mut command = cli(socket)?;
    command.args(["compose", "-p", &project.name]);
    if let Some(folder) = &project.working_dir {
        command.args(["--project-directory", folder]);
    }
    for file in files {
        command.args(["-f", file]);
    }
    command.args(args);
    Ok(command)
}

const RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Runs a CLI command to its end, at most 30 minutes; a failure carries the last lines of its output.
pub fn run(command: Command) -> Result<(), String> {
    let program = Path::new(command.get_program()).file_name().unwrap_or_default().to_string_lossy().into_owned();
    let output = output(command, RUN_TIMEOUT)?;
    if output.status.success() {
        return Ok(());
    }
    // Compose writes its error last, on stderr.
    let text = String::from_utf8_lossy(&output.stdout).into_owned() + &String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    if lines.is_empty() {
        return Err(format!("{program} exited with {}.", output.status));
    }
    Err(lines[lines.len().saturating_sub(15)..].join("\n"))
}

/// Waits at most `timeout`, then kills the command. Any exit status is returned as output.
pub fn output(mut command: Command, timeout: Duration) -> Result<Output, String> {
    let program = Path::new(command.get_program()).file_name().unwrap_or_default().to_string_lossy().into_owned();
    let child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|error| format!("Cannot run {program}: {error}"))?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait_with_output()));
    match rx.recv_timeout(timeout) {
        Ok(output) => output.map_err(|error| format!("Cannot run {program}: {error}")),
        Err(_) => {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            Err(format!("{program} did not finish in {} seconds.", timeout.as_secs()))
        }
    }
}

/// The CLI of the runtime at `socket`: Apple's `container`, else the docker CLI talking to `socket`,
/// whatever context the shell has. `DOCKWATCH_DOCKER` names the docker binary when it is somewhere unusual.
/// `exec -it`, `cp` and `image pull` take the same arguments in both.
pub fn cli(socket: &str) -> Result<Command, String> {
    if socket == apple::SOCKET {
        return find_binary("container").map(Command::new).ok_or("The container CLI is not installed or not on PATH.".into());
    }
    let binary = match std::env::var_os("DOCKWATCH_DOCKER") {
        Some(binary) => PathBuf::from(binary),
        None => find_binary("docker").ok_or("The docker CLI is not installed or not on PATH. Set DOCKWATCH_DOCKER to its path.")?,
    };
    let mut command = Command::new(binary);
    command.env("DOCKER_HOST", format!("unix://{socket}")).env_remove("DOCKER_CONTEXT");
    Ok(command)
}

pub fn find_binary(name: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::env::var("PATH").unwrap_or_default();
    let extra = ["/opt/homebrew/bin", "/usr/local/bin", &format!("{home}/.orbstack/bin"), &format!("{home}/.rd/bin"), "/Applications/Docker.app/Contents/Resources/bin"].map(String::from);
    path.split(':')
        .map(String::from)
        .chain(extra)
        .map(|folder| PathBuf::from(folder).join(name))
        .find(|candidate| candidate.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str, depends_on: &[&str]) -> Container {
        Container { id: name.into(), project: Some("shop".into()), service: Some(name.into()), depends_on: depends_on.iter().map(|name| name.to_string()).collect(), ..Container::default() }
    }

    fn ids(batches: &[Vec<Container>]) -> Vec<Vec<&str>> {
        batches.iter().map(|batch| batch.iter().map(|container| container.id.as_str()).collect()).collect()
    }

    #[test]
    fn batches_follow_depends_on() {
        let containers = [service("web", &["api"]), service("api", &["db", "cache"]), service("db", &[]), service("cache", &["gone"])];
        assert_eq!(ids(&start_batches(&containers)), [vec!["db", "cache"], vec!["api"], vec!["web"]]);
    }

    #[test]
    fn a_cycle_goes_last_together() {
        let containers = [service("a", &["b"]), service("b", &["a"]), service("c", &[])];
        assert_eq!(ids(&start_batches(&containers)), [vec!["c"], vec!["a", "b"]]);
    }

    #[test]
    fn groups_containers_and_keeps_the_local_project() {
        let mut web = service("web", &[]);
        web.config_files = vec!["/srv/shop/compose.yaml".into()];
        let mut solo = service("solo", &[]);
        solo.project = None;
        let local = Project { name: "here".into(), ..Project::default() };
        let projects = projects(&[web, solo], Some(&local));
        let names: Vec<_> = projects.iter().map(|project| (project.name.as_str(), project.containers.len())).collect();
        assert_eq!(names, [("here", 0), ("shop", 1)]);
        assert_eq!(projects[1].config_files, ["/srv/shop/compose.yaml"]);
    }

    #[test]
    fn names_the_local_project_like_compose() {
        let folder = std::env::temp_dir().join(format!("My.Shop_{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("compose.yaml"), "services:\n  web:\n    image: nginx\n").unwrap();
        assert_eq!(local_project(&folder).unwrap().name, format!("myshop_{}", std::process::id()));
        std::fs::write(folder.join("compose.yaml"), "name: \"store\"\nservices: {}\n").unwrap();
        assert_eq!(local_project(&folder).unwrap().name, "store");
        std::fs::write(folder.join("compose.yaml"), "name: store # prod\nservices: {}\n").unwrap();
        assert_eq!(local_project(&folder).unwrap().name, "store");
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(folder_name(Path::new("/srv/_api")), "api");
        assert_eq!(declared_name("name: ${APP:-shop}"), None);
    }

    #[test]
    fn a_failure_ends_with_stderr_and_is_never_empty() {
        let command = |script: &str| {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            command
        };
        assert!(run(command("echo out; echo err >&2; exit 1")).unwrap_err().ends_with("err"));
        assert_eq!(run(command("exit 3")).unwrap_err(), "sh exited with exit status: 3.");
        assert_eq!(run(command("true")), Ok(()));
    }
}
