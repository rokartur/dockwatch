//! Apple's `container` runtime, driven through its CLI: it has no Docker socket, only XPC.
//! Values come back in the Docker shapes the rest of the app reads.

use crate::compose;
use crate::docker::{Container, Engine, Image, Mount, Network, Snapshot, Volume, array, label, text};
use crate::http::{Error, Result, Stream};
use crate::logs::LineSplitter;
use serde_json::{Value, json};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// Stands in for a socket path in `Runtime` and `Docker`, so settings and selection keep working.
pub const SOCKET: &str = "apple://container";

const TIMEOUT: Duration = Duration::from_secs(120);
const LONG_TIMEOUT: Duration = Duration::from_secs(3600);
/// The CLI has no event stream; the app refreshes this often and keeps a snapshot that did not change.
const POLL_EVERY: Duration = Duration::from_secs(2);
const ANONYMOUS: &str = "com.apple.container.resource.anonymous";

pub fn snapshot() -> Result<Snapshot> {
    let (containers, images, networks, volumes) = thread::scope(|scope| {
        let containers = scope.spawn(|| json(&["list", "--all", "--format", "json"]));
        let images = scope.spawn(|| json(&["image", "list", "--format", "json"]));
        let networks = scope.spawn(|| json(&["network", "list", "--format", "json"]));
        let volumes = json(&["volume", "list", "--format", "json"]);
        (containers.join().unwrap(), images.join().unwrap(), networks.join().unwrap(), volumes)
    });
    let mut snapshot = Snapshot {
        containers: array(&containers?).iter().map(container_from_list).collect(),
        images: array(&images?).iter().map(image_from_list).collect(),
        networks: array(&networks?).iter().map(network_from_list).collect(),
        volumes: array(&volumes?).iter().map(volume_from_list).collect(),
    };
    snapshot.containers.sort_by(|a, b| a.name.cmp(&b.name));
    snapshot.images.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.reference.cmp(&b.reference)));
    snapshot.networks.sort_by(|a, b| a.name.cmp(&b.name));
    snapshot.volumes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(snapshot)
}

pub fn engine() -> Result<Engine> {
    let status = json(&["system", "status", "--format", "json"])?;
    Ok(Engine {
        version: text(&status["server"]["version"]),
        api_version: "-".into(),
        cpus: status["host"]["cpus"].as_u64().unwrap_or(0),
        memory: 0,
        system: text(&status["host"]["operatingSystem"]).replace("Version", "macOS"),
        kernel: "-".into(),
        architecture: text(&status["host"]["architecture"]),
        name: "Apple container".into(),
    })
}

pub fn ping() -> Result<()> {
    let status = run_json(&["system", "status", "--format", "json"], Duration::from_secs(5))?;
    if status["status"] != "running" {
        return Err(Error::new("Stopped. Start it with `container system start`."));
    }
    Ok(())
}

/// `action` is `start`, `stop`, `restart`, `pause` or `unpause`; Apple's runtime cannot pause.
pub fn container_action(id: &str, action: &str) -> Result<()> {
    match action {
        "start" | "stop" => run(&[action, id], LONG_TIMEOUT).map(drop),
        "restart" => run(&["stop", id], LONG_TIMEOUT).and_then(|_| run(&["start", id], LONG_TIMEOUT)).map(drop),
        _ => Err(Error::new("Apple container cannot pause containers.")),
    }
}

pub fn remove_container(id: &str, force: bool) -> Result<()> {
    let args: &[&str] = if force { &["delete", "--force", id] } else { &["delete", id] };
    run(args, LONG_TIMEOUT).map(drop)
}

pub fn remove(kind: &str, name: &str) -> Result<()> {
    run(&[kind, "delete", name], TIMEOUT).map(drop)
}

/// Like `docker image prune -a`. Returns bytes reclaimed, from `system df` before and after.
pub fn prune_images() -> Result<u64> {
    let before = disk_usage("images")?;
    run(&["image", "prune", "--all"], LONG_TIMEOUT)?;
    Ok(before.saturating_sub(disk_usage("images")?))
}

/// Returns how many networks were removed.
pub fn prune_networks() -> Result<usize> {
    let count = || json(&["network", "list", "--format", "json"]).map(|networks| array(&networks).len());
    let before = count()?;
    run(&["network", "prune"], LONG_TIMEOUT)?;
    Ok(before.saturating_sub(count()?))
}

/// Anonymous volumes no container uses. `container volume prune` would remove named ones too.
pub fn prune_volumes() -> Result<u64> {
    let snapshot = snapshot()?;
    let before = disk_usage("volumes")?;
    for volume in snapshot.volumes.iter().filter(|volume| volume.anonymous) {
        if !snapshot.containers.iter().any(|container| container.volumes.iter().any(|mount| mount.name == volume.name)) {
            remove("volume", &volume.name)?;
        }
    }
    Ok(before.saturating_sub(disk_usage("volumes")?))
}

/// `kind` is empty for a container, else `image`, `network` or `volume`.
pub fn inspect(kind: &str, name: &str) -> Result<Value> {
    let args: Vec<&str> = [kind, "inspect", name].into_iter().filter(|arg| !arg.is_empty()).collect();
    Ok(json(&args)?[0].clone())
}

/// The image config's history, in the shape of Docker's history endpoint. Layer sizes are unknown.
pub fn image_history(reference: &str) -> Result<Value> {
    Ok(history(&inspect("image", reference)?))
}

/// `ps` in the container, in the shape of Docker's top endpoint.
pub fn top(id: &str) -> Result<Value> {
    let (output, errors) = exec(id, &["ps", "-o", "pid,user,time,args"])?;
    if output.is_empty() {
        return Err(Error::new(String::from_utf8_lossy(&errors).trim()));
    }
    Ok(processes(&String::from_utf8_lossy(&output)))
}

/// Same contract as `Docker::exec`: output of any exit status, an error when the command is missing.
pub fn exec(id: &str, command: &[&str]) -> Result<(Vec<u8>, Vec<u8>)> {
    let output = output(&[&["exec", id], command].concat(), TIMEOUT)?;
    let errors = String::from_utf8_lossy(&output.stderr);
    if errors.contains("failed to find target executable") {
        return Err(Error::new(format!("{} cannot run here: not in the image.", command[0])));
    }
    if errors.contains("is not running") {
        return Err(Error { message: errors.trim().trim_start_matches("Error: ").to_string(), status: 409 });
    }
    Ok((output.stdout, output.stderr))
}

/// A ping, then `on_event`, every two seconds; `on_end` gets the ping that failed.
pub fn events(mut on_event: impl FnMut() + Send + 'static, on_end: impl FnOnce(Option<Error>) + Send + 'static) -> Stream {
    let tick = move || {
        ping()?;
        on_event();
        Ok(())
    };
    Stream::poll(POLL_EVERY, tick, |error| on_end(Some(error)))
}

/// Last `tail` lines, then follows. The CLI sends no timestamps, so each line gets an empty one.
pub fn logs(id: &str, tail: usize, mut on_data: impl FnMut(&[u8], &str) + Send + 'static, on_end: impl FnOnce(Option<Error>) + Send + 'static) -> Stream {
    let mut command = Command::new(compose::find_binary("container").unwrap_or("container".into()));
    command.args(["logs", "--follow", "-n", &tail.to_string(), id]);
    let mut lines = LineSplitter::default();
    let on_chunk = move |chunk: &[u8]| {
        for line in lines.feed(chunk) {
            on_data(format!(" {line}\n").as_bytes(), "text/plain");
        }
    };
    Stream::process(command, on_chunk, on_end)
}

/// One Docker-shaped reading a second, as JSON lines.
pub fn stats(id: &str, mut on_data: impl FnMut(&[u8], &str) + Send + 'static, on_end: impl FnOnce(Option<Error>) + Send + 'static) -> Stream {
    let (id, started) = (id.to_string(), Instant::now());
    let mut previous = None;
    let tick = move || {
        let reading = json(&["stats", "--no-stream", "--format", "json", &id])?;
        let (cpu, wall) = (reading[0]["cpuUsageUsec"].as_u64().unwrap_or(0) * 1000, started.elapsed().as_nanos() as u64);
        on_data(format!("{}\n", docker_stats(&reading[0], (cpu, wall), previous)).as_bytes(), "application/json");
        previous = Some((cpu, wall));
        Ok(())
    };
    Stream::poll(Duration::from_secs(1), tick, |error| on_end(Some(error)))
}

fn json(args: &[&str]) -> Result<Value> {
    run_json(args, TIMEOUT)
}

fn run_json(args: &[&str], timeout: Duration) -> Result<Value> {
    let stdout = run(args, timeout)?;
    serde_json::from_slice(&stdout).map_err(|error| Error::new(format!("container sent JSON this app cannot read: {error}")))
}

/// Stdout of a command that succeeded; a failure carries the CLI's error line.
fn run(args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let output = output(args, timeout)?;
    if !output.status.success() {
        let errors = String::from_utf8_lossy(&output.stderr);
        let message = errors.lines().rfind(|line| !line.trim().is_empty()).unwrap_or("container failed.");
        return Err(Error::new(message.trim().trim_start_matches("Error: ")));
    }
    Ok(output.stdout)
}

fn output(args: &[&str], timeout: Duration) -> Result<std::process::Output> {
    let mut command = compose::cli(SOCKET)?;
    command.args(args);
    Ok(compose::output(command, timeout)?)
}

fn disk_usage(kind: &str) -> Result<u64> {
    Ok(json(&["system", "df", "--format", "json"])?[kind]["sizeInBytes"].as_u64().unwrap_or(0))
}

fn container_from_list(entry: &Value) -> Container {
    let config = &entry["configuration"];
    let labels = &config["labels"];
    let summary = json!({ "Labels": labels });
    let mut ports: Vec<(u64, u64)> = array(&config["publishedPorts"]).iter().filter_map(|port| Some((port["hostPort"].as_u64()?, port["containerPort"].as_u64()?))).collect();
    ports.sort();
    let volumes = array(&config["mounts"]).iter().filter_map(|mount| Some(Mount { name: mount["type"]["volume"]["name"].as_str()?.to_string(), destination: text(&mount["destination"]) })).collect();
    let state = match entry["status"]["state"].as_str() {
        Some("stopped") => "exited".to_string(),
        state => state.unwrap_or_default().to_string(),
    };
    let status = match entry["status"]["startedDate"].as_str() {
        Some(started) if state == "running" => format!("Up since {}", started.get(..16).unwrap_or(started).replace('T', " ")),
        _ => capitalize(entry["status"]["state"].as_str().unwrap_or_default()),
    };
    let depends_on = label(&summary, "com.docker.compose.depends_on").unwrap_or_default();
    let config_files = label(&summary, "com.docker.compose.project.config_files").unwrap_or_default();
    Container {
        id: text(&entry["id"]),
        name: text(&entry["id"]),
        image: text(&config["image"]["reference"]),
        image_id: text(&config["image"]["descriptor"]["digest"]),
        ports: ports.iter().map(|(host, container)| format!("{host}:{container}")).collect::<Vec<_>>().join(", "),
        networks: array(&config["networks"]).iter().map(|network| text(&network["network"])).collect(),
        volumes,
        status,
        state,
        project: label(&summary, "com.docker.compose.project"),
        service: label(&summary, "com.docker.compose.service"),
        depends_on: depends_on.split(',').filter_map(|entry| entry.split(':').next()).filter(|name| !name.is_empty()).map(String::from).collect(),
        working_dir: label(&summary, "com.docker.compose.project.working_dir"),
        config_files: config_files.split(',').filter(|path| !path.is_empty()).map(String::from).collect(),
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// One row per image, sized like `docker images`: the platform this Mac runs, not every platform in the index.
fn image_from_list(entry: &Value) -> Image {
    Image {
        reference: text(&entry["configuration"]["name"]),
        id: text(&entry["configuration"]["descriptor"]["digest"]),
        size: host_variant(entry).and_then(|variant| variant["size"].as_u64()).unwrap_or(0),
    }
}

/// The arm64 variant Apple silicon runs, else the first real platform; attestation variants report `unknown`.
fn host_variant(image: &Value) -> Option<&Value> {
    let variants = array(&image["variants"]);
    let arm64 = variants.iter().find(|variant| variant["platform"]["architecture"] == "arm64");
    arm64.or_else(|| variants.iter().find(|variant| variant["platform"]["architecture"] != "unknown"))
}

fn network_from_list(entry: &Value) -> Network {
    let config = &entry["configuration"];
    Network {
        id: text(&entry["id"]),
        name: text(&config["name"]),
        driver: text(&config["mode"]),
        subnet: text(&entry["status"]["ipv4Subnet"]),
        project: config["labels"]["com.docker.compose.project"].as_str().map(String::from),
        system: config["labels"]["com.apple.container.resource.role"] == "builtin",
    }
}

fn volume_from_list(entry: &Value) -> Volume {
    let config = &entry["configuration"];
    Volume { name: text(&config["name"]), driver: text(&config["driver"]), created_at: text(&config["creationDate"]), anonymous: config["labels"].get(ANONYMOUS).is_some() }
}

fn history(inspect: &Value) -> Value {
    let Some(variant) = host_variant(inspect) else { return json!([]) };
    let mut layers: Vec<Value> =
        array(&variant["config"]["history"]).iter().map(|layer| json!({"Created": epoch(layer["created"].as_str().unwrap_or_default()), "CreatedBy": layer["created_by"], "Size": 0})).collect();
    layers.reverse();
    Value::Array(layers)
}

/// Seconds since 1970 of an RFC 3339 UTC time; 0 when it does not parse.
fn epoch(time: &str) -> i64 {
    let number = |range: std::ops::Range<usize>| time.get(range).and_then(|digits| digits.parse::<i64>().ok());
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (number(0..4), number(5..7), number(8..10), number(11..13), number(14..16), number(17..19)) else { return 0 };
    // Howard Hinnant's days_from_civil.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    days * 86_400 + hour * 3600 + minute * 60 + second
}

/// `ps` columns, with the last one (the command) keeping its spaces.
fn processes(ps: &str) -> Value {
    let mut lines = ps.lines().filter(|line| !line.trim().is_empty());
    let titles: Vec<&str> = lines.next().unwrap_or_default().split_whitespace().collect();
    let rows: Vec<Vec<String>> = lines
        .map(|line| {
            let mut cells: Vec<String> = Vec::new();
            let mut rest = line.trim_start();
            while cells.len() + 1 < titles.len() {
                let (cell, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                cells.push(cell.to_string());
                rest = tail.trim_start();
            }
            cells.push(rest.to_string());
            cells
        })
        .collect();
    json!({"Titles": titles, "Processes": rows})
}

/// CPU over wall time with one online CPU: 100% is one core, as in `docker stats`.
fn docker_stats(reading: &Value, (cpu, wall): (u64, u64), previous: Option<(u64, u64)>) -> Value {
    let number = |key: &str| reading[key].as_u64().unwrap_or(0);
    let precpu = match previous {
        Some((cpu, wall)) => json!({"cpu_usage": {"total_usage": cpu}, "system_cpu_usage": wall}),
        None => json!({}),
    };
    json!({
        "cpu_stats": {"cpu_usage": {"total_usage": cpu}, "system_cpu_usage": wall, "online_cpus": 1},
        "precpu_stats": precpu,
        "memory_stats": {"usage": number("memoryUsageBytes"), "limit": number("memoryLimitBytes")},
        "networks": {"eth0": {"rx_bytes": number("networkRxBytes"), "tx_bytes": number("networkTxBytes")}},
        "blkio_stats": {"io_service_bytes_recursive": [{"op": "read", "value": number("blockReadBytes")}, {"op": "write", "value": number("blockWriteBytes")}]},
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::Sample;

    #[test]
    fn reads_a_container_from_the_list() {
        let entry = json!({
            "id": "web",
            "configuration": {
                "image": {"reference": "docker.io/library/nginx:alpine", "descriptor": {"digest": "sha256:713f"}},
                "labels": {"com.docker.compose.project": "shop", "com.docker.compose.depends_on": "db:service_started:false"},
                "mounts": [
                    {"destination": "/data", "type": {"volume": {"name": "dwdata"}}},
                    {"destination": "/src", "type": {"virtiofs": {}}}
                ],
                "networks": [{"network": "dwnet"}],
                "publishedPorts": [{"containerPort": 80, "hostPort": 8080}]
            },
            "status": {"state": "running", "startedDate": "2026-11-06T11:12:07Z"}
        });
        let container = container_from_list(&entry);
        assert_eq!((container.name.as_str(), container.image_id.as_str(), container.ports.as_str()), ("web", "sha256:713f", "8080:80"));
        assert_eq!(container.volumes, [Mount { name: "dwdata".into(), destination: "/data".into() }]);
        assert_eq!(container.status, "Up since 2026-11-06 11:12");
        assert!(container.running());
        assert_eq!((container.networks, container.project, container.depends_on), (vec!["dwnet".to_string()], Some("shop".into()), vec!["db".to_string()]));

        let stopped = container_from_list(&json!({"id": "made", "configuration": {}, "status": {"state": "stopped"}}));
        assert_eq!((stopped.state.as_str(), stopped.status.as_str()), ("exited", "Stopped"));
    }

    #[test]
    fn reads_images_networks_and_volumes() {
        let image = image_from_list(&json!({"configuration": {"name": "alpine:latest", "descriptor": {"digest": "sha256:3f4d"}}, "variants": [
            {"platform": {"architecture": "amd64"}, "size": 3},
            {"platform": {"architecture": "unknown"}, "size": 1},
            {"platform": {"architecture": "arm64"}, "size": 4}
        ]}));
        assert_eq!(image, Image { reference: "alpine:latest".into(), id: "sha256:3f4d".into(), size: 4 });
        let amd64_only = json!({"variants": [{"platform": {"architecture": "unknown"}, "size": 1}, {"platform": {"architecture": "amd64"}, "size": 3}]});
        assert_eq!(image_from_list(&amd64_only).size, 3);

        let builtin =
            json!({"id": "default", "configuration": {"name": "default", "mode": "nat", "labels": {"com.apple.container.resource.role": "builtin"}}, "status": {"ipv4Subnet": "192.168.64.0/24"}});
        let network = network_from_list(&builtin);
        assert!(network.system);
        assert_eq!(network.subnet, "192.168.64.0/24");
        assert!(!network_from_list(&json!({"id": "dwnet", "configuration": {"name": "dwnet", "labels": {}}})).system);

        assert!(volume_from_list(&json!({"configuration": {"name": "763d", "labels": {ANONYMOUS: ""}}})).anonymous);
        assert!(!volume_from_list(&json!({"configuration": {"name": "dwdata", "labels": {}}})).anonymous);
    }

    #[test]
    fn converts_history_newest_first() {
        let inspect = json!({"variants": [
            {"platform": {"architecture": "unknown"}, "config": {}},
            {"platform": {"architecture": "arm64"}, "config": {"history": [
                {"created": "1970-01-02T00:00:01Z", "created_by": "ADD rootfs /"},
                {"created": "2026-11-05T18:00:31.5Z", "created_by": "CMD [\"sh\"]"}
            ]}}
        ]});
        let layers = history(&inspect);
        assert_eq!(layers[0]["CreatedBy"], "CMD [\"sh\"]");
        assert_eq!(layers[1]["Created"], 86_401);
        assert_eq!(epoch("2000-03-01T00:00:00Z"), 951_868_800);
        assert_eq!(epoch("garbage"), 0);
    }

    #[test]
    fn splits_ps_into_columns() {
        let top = processes("PID   USER     TIME  COMMAND\n    1 root      0:00 nginx: master process nginx -g daemon off;\n");
        assert_eq!(top["Titles"], json!(["PID", "USER", "TIME", "COMMAND"]));
        assert_eq!(top["Processes"][0], json!(["1", "root", "0:00", "nginx: master process nginx -g daemon off;"]));
    }

    #[test]
    fn reports_cpu_as_a_share_of_one_core() {
        let reading = json!({"cpuUsageUsec": 0, "memoryUsageBytes": 10, "memoryLimitBytes": 100, "networkRxBytes": 1, "networkTxBytes": 2, "blockReadBytes": 3, "blockWriteBytes": 4});
        assert_eq!(Sample::from_stats(&docker_stats(&reading, (0, 0), None)).cpu_percent, None);
        let sample = Sample::from_stats(&docker_stats(&reading, (1_500_000_000, 2_000_000_000), Some((500_000_000, 1_000_000_000))));
        assert_eq!(sample.cpu_percent, Some(100.0));
        assert_eq!((sample.memory_used, sample.memory_limit, sample.net_rx, sample.net_tx, sample.block_read, sample.block_write), (10, 100, 1, 2, 3, 4));
    }
}
