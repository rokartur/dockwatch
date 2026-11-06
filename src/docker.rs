use crate::files::{self, FileEntry};
use crate::http::{Connection, Error, Head, Result, Stream};
use serde_json::{Value, json};
use std::thread;
use std::time::Duration;

/// Stop waits out the container's grace period and prune walks every object before Docker answers.
const LONG_REQUEST: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub image_id: String,
    pub ports: String,
    pub networks: Vec<String>,
    pub volumes: Vec<Mount>,
    /// `Up 3 minutes (healthy)`, as Docker words it.
    pub status: String,
    /// `running`, `paused`, `exited`, `created`, `restarting`, `removing` or `dead`.
    pub state: String,
    pub project: Option<String>,
    pub service: Option<String>,
    /// Services this one waits for, from compose's `com.docker.compose.depends_on` label.
    pub depends_on: Vec<String>,
    pub working_dir: Option<String>,
    pub config_files: Vec<String>,
}

impl Container {
    pub fn running(&self) -> bool {
        self.state == "running"
    }

    /// Running, paused or restarting: what a stop acts on.
    pub fn up(&self) -> bool {
        matches!(self.state.as_str(), "running" | "paused" | "restarting")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    /// `name:tag`, or the image ID when untagged; one image with two tags gives two rows.
    pub reference: String,
    pub id: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub subnet: String,
    pub project: Option<String>,
}

impl Network {
    /// Docker creates these itself and refuses to remove them.
    pub fn is_system(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none")
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    /// RFC 3339, as Docker sends it.
    pub created_at: String,
    /// Created by a `VOLUME` in the image or an unnamed `-v /path`; its name is a random hash.
    pub anonymous: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mount {
    pub name: String,
    /// Path inside the container.
    pub destination: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub networks: Vec<Network>,
    pub volumes: Vec<Volume>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Engine {
    pub version: String,
    pub api_version: String,
    pub cpus: u64,
    pub memory: u64,
    pub system: String,
    pub kernel: String,
    pub architecture: String,
    pub name: String,
}

/// Docker Engine API client over a Unix socket. Paths carry no API version, so the daemon
/// answers in its own latest one.
#[derive(Clone, Debug)]
pub struct Docker {
    pub socket: String,
}

impl Docker {
    pub fn snapshot(&self) -> Result<Snapshot> {
        let (containers, images, networks, volumes) = thread::scope(|scope| {
            let containers = scope.spawn(|| self.get("/containers/json?all=1"));
            let images = scope.spawn(|| self.get("/images/json"));
            let networks = scope.spawn(|| self.get("/networks"));
            let volumes = self.get("/volumes");
            (containers.join().unwrap(), images.join().unwrap(), networks.join().unwrap(), volumes)
        });
        let mut snapshot = Snapshot {
            containers: array(&containers?).iter().map(container_from_summary).collect(),
            images: array(&images?).iter().flat_map(images_from_summary).collect(),
            networks: array(&networks?).iter().map(network_from_summary).collect(),
            volumes: array(&volumes?["Volumes"]).iter().map(volume_from_summary).collect(),
        };
        // The daemon lists networks and volumes in map order, which changes per call.
        snapshot.containers.sort_by(|a, b| a.name.cmp(&b.name));
        snapshot.images.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.reference.cmp(&b.reference)));
        snapshot.networks.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(snapshot)
    }

    pub fn engine(&self) -> Result<Engine> {
        let info = self.get("/info")?;
        let version = self.get("/version")?;
        Ok(Engine {
            version: text(&info["ServerVersion"]),
            api_version: text(&version["ApiVersion"]),
            cpus: info["NCPU"].as_u64().unwrap_or(0),
            memory: info["MemTotal"].as_u64().unwrap_or(0),
            system: text(&info["OperatingSystem"]),
            kernel: text(&info["KernelVersion"]),
            architecture: text(&info["Architecture"]),
            name: text(&info["Name"]),
        })
    }

    pub fn ping(&self) -> Result<()> {
        let body = self.request_with_timeout("GET", "/_ping", None, Duration::from_secs(2))?;
        if body != b"OK" {
            return Err(Error::new("The socket answered, but not as Docker."));
        }
        Ok(())
    }

    /// `action` is `start`, `stop`, `restart`, `pause` or `unpause`.
    pub fn container_action(&self, id: &str, action: &str) -> Result<()> {
        self.request_with_timeout("POST", &format!("/containers/{id}/{action}"), None, LONG_REQUEST).map(drop)
    }

    /// Volumes always stay: `v=0`.
    pub fn remove_container(&self, id: &str, force: bool) -> Result<()> {
        self.request("DELETE", &format!("/containers/{id}?force={}&v=0", force as u8), None).map(drop)
    }

    /// Removing by `name:tag` untags; the image goes once its last tag does.
    pub fn remove_image(&self, reference: &str) -> Result<()> {
        self.request("DELETE", &format!("/images/{}", escape(reference)), None).map(drop)
    }

    pub fn remove_network(&self, id: &str) -> Result<()> {
        self.request("DELETE", &format!("/networks/{id}"), None).map(drop)
    }

    pub fn remove_volume(&self, name: &str) -> Result<()> {
        self.request("DELETE", &format!("/volumes/{}", escape(name)), None).map(drop)
    }

    /// Removes every image no container uses, like `docker image prune -a`. Returns bytes reclaimed.
    pub fn prune_images(&self) -> Result<u64> {
        let filters = escape(r#"{"dangling":["false"]}"#);
        let body = self.request_with_timeout("POST", &format!("/images/prune?filters={filters}"), None, LONG_REQUEST)?;
        Ok(parse(&body)?["SpaceReclaimed"].as_u64().unwrap_or(0))
    }

    /// Returns how many networks were removed.
    pub fn prune_networks(&self) -> Result<usize> {
        Ok(array(&self.post_json("/networks/prune")?["NetworksDeleted"]).len())
    }

    /// Anonymous volumes only, like `docker volume prune` without `-a`. Returns bytes reclaimed.
    /// Before API 1.42 an unfiltered prune also removed named volumes; those daemons reject `all`.
    pub fn prune_volumes(&self) -> Result<u64> {
        let filters = escape(r#"{"all":["false"]}"#);
        let body = self.request_with_timeout("POST", &format!("/volumes/prune?filters={filters}"), None, LONG_REQUEST)?;
        Ok(parse(&body)?["SpaceReclaimed"].as_u64().unwrap_or(0))
    }

    pub fn inspect_container(&self, id: &str) -> Result<Value> {
        self.get(&format!("/containers/{id}/json"))
    }

    pub fn inspect_image(&self, reference: &str) -> Result<Value> {
        self.get(&format!("/images/{}/json", escape(reference)))
    }

    pub fn image_history(&self, id: &str) -> Result<Value> {
        self.get(&format!("/images/{}/history", escape(id)))
    }

    pub fn inspect_network(&self, id: &str) -> Result<Value> {
        self.get(&format!("/networks/{id}"))
    }

    pub fn inspect_volume(&self, name: &str) -> Result<Value> {
        self.get(&format!("/volumes/{}", escape(name)))
    }

    pub fn top(&self, id: &str) -> Result<Value> {
        self.get(&format!("/containers/{id}/top"))
    }

    /// Runs `command` without a TTY and waits for it to exit. No shell: arguments pass as they are.
    /// A command missing from the image fails with 126 or 127, its message in the output.
    pub fn exec(&self, id: &str, command: &[&str]) -> Result<(Vec<u8>, Vec<u8>)> {
        let create = json!({"AttachStdout": true, "AttachStderr": true, "Cmd": command}).to_string();
        let created = self.request("POST", &format!("/containers/{id}/exec"), Some(("application/json", create.as_bytes())))?;
        let exec_id = text(&parse(&created)?["Id"]);
        let start = json!({"Detach": false, "Tty": false}).to_string();
        let (output, errors) = files::demultiplex(&self.request("POST", &format!("/exec/{exec_id}/start"), Some(("application/json", start.as_bytes())))?);
        if let Some(126 | 127) = self.get(&format!("/exec/{exec_id}/json"))?["ExitCode"].as_i64() {
            let message = format!("{}{}", String::from_utf8_lossy(&output), String::from_utf8_lossy(&errors));
            return Err(Error::new(format!("{} cannot run here: {}", command[0], message.trim())));
        }
        Ok((output, errors))
    }

    /// Entries of one folder in a running container, through `find` and `stat` in the image.
    pub fn list_files(&self, id: &str, folder: &str) -> Result<Vec<FileEntry>> {
        // `-L` describes what a link points to, so a linked folder opens.
        let list = |follow: &[&str]| {
            let command = [&["find", folder, "-mindepth", "1", "-maxdepth", "1", "-exec", "stat"], follow, &["-c", "%F\t%s\t%Y\t%A\t%n", "{}", "+"]].concat();
            self.exec(id, &command).map_err(|error| if error.status == 409 { Error { message: "Start the container to browse its files.".into(), status: 409 } } else { error })
        };
        let (output, errors) = list(&["-L"])?;
        if errors.is_empty() {
            return Ok(files::parse_listing(&String::from_utf8_lossy(&output)));
        }
        if output.is_empty() {
            return Err(Error::new(String::from_utf8_lossy(&errors).trim()));
        }
        // Broken links fail `stat -L` alone; list them as the links they are.
        let mut entries = files::parse_listing(&String::from_utf8_lossy(&output));
        let (links, _) = list(&[])?;
        for link in files::parse_listing(&String::from_utf8_lossy(&links)) {
            if !entries.iter().any(|entry| entry.path == link.path) {
                entries.push(link);
            }
        }
        files::sort_entries(&mut entries);
        Ok(entries)
    }

    /// The first `limit` bytes, through `head` in the container, which follows links; the archive
    /// endpoint would return the link itself.
    pub fn read_file(&self, id: &str, path: &str, limit: u64) -> Result<Vec<u8>> {
        let (output, errors) = self.exec(id, &["head", "-c", &limit.to_string(), path])?;
        if output.is_empty() && !errors.is_empty() {
            return Err(Error::new(String::from_utf8_lossy(&errors).trim()));
        }
        Ok(output)
    }

    /// Container, image, network and volume events, except the exec ones every healthcheck run
    /// sends; `on_end` runs once the runtime goes away.
    pub fn events(&self, mut on_event: impl FnMut() + Send + 'static, on_end: impl FnOnce(Option<Error>) + Send + 'static) -> Stream {
        let filters = escape(r#"{"type":["container","image","network","volume"]}"#);
        let on_data = move |chunk: &[u8], _: &str| {
            if !only_exec_events(chunk) {
                on_event();
            }
        };
        self.stream(&format!("/events?filters={filters}"), on_data, on_end)
    }

    /// Last `tail` lines, then follows. Each line starts with Docker's timestamp.
    pub fn logs(&self, id: &str, tail: usize, on_data: impl FnMut(&[u8], &str) + Send + 'static, on_end: impl FnOnce(Option<Error>) + Send + 'static) -> Stream {
        self.stream(&format!("/containers/{id}/logs?follow=1&stdout=1&stderr=1&timestamps=1&tail={tail}"), on_data, on_end)
    }

    /// One JSON reading per line, about one per second.
    pub fn stats(&self, id: &str, on_data: impl FnMut(&[u8], &str) + Send + 'static, on_end: impl FnOnce(Option<Error>) + Send + 'static) -> Stream {
        self.stream(&format!("/containers/{id}/stats?stream=1"), on_data, on_end)
    }

    /// `on_end` gets the error when connecting, the request or the body fails.
    fn stream(&self, path: &str, mut on_data: impl FnMut(&[u8], &str) + Send + 'static, on_end: impl FnOnce(Option<Error>) + Send + 'static) -> Stream {
        let path = path.to_string();
        Stream::spawn(&self.socket, move |connection| {
            let result = (|| {
                let mut connection = connection?;
                connection.send("GET", &path, None)?;
                let head = connection.read_head()?;
                if head.status >= 400 {
                    return Err(api_error(&head, read_all(&mut connection, &head)?));
                }
                connection.read_body(&head, |chunk| {
                    on_data(chunk, &head.content_type);
                    Ok(())
                })
            })();
            on_end(result.err());
        })
    }

    fn get(&self, path: &str) -> Result<Value> {
        parse(&self.request("GET", path, None)?)
    }

    fn post_json(&self, path: &str) -> Result<Value> {
        parse(&self.request("POST", path, None)?)
    }

    /// Stop, restart, prune and archive calls answer only when done and use `LONG_REQUEST` instead.
    fn request(&self, method: &str, path: &str, body: Option<(&str, &[u8])>) -> Result<Vec<u8>> {
        self.request_with_timeout(method, path, body, Duration::from_secs(120))
    }

    fn request_with_timeout(&self, method: &str, path: &str, body: Option<(&str, &[u8])>, timeout: Duration) -> Result<Vec<u8>> {
        let mut connection = Connection::open(&self.socket, Some(timeout))?;
        connection.send(method, path, body)?;
        let head = connection.read_head()?;
        let body = read_all(&mut connection, &head)?;
        if head.status >= 400 {
            return Err(api_error(&head, body));
        }
        Ok(body)
    }
}

fn read_all(connection: &mut Connection, head: &Head) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    connection.read_body(head, |chunk| {
        body.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(body)
}

fn api_error(head: &Head, body: Vec<u8>) -> Error {
    let message = serde_json::from_slice::<Value>(&body).ok().and_then(|value| value["message"].as_str().map(String::from));
    Error { message: message.unwrap_or_else(|| format!("Docker answered HTTP {}", head.status)), status: head.status }
}

fn parse(body: &[u8]) -> Result<Value> {
    serde_json::from_slice(body).map_err(|error| Error::new(format!("Docker sent JSON this app cannot read: {error}")))
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn label(summary: &Value, name: &str) -> Option<String> {
    summary["Labels"][name].as_str().map(String::from)
}

/// True when every line of an events chunk is an `exec_*` action; unreadable chunks count as events.
fn only_exec_events(chunk: &[u8]) -> bool {
    let mut events = chunk.split(|&byte| byte == b'\n').filter(|line| !line.trim_ascii().is_empty()).peekable();
    events.peek().is_some() && events.all(|line| serde_json::from_slice::<Value>(line).is_ok_and(|event| event["Action"].as_str().is_some_and(|action| action.starts_with("exec_"))))
}

/// Percent-encodes all but unreserved characters and the `/`, `:` and `@` of image references.
fn escape(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:@".contains(&byte) {
            out.push(byte as char);
        } else {
            out += &format!("%{byte:02X}");
        }
    }
    out
}

/// `4f1d2c3b5a69` out of `sha256:4f1d2c3b5a69...`, as the docker CLI shows IDs.
pub fn short_id(id: &str) -> &str {
    let id = id.strip_prefix("sha256:").unwrap_or(id);
    &id[..id.len().min(12)]
}

/// `8080:80, 5432:5432`, published ports only; IPv4 and IPv6 bindings of one port appear once.
fn published_ports(ports: &[Value]) -> String {
    let mut bound: Vec<(u64, u64)> = ports.iter().filter_map(|port| Some((port["PublicPort"].as_u64()?, port["PrivatePort"].as_u64()?))).collect();
    bound.sort();
    bound.dedup();
    bound.iter().map(|(public, private)| format!("{public}:{private}")).collect::<Vec<_>>().join(", ")
}

fn container_from_summary(summary: &Value) -> Container {
    let name = summary["Names"][0].as_str().unwrap_or_default().trim_start_matches('/').to_string();
    let networks = summary["NetworkSettings"]["Networks"].as_object().map(|networks| networks.keys().cloned().collect()).unwrap_or_default();
    let volumes = array(&summary["Mounts"]).iter().filter(|mount| mount["Type"] == "volume").map(|mount| Mount { name: text(&mount["Name"]), destination: text(&mount["Destination"]) }).collect();
    // "db:service_started:false,redis:service_healthy:true"
    let depends_on = label(summary, "com.docker.compose.depends_on").unwrap_or_default();
    let config_files = label(summary, "com.docker.compose.project.config_files").unwrap_or_default();
    Container {
        id: text(&summary["Id"]),
        name,
        image: text(&summary["Image"]),
        image_id: text(&summary["ImageID"]),
        ports: published_ports(array(&summary["Ports"])),
        networks,
        volumes,
        status: text(&summary["Status"]),
        state: text(&summary["State"]),
        project: label(summary, "com.docker.compose.project"),
        service: label(summary, "com.docker.compose.service"),
        depends_on: depends_on.split(',').filter_map(|entry| entry.split(':').next()).filter(|name| !name.is_empty()).map(String::from).collect(),
        working_dir: label(summary, "com.docker.compose.project.working_dir"),
        config_files: config_files.split(',').filter(|path| !path.is_empty()).map(String::from).collect(),
    }
}

fn images_from_summary(summary: &Value) -> Vec<Image> {
    let image = |reference: &str| Image { reference: reference.to_string(), id: text(&summary["Id"]), size: summary["Size"].as_u64().unwrap_or(0) };
    let mut images: Vec<_> = array(&summary["RepoTags"]).iter().filter_map(Value::as_str).filter(|&reference| reference != "<none>:<none>").map(image).collect();
    if images.is_empty() {
        images.push(image(&text(&summary["Id"])));
    }
    images
}

fn network_from_summary(summary: &Value) -> Network {
    Network {
        id: text(&summary["Id"]),
        name: text(&summary["Name"]),
        driver: text(&summary["Driver"]),
        subnet: text(&summary["IPAM"]["Config"][0]["Subnet"]),
        project: label(summary, "com.docker.compose.project"),
    }
}

fn volume_from_summary(summary: &Value) -> Volume {
    let name = text(&summary["Name"]);
    Volume {
        driver: text(&summary["Driver"]),
        created_at: text(&summary["CreatedAt"]),
        // Engines before 23 do not set the label; their anonymous volumes are 64 hex digits.
        anonymous: label(summary, "com.docker.volume.anonymous").is_some() || is_hash(&name),
        name,
    }
}

fn is_hash(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_compose_container() {
        let summary = json!({
            "Id": "abc", "Names": ["/shop-web-1"], "Image": "nginx", "ImageID": "sha256:1", "State": "running", "Status": "Up 3 minutes",
            "Ports": [{"PrivatePort": 80, "PublicPort": 8080}, {"PrivatePort": 80, "PublicPort": 8080}, {"PrivatePort": 443}],
            "NetworkSettings": {"Networks": {"shop_default": {}}},
            "Mounts": [{"Type": "volume", "Name": "shop_data", "Destination": "/data"}, {"Type": "bind", "Source": "/tmp"}],
            "Labels": {
                "com.docker.compose.project": "shop", "com.docker.compose.service": "web",
                "com.docker.compose.depends_on": "db:service_started:false,redis:service_healthy:true",
                "com.docker.compose.project.working_dir": "/srv/shop",
                "com.docker.compose.project.config_files": "/srv/shop/compose.yaml,/srv/shop/compose.override.yaml"
            }
        });
        let container = container_from_summary(&summary);
        assert_eq!(container.name, "shop-web-1");
        assert_eq!(container.ports, "8080:80");
        assert_eq!(container.networks, ["shop_default"]);
        assert_eq!(container.volumes, [Mount { name: "shop_data".into(), destination: "/data".into() }]);
        assert_eq!(container.depends_on, ["db", "redis"]);
        assert_eq!(container.config_files.len(), 2);
        assert!(container.running());
    }

    #[test]
    fn gives_one_image_row_per_tag() {
        let tagged = json!({"Id": "sha256:1", "RepoTags": ["nginx:1.27", "localhost:5000/nginx:latest"], "Created": 1, "Size": 2});
        let references: Vec<_> = images_from_summary(&tagged).into_iter().map(|image| image.reference).collect();
        assert_eq!(references, ["nginx:1.27", "localhost:5000/nginx:latest"]);
        let untagged = json!({"Id": "sha256:2", "RepoTags": ["<none>:<none>"], "Created": 1, "Size": 2});
        assert_eq!(images_from_summary(&untagged)[0].reference, "sha256:2");
    }

    #[test]
    fn ignores_chunks_of_only_exec_events() {
        assert!(only_exec_events(br#"{"Type":"container","Action":"exec_start: sh -c true"}"#));
        assert!(!only_exec_events(b"{\"Action\":\"exec_die\"}\n{\"Action\":\"die\"}\n"));
        assert!(!only_exec_events(b"not json"));
        assert!(!only_exec_events(b"\n"));
    }

    #[test]
    fn takes_unlabelled_hash_volumes_for_anonymous() {
        let old = json!({"Name": "5f9d6d220f7c5f9d6d220f7c5f9d6d220f7c5f9d6d220f7c5f9d6d220f7c5f9d", "Labels": null});
        assert!(volume_from_summary(&old).anonymous);
        assert!(!volume_from_summary(&json!({"Name": "shop_data"})).anonymous);
    }

    #[test]
    fn escapes_query_values() {
        assert_eq!(escape(r#"{"a":["b c"]}"#), "%7B%22a%22:%5B%22b%20c%22%5D%7D");
        assert_eq!(escape("ghcr.io/x/y:1"), "ghcr.io/x/y:1");
        assert_eq!(short_id("sha256:0123456789abcdef"), "0123456789ab");
    }
}
