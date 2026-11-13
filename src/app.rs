use crate::compose::{self, Project};
use crate::docker::{Container, Docker, Engine, Image, Network, Snapshot, Volume};
use crate::files::{self, FileEntry};
use crate::http::{Error, Result, Stream};
use crate::logs::{self, LineSplitter, LogDecoder, LogLine};
use crate::runtime::{self, Runtime};
use crate::settings::{self, Settings};
use crate::stats::{self, Sample};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Panel {
    Runtimes,
    Projects,
    Containers,
    Images,
    Volumes,
    Networks,
}

pub const PANELS: [Panel; 6] = [Panel::Runtimes, Panel::Projects, Panel::Containers, Panel::Images, Panel::Volumes, Panel::Networks];

impl Panel {
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn title(self) -> &'static str {
        match self {
            Panel::Runtimes => "Runtimes",
            Panel::Projects => "Projects",
            Panel::Containers => "Containers",
            Panel::Images => "Images",
            Panel::Volumes => "Volumes",
            Panel::Networks => "Networks",
        }
    }

    pub fn tabs(self) -> &'static [Tab] {
        match self {
            Panel::Runtimes => &[Tab::Engine],
            Panel::Projects => &[Tab::Logs, Tab::Services, Tab::Compose],
            Panel::Containers => &[Tab::Logs, Tab::Stats, Tab::Env, Tab::Inspect, Tab::Top, Tab::Files],
            Panel::Images => &[Tab::Inspect, Tab::History],
            Panel::Volumes | Panel::Networks => &[Tab::Inspect],
        }
    }

    /// What `a` narrows the list to.
    pub fn narrow_label(self) -> &'static str {
        if self == Panel::Containers { "running" } else { "unused" }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Engine,
    Logs,
    Services,
    Compose,
    Stats,
    Env,
    Inspect,
    Top,
    Files,
    History,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    StartStop,
    Restart,
    Pause,
    Remove,
    Shell,
    Narrow,
    Up,
    Down,
    ComposePull,
    Build,
    Pull,
    Prune,
    Save,
    Upload,
    Timestamps,
    Rescan,
    Filter,
    Menu,
    Quit,
}

pub struct Binding {
    pub key: char,
    pub label: &'static str,
    pub cmd: Cmd,
}

const fn bind(key: char, label: &'static str, cmd: Cmd) -> Binding {
    Binding { key, label, cmd }
}

const RUNTIME_KEYS: &[Binding] = &[];
const PROJECT_KEYS: &[Binding] = &[
    bind('u', "up", Cmd::Up),
    bind('D', "down", Cmd::Down),
    bind('s', "start/stop", Cmd::StartStop),
    bind('r', "restart", Cmd::Restart),
    bind('P', "pull", Cmd::ComposePull),
    bind('b', "build", Cmd::Build),
    bind('d', "remove", Cmd::Remove),
    bind('t', "timestamps", Cmd::Timestamps),
];
const CONTAINER_KEYS: &[Binding] = &[
    bind('s', "start/stop", Cmd::StartStop),
    bind('r', "restart", Cmd::Restart),
    bind('p', "pause", Cmd::Pause),
    bind('e', "shell", Cmd::Shell),
    bind('d', "remove", Cmd::Remove),
    bind('a', "only running", Cmd::Narrow),
    bind('t', "timestamps", Cmd::Timestamps),
    bind('S', "save file", Cmd::Save),
    bind('U', "upload", Cmd::Upload),
];
const IMAGE_KEYS: &[Binding] = &[bind('P', "pull", Cmd::Pull), bind('d', "remove", Cmd::Remove), bind('X', "prune", Cmd::Prune), bind('a', "only unused", Cmd::Narrow)];
const RESOURCE_KEYS: &[Binding] = &[bind('d', "remove", Cmd::Remove), bind('X', "prune", Cmd::Prune), bind('a', "only unused", Cmd::Narrow)];
pub const GLOBAL_KEYS: &[Binding] = &[bind('/', "filter", Cmd::Filter), bind('R', "rescan", Cmd::Rescan), bind('x', "menu", Cmd::Menu), bind('?', "menu", Cmd::Menu), bind('q', "quit", Cmd::Quit)];

pub fn bindings(panel: Panel) -> &'static [Binding] {
    match panel {
        Panel::Runtimes => RUNTIME_KEYS,
        Panel::Projects => PROJECT_KEYS,
        Panel::Containers => CONTAINER_KEYS,
        Panel::Images => IMAGE_KEYS,
        Panel::Volumes | Panel::Networks => RESOURCE_KEYS,
    }
}

pub enum Msg {
    Runtimes(Vec<Runtime>),
    /// Connection, refresh number, result.
    Snapshot(u64, u64, Result<Snapshot>),
    Engine(u64, Result<Engine>),
    DockerEvent(u64),
    EventsEnded(u64, Option<Error>),
    Stats(u64, String, Sample),
    StatsEnded(u64, String),
    Logs(u64, Vec<LogLine>),
    Text(u64, Result<Vec<String>>),
    Files(u64, String, Result<Vec<FileEntry>>),
    /// View, file path, lines.
    Preview(u64, String, Result<Vec<String>>),
    Done(String, Result<Option<String>>),
    /// Text, whether it is an error.
    Notice(String, bool),
}

pub enum Content {
    Loading,
    Lines(Vec<String>),
    Failed(String),
}

pub struct FilesView {
    pub folder: String,
    /// None while the folder loads.
    pub entries: Option<Result<Vec<FileEntry>>>,
    pub selected: usize,
    pub preview: Content,
    /// Preview lines scrolled past, moved by PageUp and PageDown.
    pub preview_scroll: usize,
    /// Name to select once the folder loads: the folder just left, after going up.
    reselect: Option<String>,
}

/// What the main panel shows for the selection and tab; `key` changes when it must load again.
pub struct View {
    pub key: String,
    pub id: u64,
    pub logs: VecDeque<LogLine>,
    pub content: Content,
    /// Log lines up from the bottom on the Logs tab, lines down from the top elsewhere.
    pub scroll: usize,
    pub files: FilesView,
    loaded_at: Instant,
    _streams: Vec<Stream>,
}

impl View {
    fn new(key: String, id: u64) -> Self {
        View {
            key,
            id,
            logs: VecDeque::new(),
            content: Content::Loading,
            scroll: 0,
            files: FilesView { folder: "/".into(), entries: None, selected: 0, preview: Content::Lines(Vec::new()), preview_scroll: 0, reselect: None },
            loaded_at: Instant::now(),
            _streams: Vec::new(),
        }
    }
}

type Job = Box<dyn FnOnce(&Docker) -> Result<Option<String>> + Send>;

pub enum Popup {
    Confirm {
        title: String,
        body: String,
        job: (String, Job),
    },
    Pull {
        value: String,
    },
    /// A host path to copy into the open container folder.
    Upload {
        value: String,
    },
    Menu {
        selected: usize,
    },
    Error {
        title: String,
        body: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Editing {
    List,
    /// The text filter of the main panel's lines.
    Main,
}

pub struct Notice {
    pub text: String,
    pub error: bool,
    at: Instant,
}

const REFRESH_DELAY: Duration = Duration::from_millis(150);
const RESCAN_EVERY: Duration = Duration::from_secs(5);
const TOP_EVERY: Duration = Duration::from_secs(2);
const PREVIEW_LIMIT: u64 = 512 * 1024;

pub struct App {
    tx: Sender<Msg>,
    pub runtimes: Vec<Runtime>,
    pub chosen: Option<String>,
    pub docker: Option<Docker>,
    /// Bumped on every connect and disconnect, so answers from an old connection are dropped.
    conn: u64,
    pub engine: Option<Result<Engine>>,
    pub snapshot: Snapshot,
    pub projects: Vec<Project>,
    local: Option<Project>,
    events: Option<Stream>,
    refresh_at: Option<Instant>,
    /// Snapshots are numbered, so a slow one that lands late does not overwrite a newer one.
    refreshes: u64,
    applied_refresh: u64,
    rescan_at: Instant,
    rescanning: bool,
    pub stats: HashMap<String, VecDeque<Sample>>,
    stat_streams: HashMap<String, Stream>,
    pub focus: Panel,
    pub main_focused: bool,
    selected: [Option<String>; PANELS.len()],
    pub filters: [String; PANELS.len()],
    pub narrow: [bool; PANELS.len()],
    tabs: [usize; PANELS.len()],
    pub editing: Option<Editing>,
    pub main_filter: String,
    pub timestamps: bool,
    pub view: View,
    pub popup: Option<Popup>,
    pub busy: Vec<String>,
    pub notice: Option<Notice>,
    /// Main panel height, set on each draw, for paging.
    pub page: usize,
    pub shell: Option<Command>,
    pub quit: bool,
}

impl App {
    pub fn new(tx: Sender<Msg>, local: Option<Project>) -> Self {
        let projects = compose::projects(&[], local.as_ref());
        let mut narrow = [false; PANELS.len()];
        for (slot, saved) in narrow.iter_mut().zip(settings::load().narrow) {
            *slot = saved;
        }
        let notice = std::env::var("DOCKER_HOST").ok().filter(|host| !host.is_empty() && !host.starts_with("unix://")).map(|host| Notice {
            text: format!("DOCKER_HOST {host} is not a Unix socket, so dockwatch ignores it."),
            error: true,
            at: Instant::now(),
        });
        App {
            tx,
            runtimes: Vec::new(),
            chosen: None,
            docker: None,
            conn: 0,
            engine: None,
            snapshot: Snapshot::default(),
            projects,
            local,
            events: None,
            refresh_at: None,
            refreshes: 0,
            applied_refresh: 0,
            // Due at once, so the first tick scans.
            rescan_at: Instant::now().checked_sub(RESCAN_EVERY).unwrap_or_else(Instant::now),
            rescanning: false,
            stats: HashMap::new(),
            stat_streams: HashMap::new(),
            focus: Panel::Containers,
            main_focused: false,
            selected: Default::default(),
            filters: Default::default(),
            narrow,
            tabs: [0; PANELS.len()],
            editing: None,
            main_filter: String::new(),
            timestamps: true,
            view: View::new(String::new(), 0),
            popup: None,
            busy: Vec::new(),
            notice,
            page: 20,
            shell: None,
            quit: false,
        }
    }

    pub fn tab(&self) -> Tab {
        self.focus.tabs()[self.tabs[self.focus.index()]]
    }

    pub fn tab_index(&self) -> usize {
        self.tabs[self.focus.index()]
    }

    pub fn containers(&self) -> Vec<&Container> {
        let filter = &self.filters[Panel::Containers.index()];
        let only_running = self.narrow[Panel::Containers.index()];
        self.snapshot
            .containers
            .iter()
            .filter(|container| !only_running || container.up())
            .filter(|container| matches(filter, &[&container.name, &container.image, container.project.as_deref().unwrap_or_default()]))
            .collect()
    }

    pub fn visible_projects(&self) -> Vec<&Project> {
        let filter = &self.filters[Panel::Projects.index()];
        self.projects.iter().filter(|project| matches(filter, &[&project.name])).collect()
    }

    pub fn images(&self) -> Vec<&Image> {
        let filter = &self.filters[Panel::Images.index()];
        let only_unused = self.narrow[Panel::Images.index()];
        self.snapshot.images.iter().filter(|image| !only_unused || !self.image_used(image)).filter(|image| matches(filter, &[&image.reference])).collect()
    }

    pub fn volumes(&self) -> Vec<&Volume> {
        let filter = &self.filters[Panel::Volumes.index()];
        let only_unused = self.narrow[Panel::Volumes.index()];
        let mut volumes: Vec<(&Volume, String)> = self
            .snapshot
            .volumes
            .iter()
            .filter(|volume| !only_unused || !self.volume_used(volume))
            .map(|volume| (volume, self.volume_label(volume)))
            .filter(|(volume, label)| matches(filter, &[&volume.name, label]))
            .collect();
        volumes.sort_by(|(a, a_label), (b, b_label)| (a.anonymous, a_label).cmp(&(b.anonymous, b_label)));
        volumes.into_iter().map(|(volume, _)| volume).collect()
    }

    pub fn networks(&self) -> Vec<&Network> {
        let filter = &self.filters[Panel::Networks.index()];
        let only_unused = self.narrow[Panel::Networks.index()];
        self.snapshot.networks.iter().filter(|network| !only_unused || !self.network_used(network)).filter(|network| matches(filter, &[&network.name, &network.driver, &network.subnet])).collect()
    }

    pub fn image_used(&self, image: &Image) -> bool {
        self.snapshot.containers.iter().any(|container| container.image_id == image.id)
    }

    pub fn volume_used(&self, volume: &Volume) -> bool {
        self.snapshot.containers.iter().any(|container| container.volumes.iter().any(|mount| mount.name == volume.name))
    }

    /// An anonymous volume's hash says nothing, so it goes by where it is mounted: `db-1:/var/lib/postgresql/data`.
    pub fn volume_label(&self, volume: &Volume) -> String {
        if !volume.anonymous {
            return volume.name.clone();
        }
        let mut users = Vec::new();
        for container in &self.snapshot.containers {
            for mount in container.volumes.iter().filter(|mount| mount.name == volume.name) {
                users.push(format!("{}:{}", container.name, mount.destination));
            }
        }
        if users.is_empty() { format!("unused anonymous, created {}", volume.created_at.get(..16).unwrap_or(&volume.created_at).replace('T', " ")) } else { users.join(", ") }
    }

    pub fn network_used(&self, network: &Network) -> bool {
        network.system || self.snapshot.containers.iter().any(|container| container.networks.contains(&network.name))
    }

    /// The header of an image, volume or network's Inspect tab: the containers that use it, then a blank line.
    fn used_by(&self, uses: impl Fn(&Container) -> bool) -> Vec<String> {
        let names: Vec<&str> = self.snapshot.containers.iter().filter(|container| uses(container)).map(|container| container.name.as_str()).collect();
        let names = if names.is_empty() { "no container".to_string() } else { names.join(", ") };
        vec![format!("Used by: {names}"), String::new()]
    }

    fn keys(&self, panel: Panel) -> Vec<String> {
        match panel {
            Panel::Runtimes => self.runtimes.iter().map(|runtime| runtime.socket.clone()).collect(),
            Panel::Projects => self.visible_projects().iter().map(|project| project.name.clone()).collect(),
            Panel::Containers => self.containers().iter().map(|container| container.id.clone()).collect(),
            Panel::Images => self.images().iter().map(|image| image.reference.clone()).collect(),
            Panel::Volumes => self.volumes().iter().map(|volume| volume.name.clone()).collect(),
            Panel::Networks => self.networks().iter().map(|network| network.id.clone()).collect(),
        }
    }

    /// The row the selection sits on; the first row when its item is gone.
    pub fn selected_index(&self, panel: Panel) -> Option<usize> {
        let keys = self.keys(panel);
        if keys.is_empty() {
            return None;
        }
        let selected = self.selected[panel.index()].as_ref();
        Some(selected.and_then(|key| keys.iter().position(|candidate| candidate == key)).unwrap_or(0))
    }

    fn selected_key(&self, panel: Panel) -> Option<String> {
        let keys = self.keys(panel);
        let selected = self.selected[panel.index()].as_ref();
        let index = selected.and_then(|key| keys.iter().position(|candidate| candidate == key)).unwrap_or(0);
        keys.into_iter().nth(index)
    }

    fn move_selection(&mut self, delta: isize) {
        let keys = self.keys(self.focus);
        let Some(index) = self.selected_index(self.focus) else { return };
        let next = index.saturating_add_signed(delta).min(keys.len() - 1);
        self.selected[self.focus.index()] = Some(keys[next].clone());
    }

    pub fn selected_container(&self) -> Option<&Container> {
        let id = self.selected_key(Panel::Containers)?;
        self.snapshot.containers.iter().find(|container| container.id == id)
    }

    pub fn selected_project(&self) -> Option<&Project> {
        let name = self.selected_key(Panel::Projects)?;
        self.projects.iter().find(|project| project.name == name)
    }

    pub fn selected_runtime(&self) -> Option<&Runtime> {
        let socket = self.selected_key(Panel::Runtimes)?;
        self.runtimes.iter().find(|runtime| runtime.socket == socket)
    }

    fn selected_image(&self) -> Option<&Image> {
        let reference = self.selected_key(Panel::Images)?;
        self.snapshot.images.iter().find(|image| image.reference == reference)
    }

    fn selected_volume(&self) -> Option<&Volume> {
        let name = self.selected_key(Panel::Volumes)?;
        self.snapshot.volumes.iter().find(|volume| volume.name == name)
    }

    fn selected_network(&self) -> Option<&Network> {
        let id = self.selected_key(Panel::Networks)?;
        self.snapshot.networks.iter().find(|network| network.id == id)
    }

    /// The selected container, or every container of the selected project.
    fn target_containers(&self) -> Vec<Container> {
        match self.focus {
            Panel::Containers => self.selected_container().cloned().into_iter().collect(),
            Panel::Projects => self.selected_project().map(|project| project.containers.clone()).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn rescan(&mut self) {
        self.rescan_at = Instant::now();
        self.schedule_refresh();
        if self.rescanning {
            return;
        }
        self.rescanning = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Msg::Runtimes(runtime::discover()));
        });
    }

    /// Keeps the chosen runtime by socket and connects once it runs. A stopped runtime is left to
    /// `Msg::EventsEnded`, so one slow ping does not drop a working connection.
    fn apply_runtimes(&mut self, found: Vec<Runtime>) {
        self.runtimes = found;
        if self.chosen.is_none() {
            let saved = settings::load().runtime;
            self.chosen = runtime::launch_runtime(&self.runtimes, saved.as_deref()).map(|runtime| runtime.socket.clone());
        }
        let running = self.runtimes.iter().any(|runtime| Some(&runtime.socket) == self.chosen.as_ref() && runtime.running());
        if let (Some(socket), true, false) = (&self.chosen, running, self.docker.is_some()) {
            self.connect(socket.clone());
        }
    }

    fn switch_runtime(&mut self) {
        let Some(runtime) = self.selected_runtime().cloned() else { return };
        if self.chosen.as_ref() == Some(&runtime.socket) && self.docker.is_some() {
            return;
        }
        self.chosen = Some(runtime.socket.clone());
        self.save_settings();
        self.disconnect();
        if runtime.running() {
            self.connect(runtime.socket);
        } else {
            self.notify(format!("{} is not running.", runtime.name), true);
        }
    }

    fn save_settings(&self) {
        settings::save(&Settings { runtime: self.chosen.clone(), narrow: self.narrow.to_vec(), ..settings::load() });
    }

    fn connect(&mut self, socket: String) {
        self.conn += 1;
        let conn = self.conn;
        let docker = Docker { socket };
        let (on_event, on_end) = (self.tx.clone(), self.tx.clone());
        self.events = Some(docker.events(
            move || {
                let _ = on_event.send(Msg::DockerEvent(conn));
            },
            move |error| {
                let _ = on_end.send(Msg::EventsEnded(conn, error));
            },
        ));
        self.docker = Some(docker.clone());
        self.refresh();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Msg::Engine(conn, docker.engine()));
        });
    }

    fn disconnect(&mut self) {
        self.conn += 1;
        self.events = None;
        self.docker = None;
        self.engine = None;
        self.stat_streams.clear();
        self.stats.clear();
        self.apply_snapshot(Snapshot::default());
    }

    /// Events come in bursts (a compose up sends dozens); one snapshot per 150 ms is enough.
    fn schedule_refresh(&mut self) {
        self.refresh_at.get_or_insert(Instant::now() + REFRESH_DELAY);
    }

    fn refresh(&mut self) {
        self.refresh_at = None;
        let Some(docker) = self.docker.clone() else { return };
        self.refreshes += 1;
        let (tx, conn, refresh) = (self.tx.clone(), self.conn, self.refreshes);
        thread::spawn(move || {
            let _ = tx.send(Msg::Snapshot(conn, refresh, docker.snapshot()));
        });
    }

    fn apply_snapshot(&mut self, snapshot: Snapshot) {
        let rows: Vec<Option<usize>> = PANELS.iter().map(|&panel| self.selected_index(panel)).collect();
        self.snapshot = snapshot;
        self.projects = compose::projects(&self.snapshot.containers, self.local.as_ref());
        // A removed item hands the selection to its neighbour, not to the first row.
        for (panel, row) in PANELS.into_iter().zip(rows) {
            let keys = self.keys(panel);
            let slot = &mut self.selected[panel.index()];
            if let (Some(row), Some(key)) = (row, slot.as_ref())
                && !keys.is_empty()
                && !keys.contains(key)
            {
                *slot = Some(keys[row.min(keys.len() - 1)].clone());
            }
        }
        let running: Vec<String> = self.snapshot.containers.iter().filter(|container| container.running()).map(|container| container.id.clone()).collect();
        self.stat_streams.retain(|id, _| running.contains(id));
        self.stats.retain(|id, _| running.contains(id));
        let Some(docker) = self.docker.clone() else { return };
        for id in running {
            if self.stat_streams.contains_key(&id) {
                continue;
            }
            let (tx, end_tx, conn, key, end_key) = (self.tx.clone(), self.tx.clone(), self.conn, id.clone(), id.clone());
            let mut lines = LineSplitter::default();
            let stream = docker.stats(
                &id,
                move |chunk, _| {
                    for line in lines.feed(chunk) {
                        if let Ok(stats) = serde_json::from_str::<Value>(&line) {
                            let _ = tx.send(Msg::Stats(conn, key.clone(), Sample::from_stats(&stats)));
                        }
                    }
                },
                move |_| {
                    let _ = end_tx.send(Msg::StatsEnded(conn, end_key));
                },
            );
            self.stat_streams.insert(id, stream);
        }
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Runtimes(found) => {
                self.rescanning = false;
                self.apply_runtimes(found);
            }
            Msg::Snapshot(conn, refresh, result) if conn == self.conn && refresh > self.applied_refresh => {
                self.applied_refresh = refresh;
                match result {
                    Ok(snapshot) if snapshot != self.snapshot => self.apply_snapshot(snapshot),
                    Ok(_) => {}
                    Err(error) => self.notify(error.message, true),
                }
            }
            Msg::Engine(conn, engine) if conn == self.conn => self.engine = Some(engine),
            Msg::DockerEvent(conn) if conn == self.conn => self.schedule_refresh(),
            Msg::StatsEnded(conn, id) if conn == self.conn => {
                self.stat_streams.remove(&id);
            }
            // The runtime stopped or its socket went away; the next rescan reconnects.
            Msg::EventsEnded(conn, error) if conn == self.conn => {
                if let Some(error) = error {
                    self.notify(format!("Cannot follow Docker events: {}", error.message), true);
                }
                self.disconnect();
            }
            Msg::Stats(conn, id, sample) if conn == self.conn && self.stat_streams.contains_key(&id) => {
                stats::push(self.stats.entry(id).or_default(), sample);
            }
            Msg::Logs(view, lines) if view == self.view.id => {
                let query = self.main_filter.to_lowercase();
                let shown = lines.iter().filter(|line| logs::matches(line, &query)).count();
                logs::merge(&mut self.view.logs, lines);
                // Holds a scrolled-up view on the same lines while new ones arrive below.
                if self.view.scroll > 0 {
                    let total = self.view.logs.iter().filter(|line| logs::matches(line, &query)).count();
                    self.view.scroll = (self.view.scroll + shown).min(total.saturating_sub(1));
                }
            }
            Msg::Text(view, result) if view == self.view.id => {
                self.view.content = match result {
                    Ok(lines) => Content::Lines(lines),
                    Err(error) => Content::Failed(error.message),
                };
            }
            Msg::Files(view, folder, result) if view == self.view.id && folder == self.view.files.folder => {
                let files = &mut self.view.files;
                if let (Ok(entries), Some(name)) = (&result, files.reselect.take()) {
                    files.selected = entries.iter().position(|entry| entry.name == name).unwrap_or(0);
                }
                files.entries = Some(result);
            }
            Msg::Preview(view, path, result) if view == self.view.id && self.selected_file().is_some_and(|entry| entry.path == path) => {
                self.view.files.preview_scroll = 0;
                self.view.files.preview = match result {
                    Ok(lines) => Content::Lines(lines),
                    Err(error) => Content::Failed(error.message),
                };
            }
            Msg::Notice(text, error) => self.notify(text, error),
            Msg::Done(label, result) => {
                if let Some(index) = self.busy.iter().position(|busy| *busy == label) {
                    self.busy.remove(index);
                }
                match result {
                    Ok(Some(message)) => self.notify(message, false),
                    Ok(None) => {}
                    Err(error) => self.popup = Some(Popup::Error { title: format!("{label} failed"), body: error.message }),
                }
                self.schedule_refresh();
            }
            _ => {}
        }
    }

    /// Timers and the view's reload; main calls it before every draw.
    pub fn tick(&mut self) {
        if self.refresh_at.is_some_and(|at| at <= Instant::now()) {
            self.refresh();
        }
        if self.rescan_at.elapsed() >= RESCAN_EVERY {
            self.rescan();
        }
        let key = self.view_key();
        if key != self.view.key {
            self.open_view(key);
        }
        if self.tab() == Tab::Top && self.view.loaded_at.elapsed() >= TOP_EVERY {
            self.load_top();
        }
        if self.notice.as_ref().is_some_and(|notice| notice.at.elapsed() >= Duration::from_secs(4)) {
            self.notice = None;
        }
    }

    fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.notice = Some(Notice { text: text.into(), error, at: Instant::now() });
    }

    /// Identity of what the main panel shows: a container's running state is in it, so its logs
    /// stream again after a start, and so are a project's running members.
    fn view_key(&self) -> String {
        let Some(key) = self.selected_key(self.focus) else { return String::new() };
        let state = match self.focus {
            Panel::Containers => self.selected_container().map(|container| container.state.clone()).unwrap_or_default(),
            Panel::Projects => {
                let project = self.selected_project();
                let members = project.iter().flat_map(|project| &project.containers);
                members.map(|container| format!("{}={}", container.id, container.state)).collect::<Vec<_>>().join(",")
            }
            _ => String::new(),
        };
        format!("{} {:?} {key} {state} {:?}", self.conn, self.focus, self.tab())
    }

    fn open_view(&mut self, key: String) {
        self.view = View::new(key, self.view.id + 1);
        // The local compose file reads without a runtime.
        if (self.focus, self.tab()) == (Panel::Projects, Tab::Compose) {
            let files = self.selected_project().map(|project| project.config_files.clone()).unwrap_or_default();
            self.view.content = Content::Lines(compose_file_lines(&files));
            return;
        }
        let Some(docker) = self.docker.clone() else { return };
        match (self.focus, self.tab()) {
            (Panel::Containers, Tab::Logs) => {
                if let Some(container) = self.selected_container().cloned() {
                    self.stream_logs(&docker, &container.id, None, 1000);
                }
            }
            (Panel::Projects, Tab::Logs) => {
                for container in self.selected_project().map(|project| project.containers.clone()).unwrap_or_default() {
                    let source = container.service.clone().unwrap_or(container.name.clone());
                    self.stream_logs(&docker, &container.id, Some(source), 300);
                }
            }
            (Panel::Containers, Tab::Env | Tab::Inspect) => {
                let Some(id) = self.selected_container().map(|container| container.id.clone()) else { return };
                let env = self.tab() == Tab::Env;
                self.fetch(move |docker| {
                    let inspect = docker.inspect_container(&id)?;
                    if !env {
                        return Ok(pretty(&inspect));
                    }
                    // The second is where Apple's container keeps it.
                    let env = inspect["Config"]["Env"].as_array().or(inspect["configuration"]["initProcess"]["environment"].as_array());
                    let mut lines: Vec<String> = env.into_iter().flatten().filter_map(|entry| entry.as_str().map(String::from)).collect();
                    lines.sort();
                    Ok(lines)
                });
            }
            (Panel::Containers, Tab::Top) => self.load_top(),
            (Panel::Containers, Tab::Files) => self.load_folder("/".into(), None),
            (Panel::Images, tab) => {
                let Some(image) = self.selected_image().cloned() else { return };
                let used_by = self.used_by(|container| container.image_id == image.id);
                self.fetch(move |docker| {
                    if tab == Tab::History {
                        return history_lines(&docker.image_history(&image.reference)?);
                    }
                    Ok([used_by, pretty(&docker.inspect_image(&image.reference)?)].concat())
                });
            }
            (Panel::Volumes, _) => {
                let Some(name) = self.selected_volume().map(|volume| volume.name.clone()) else { return };
                let used_by = self.used_by(|container| container.volumes.iter().any(|mount| mount.name == name));
                self.fetch(move |docker| Ok([used_by, pretty(&docker.inspect_volume(&name)?)].concat()));
            }
            (Panel::Networks, _) => {
                let Some(network) = self.selected_network().cloned() else { return };
                let used_by = self.used_by(|container| container.networks.contains(&network.name));
                self.fetch(move |docker| Ok([used_by, pretty(&docker.inspect_network(&network.id)?)].concat()));
            }
            _ => {}
        }
    }

    fn stream_logs(&mut self, docker: &Docker, id: &str, source: Option<String>, tail: usize) {
        let (tx, end_tx, view) = (self.tx.clone(), self.tx.clone(), self.view.id);
        let mut decoder = LogDecoder::default();
        let stream = docker.logs(
            id,
            tail,
            move |chunk, content_type| {
                let mut lines = decoder.feed(chunk, content_type);
                if lines.is_empty() {
                    return;
                }
                for line in &mut lines {
                    line.source.clone_from(&source);
                }
                let _ = tx.send(Msg::Logs(view, lines));
            },
            move |error| {
                if let Some(error) = error {
                    let _ = end_tx.send(Msg::Text(view, Err(error)));
                }
            },
        );
        self.view._streams.push(stream);
    }

    fn fetch(&mut self, job: impl FnOnce(&Docker) -> Result<Vec<String>> + Send + 'static) {
        let Some(docker) = self.docker.clone() else { return };
        let (tx, view) = (self.tx.clone(), self.view.id);
        self.view.loaded_at = Instant::now();
        thread::spawn(move || {
            let _ = tx.send(Msg::Text(view, job(&docker)));
        });
    }

    fn load_top(&mut self) {
        let Some(container) = self.selected_container().cloned() else { return };
        self.view.loaded_at = Instant::now();
        if !container.running() {
            self.view.content = Content::Failed("Start the container to see its processes.".into());
            return;
        }
        self.fetch(move |docker| Ok(top_lines(&docker.top(&container.id)?)));
    }

    fn load_folder(&mut self, folder: String, reselect: Option<String>) {
        let (Some(docker), Some(id)) = (self.docker.clone(), self.selected_container().map(|container| container.id.clone())) else { return };
        let files = &mut self.view.files;
        files.folder = folder.clone();
        files.entries = None;
        files.selected = 0;
        files.preview = Content::Lines(Vec::new());
        files.preview_scroll = 0;
        files.reselect = reselect;
        let (tx, view) = (self.tx.clone(), self.view.id);
        thread::spawn(move || {
            let result = docker.list_files(&id, &folder);
            let _ = tx.send(Msg::Files(view, folder, result));
        });
    }

    fn selected_file(&self) -> Option<&FileEntry> {
        match &self.view.files.entries {
            Some(Ok(entries)) => entries.get(self.view.files.selected),
            _ => None,
        }
    }

    fn open_file(&mut self) {
        let Some(entry) = self.selected_file().cloned() else { return };
        if entry.is_folder() {
            self.load_folder(entry.path, None);
            return;
        }
        if entry.kind != "regular file" {
            self.view.files.preview = Content::Failed("Only regular files can be previewed.".into());
            return;
        }
        if entry.size > PREVIEW_LIMIT {
            self.view.files.preview = Content::Failed(format!("{} is too large to preview ({}).", entry.name, stats::format_bytes(entry.size)));
            return;
        }
        let (Some(docker), Some(id)) = (self.docker.clone(), self.selected_container().map(|container| container.id.clone())) else { return };
        self.view.files.preview = Content::Loading;
        let (tx, view) = (self.tx.clone(), self.view.id);
        thread::spawn(move || {
            let result = docker.read_file(&id, &entry.path, PREVIEW_LIMIT).map(|bytes| preview_lines(&bytes));
            let _ = tx.send(Msg::Preview(view, entry.path, result));
        });
    }

    fn leave_folder(&mut self) {
        let folder = self.view.files.folder.clone();
        if folder != "/" {
            let name = folder.rsplit('/').next().map(String::from);
            self.load_folder(files::parent_folder(&folder), name);
        }
    }

    pub fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Some(popup) = self.popup.take() {
            self.popup_key(popup, key);
            return;
        }
        if let Some(editing) = self.editing {
            self.edit_key(editing, key);
            return;
        }
        let files = self.main_focused && self.tab() == Tab::Files;
        match key.code {
            KeyCode::Tab => self.focus_panel((self.focus.index() + 1) % PANELS.len()),
            KeyCode::BackTab => self.focus_panel((self.focus.index() + PANELS.len() - 1) % PANELS.len()),
            KeyCode::Char(digit @ '1'..='6') => self.focus_panel(digit as usize - '1' as usize),
            KeyCode::Char('[') => self.switch_tab(-1),
            KeyCode::Char(']') => self.switch_tab(1),
            KeyCode::Left | KeyCode::Char('h') if files => self.leave_folder(),
            KeyCode::Backspace if files => self.leave_folder(),
            KeyCode::Left | KeyCode::Char('h') if self.main_focused => self.main_focused = false,
            KeyCode::Left | KeyCode::Char('h') => self.focus_panel((self.focus.index() + PANELS.len() - 1) % PANELS.len()),
            KeyCode::Right | KeyCode::Char('l') if files => self.open_file(),
            KeyCode::Right | KeyCode::Char('l') if !self.main_focused => self.main_focused = true,
            KeyCode::Enter if files => self.open_file(),
            KeyCode::Enter if self.focus == Panel::Runtimes => self.switch_runtime(),
            KeyCode::Enter => self.main_focused = true,
            KeyCode::Esc if self.main_focused => self.main_focused = false,
            KeyCode::Esc => self.filters[self.focus.index()].clear(),
            KeyCode::PageUp if files => self.scroll_preview(-(self.page.max(2) as isize - 1)),
            KeyCode::PageDown if files => self.scroll_preview(self.page.max(2) as isize - 1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::PageUp => self.step(-(self.page.max(2) as isize - 1)),
            KeyCode::PageDown => self.step(self.page.max(2) as isize - 1),
            KeyCode::Home | KeyCode::Char('g') => self.step(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.step(isize::MAX / 2),
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                if let Some(binding) = bindings(self.focus).iter().chain(GLOBAL_KEYS).find(|binding| binding.key == c) {
                    self.run(binding.cmd);
                }
            }
            _ => {}
        }
    }

    fn focus_panel(&mut self, index: usize) {
        self.focus = PANELS[index];
        self.main_focused = false;
    }

    fn switch_tab(&mut self, delta: isize) {
        let count = self.focus.tabs().len() as isize;
        let tab = &mut self.tabs[self.focus.index()];
        *tab = (*tab as isize + delta).rem_euclid(count) as usize;
    }

    /// Down is positive: the next row in a list, newer lines in logs, further down in text.
    fn step(&mut self, delta: isize) {
        if !self.main_focused {
            self.move_selection(delta);
            return;
        }
        match self.tab() {
            Tab::Logs => {
                let query = self.main_filter.to_lowercase();
                let shown = self.view.logs.iter().filter(|line| logs::matches(line, &query)).count();
                self.view.scroll = self.view.scroll.saturating_add_signed(-delta).min(shown.saturating_sub(1));
            }
            Tab::Files => {
                let count = match &self.view.files.entries {
                    Some(Ok(entries)) => entries.len(),
                    _ => 0,
                };
                let files = &mut self.view.files;
                let selected = files.selected.saturating_add_signed(delta).min(count.saturating_sub(1));
                if selected != files.selected {
                    files.selected = selected;
                    files.preview = Content::Lines(Vec::new());
                    files.preview_scroll = 0;
                }
            }
            // draw_content clamps it to the wrapped, filtered rows.
            _ => self.view.scroll = self.view.scroll.saturating_add_signed(delta),
        }
    }

    fn scroll_preview(&mut self, delta: isize) {
        let files = &mut self.view.files;
        let Content::Lines(lines) = &files.preview else { return };
        files.preview_scroll = files.preview_scroll.saturating_add_signed(delta).min(lines.len().saturating_sub(1));
    }

    fn edit_key(&mut self, editing: Editing, key: KeyEvent) {
        let text = match editing {
            Editing::List => &mut self.filters[self.focus.index()],
            Editing::Main => &mut self.main_filter,
        };
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Enter => self.editing = None,
            KeyCode::Esc => {
                text.clear();
                self.editing = None;
            }
            _ => {}
        }
        if editing == Editing::Main {
            self.view.scroll = 0;
        }
    }

    fn popup_key(&mut self, popup: Popup, key: KeyEvent) {
        match popup {
            Popup::Confirm { job: (label, job), .. } if key.code == KeyCode::Char('y') => self.spawn_job(label, job),
            popup @ Popup::Confirm { .. } if !matches!(key.code, KeyCode::Esc | KeyCode::Char('n' | 'q')) => self.popup = Some(popup),
            Popup::Confirm { .. } => {}
            Popup::Pull { value } => {
                let Some(reference) = self.prompt_key(key, value, |value| Popup::Pull { value }) else { return };
                self.pull(reference);
            }
            Popup::Upload { value } => {
                let Some(path) = self.prompt_key(key, value, |value| Popup::Upload { value }) else { return };
                self.upload(path);
            }
            Popup::Menu { selected } => {
                let menu = self.menu();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.popup = Some(Popup::Menu { selected: selected.saturating_sub(1) }),
                    KeyCode::Down | KeyCode::Char('j') => self.popup = Some(Popup::Menu { selected: (selected + 1).min(menu.len() - 1) }),
                    KeyCode::Enter => self.run(menu[selected].cmd),
                    KeyCode::Esc | KeyCode::Char('x' | '?') => {}
                    KeyCode::Char(c) => match menu.iter().find(|binding| binding.key == c) {
                        Some(binding) => self.run(binding.cmd),
                        None => self.popup = Some(Popup::Menu { selected }),
                    },
                    _ => self.popup = Some(Popup::Menu { selected }),
                }
            }
            Popup::Error { .. } => {}
        }
    }

    /// The focused panel's commands, then the global ones, once each.
    pub fn menu(&self) -> Vec<&'static Binding> {
        let mut menu: Vec<&Binding> = bindings(self.focus).iter().collect();
        menu.extend(GLOBAL_KEYS.iter().filter(|binding| binding.cmd != Cmd::Menu));
        menu
    }

    /// One key in a one-line prompt: the trimmed text once Enter is pressed on a non-blank value;
    /// otherwise the prompt stays open, unless it was Esc.
    fn prompt_key(&mut self, key: KeyEvent, mut value: String, popup: fn(String) -> Popup) -> Option<String> {
        match key.code {
            KeyCode::Enter if !value.trim().is_empty() => return Some(value.trim().to_string()),
            KeyCode::Esc => return None,
            KeyCode::Backspace => {
                value.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => value.push(c),
            _ => {}
        }
        self.popup = Some(popup(value));
        None
    }

    fn run(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Quit if !self.busy.is_empty() => self.notify(format!("Wait for {} to finish, or press Ctrl-C.", self.busy.join(", ")), true),
            Cmd::Quit => self.quit = true,
            Cmd::Menu => self.popup = Some(Popup::Menu { selected: 0 }),
            Cmd::Filter => self.editing = Some(if self.main_focused { Editing::Main } else { Editing::List }),
            Cmd::Rescan => self.rescan(),
            Cmd::Timestamps => self.timestamps = !self.timestamps,
            Cmd::Narrow => {
                self.narrow[self.focus.index()] ^= true;
                self.save_settings();
            }
            Cmd::Save => self.save_file(),
            Cmd::Upload if self.tab() == Tab::Files && self.view.files.entries.is_some() => self.popup = Some(Popup::Upload { value: String::new() }),
            Cmd::Upload => self.notify("Open the Files tab to upload into its folder.", true),
            Cmd::StartStop => self.start_stop(),
            Cmd::Restart => {
                let batches = compose::start_batches(&self.target_containers());
                self.spawn("Restart", move |docker| run_batches(docker, batches, "restart"));
            }
            Cmd::Pause => {
                let Some(container) = self.selected_container().cloned() else { return };
                let action = match container.state.as_str() {
                    "paused" => "unpause",
                    "running" => "pause",
                    _ => return self.notify(format!("{} is not running.", container.name), true),
                };
                self.spawn(format!("{} {}", if action == "pause" { "Pause" } else { "Unpause" }, container.name), move |docker| docker.container_action(&container.id, action).map(|_| None));
            }
            Cmd::Remove => self.remove(),
            Cmd::Prune => self.prune(),
            Cmd::Shell => self.open_shell(),
            Cmd::Up => self.compose(&["up", "-d"]),
            Cmd::ComposePull => self.compose(&["pull"]),
            Cmd::Build => self.compose(&["build"]),
            Cmd::Down => {
                let Some(project) = self.selected_project().cloned() else { return };
                match self.compose_command(&project, &["down"]) {
                    Ok(command) => self.confirm(
                        format!("docker compose down {}?", project.name),
                        "Its containers and networks are removed. Volumes and images stay.".into(),
                        format!("Down {}", project.name),
                        move |_| compose::run(command).map(|_| Some(format!("{} is down.", project.name))).map_err(Error::new),
                    ),
                    Err(message) => self.fail("Compose", message),
                }
            }
            Cmd::Pull => self.popup = Some(Popup::Pull { value: String::new() }),
        }
    }

    /// Through the runtime's CLI, so a private registry gets the login stored in its credential helper.
    fn pull(&mut self, reference: String) {
        let Some(socket) = self.docker.as_ref().map(|docker| docker.socket.clone()) else { return self.fail("Pull", "No Docker runtime is connected.".into()) };
        let mut command = match compose::cli(&socket) {
            Ok(command) => command,
            Err(message) => return self.fail("Pull", message),
        };
        command.args(["image", "pull", &reference]);
        self.spawn(format!("Pull {reference}"), move |_| compose::run(command).map(|_| Some(format!("Pulled {reference}."))).map_err(Error::new));
    }

    /// `docker cp` into the current directory: it unpacks the archive and keeps modes and links.
    fn save_file(&mut self) {
        let socket = self.docker.as_ref().map(|docker| docker.socket.clone());
        let id = self.selected_container().map(|container| container.id.clone());
        let entry = self.selected_file().filter(|_| self.tab() == Tab::Files).cloned();
        let (Some(socket), Some(id), Some(entry)) = (socket, id, entry) else {
            return self.notify("Select a file in the Files tab to save it.", true);
        };
        let mut command = match compose::cli(&socket) {
            Ok(command) => command,
            Err(message) => return self.fail("Save", message),
        };
        let folder = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let target = match save_target(&folder, &entry.name) {
            Ok(target) => target,
            Err(message) => return self.fail("Save", message),
        };
        command.arg("cp").arg(format!("{id}:{}", entry.path)).arg(target);
        let message = format!("Saved {} to {}.", entry.name, folder.display());
        self.spawn(format!("Save {}", entry.name), move |_| compose::run(command).map(|_| Some(message)).map_err(Error::new));
    }

    /// `docker cp` a host file or folder into the open folder, then lists the folder again.
    fn upload(&mut self, path: String) {
        let socket = self.docker.as_ref().map(|docker| docker.socket.clone());
        let (Some(socket), Some(id)) = (socket, self.selected_container().map(|container| container.id.clone())) else { return };
        let source = match path.strip_prefix("~/") {
            Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
            None => path,
        };
        if std::fs::symlink_metadata(&source).is_err() {
            return self.fail("Upload", format!("{source} does not exist."));
        }
        let mut command = match compose::cli(&socket) {
            Ok(command) => command,
            Err(message) => return self.fail("Upload", message),
        };
        let (folder, tx, view) = (self.view.files.folder.clone(), self.tx.clone(), self.view.id);
        command.args(["cp", &source, &format!("{id}:{folder}")]);
        let message = format!("Uploaded {source} to {folder}.");
        self.spawn(format!("Upload {source}"), move |docker| {
            compose::run(command).map_err(Error::new)?;
            let _ = tx.send(Msg::Files(view, folder.clone(), docker.list_files(&id, &folder)));
            Ok(Some(message))
        });
    }

    /// Compose order: dependencies start first and stop last.
    fn start_stop(&mut self) {
        let containers = self.target_containers();
        if containers.iter().any(Container::up) {
            let mut batches = compose::start_batches(&containers.into_iter().filter(Container::up).collect::<Vec<_>>());
            batches.reverse();
            self.spawn("Stop", move |docker| run_batches(docker, batches, "stop"));
        } else if !containers.is_empty() {
            let batches = compose::start_batches(&containers);
            self.spawn("Start", move |docker| run_batches(docker, batches, "start"));
        }
    }

    fn remove(&mut self) {
        match self.focus {
            Panel::Containers | Panel::Projects => {
                let containers = self.target_containers();
                let Some(first) = containers.first() else { return };
                let running = containers.iter().filter(|container| container.up()).count();
                let project = self.selected_project().filter(|_| self.focus == Panel::Projects).map(|project| project.name.clone());
                let (networks, shared): (Vec<Network>, Vec<Network>) = self
                    .snapshot
                    .networks
                    .iter()
                    .filter(|network| network.project.is_some() && network.project == project)
                    .cloned()
                    .partition(|network| !self.snapshot.containers.iter().any(|container| container.project != project && container.networks.contains(&network.name)));
                let shared_note = if shared.is_empty() {
                    String::new()
                } else {
                    format!(" {} stays: containers outside the project use it.", shared.iter().map(|network| network.name.as_str()).collect::<Vec<_>>().join(", "))
                };
                let (title, body) = match &project {
                    Some(name) => (
                        format!("Remove compose project {name}?"),
                        format!(
                            "Removes {}{}{}. Volumes and images stay.{shared_note}",
                            count(containers.len(), "container"),
                            if networks.is_empty() { String::new() } else { format!(" and {}", count(networks.len(), "network")) },
                            if running > 0 { format!(", killing {running} running first") } else { String::new() }
                        ),
                    ),
                    None if first.up() => (format!("Remove {}?", first.name), "The container is running. It is killed, then removed. Its volumes stay.".into()),
                    None => (format!("Remove {}?", first.name), "The container is removed. Its volumes stay.".into()),
                };
                let label = format!("Remove {}", project.unwrap_or(first.name.clone()));
                self.confirm(title, body, label, move |docker| {
                    let errors: Vec<Error> = thread::scope(|scope| {
                        let removals: Vec<_> = containers.iter().map(|container| scope.spawn(|| docker.remove_container(&container.id, container.up()))).collect();
                        removals.into_iter().filter_map(|removal| removal.join().unwrap().err()).collect()
                    });
                    if let Some(error) = errors.into_iter().next() {
                        return Err(error);
                    }
                    for network in &networks {
                        docker.remove_network(&network.id)?;
                    }
                    Ok(None)
                });
            }
            Panel::Images => {
                let Some(image) = self.selected_image().cloned() else { return };
                if self.image_used(&image) {
                    return self.fail("Remove image", format!("{} is used by a container. Remove the container first.", image.reference));
                }
                let reference = image.reference.clone();
                self.confirm(format!("Remove {reference}?"), "The tag is removed. The image goes when no tag is left.".into(), format!("Remove {reference}"), move |docker| {
                    docker.remove_image(&reference).map(|_| None)
                });
            }
            Panel::Volumes => {
                let Some(volume) = self.selected_volume().cloned() else { return };
                let label = self.volume_label(&volume);
                if self.volume_used(&volume) {
                    return self.fail("Remove volume", format!("{label} is used by a container. Remove the container first."));
                }
                let name = volume.name;
                self.confirm(format!("Remove volume {label}?"), "Its data is deleted for good.".into(), format!("Remove {label}"), move |docker| docker.remove_volume(&name).map(|_| None));
            }
            Panel::Networks => {
                let Some(network) = self.selected_network().cloned() else { return };
                if network.system {
                    return self.fail("Remove network", format!("{} belongs to the runtime and cannot be removed.", network.name));
                }
                self.confirm(format!("Remove network {}?", network.name), "Docker refuses while a container is attached.".into(), format!("Remove {}", network.name), move |docker| {
                    docker.remove_network(&network.id).map(|_| None)
                });
            }
            Panel::Runtimes => {}
        }
    }

    fn prune(&mut self) {
        match self.focus {
            Panel::Images => self.confirm("Remove unused images?".into(), "Every image no container uses is removed, tagged ones too.".into(), "Prune images".into(), |docker| {
                Ok(Some(format!("Removed unused images, reclaimed {}.", stats::format_bytes(docker.prune_images()?))))
            }),
            Panel::Volumes => {
                self.confirm("Remove unused volumes?".into(), "Every anonymous volume no container uses is removed with its data. Named volumes stay.".into(), "Prune volumes".into(), |docker| {
                    Ok(Some(format!("Removed unused volumes, reclaimed {}.", stats::format_bytes(docker.prune_volumes()?))))
                })
            }
            Panel::Networks => self.confirm("Remove unused networks?".into(), "Every network no container uses is removed.".into(), "Prune networks".into(), |docker| {
                Ok(Some(format!("Removed {} unused networks.", docker.prune_networks()?)))
            }),
            _ => {}
        }
    }

    fn compose(&mut self, args: &'static [&'static str]) {
        let Some(project) = self.selected_project().cloned() else { return };
        match self.compose_command(&project, args) {
            Ok(command) => {
                let verb = args.join(" ");
                self.spawn(format!("compose {verb} {}", project.name), move |_| {
                    compose::run(command).map_err(Error::new)?;
                    Ok(Some(format!("docker compose {verb} finished for {}.", project.name)))
                });
            }
            Err(message) => self.fail("Compose", message),
        }
    }

    fn compose_command(&self, project: &Project, args: &[&str]) -> std::result::Result<Command, String> {
        let socket = self.docker.as_ref().map(|docker| docker.socket.clone()).ok_or("No Docker runtime is connected.")?;
        compose::command(&socket, project, args)
    }

    fn open_shell(&mut self) {
        let (Some(docker), Some(container)) = (self.docker.as_ref(), self.selected_container()) else { return };
        if !container.running() {
            return self.notify(format!("Start {} to open a shell.", container.name), true);
        }
        let id = container.id.clone();
        match compose::cli(&docker.socket) {
            Ok(mut command) => {
                command.args(["exec", "-it", &id, "sh", "-c", "command -v bash >/dev/null && exec bash || exec sh"]);
                // Closes the view's streams, so a busy log does not pile up while the shell runs; tick reopens it.
                self.view = View::new(String::new(), self.view.id + 1);
                self.shell = Some(command);
            }
            Err(message) => self.fail("Shell", message),
        }
    }

    fn confirm(&mut self, title: String, body: String, label: String, job: impl FnOnce(&Docker) -> Result<Option<String>> + Send + 'static) {
        self.popup = Some(Popup::Confirm { title, body, job: (label, Box::new(job)) });
    }

    fn fail(&mut self, title: &str, body: String) {
        self.popup = Some(Popup::Error { title: title.into(), body });
    }

    fn spawn(&mut self, label: impl Into<String>, job: impl FnOnce(&Docker) -> Result<Option<String>> + Send + 'static) {
        self.spawn_job(label.into(), Box::new(job));
    }

    fn spawn_job(&mut self, label: String, job: Job) {
        let Some(docker) = self.docker.clone() else { return self.notify("No Docker runtime is connected.", true) };
        self.busy.push(label.clone());
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = job(&docker);
            let _ = tx.send(Msg::Done(label, result));
        });
    }

    pub fn shell_failed(&mut self, message: String) {
        self.fail("Shell", message);
    }
}

/// Batch by batch, each batch at once; the first failing batch ends the run.
fn run_batches(docker: &Docker, batches: Vec<Vec<Container>>, action: &str) -> Result<Option<String>> {
    for batch in batches {
        let errors: Vec<Error> = thread::scope(|scope| {
            let runs: Vec<_> = batch.iter().map(|container| scope.spawn(|| docker.container_action(&container.id, action))).collect();
            runs.into_iter().filter_map(|run| run.join().unwrap().err()).collect()
        });
        if let Some(error) = errors.into_iter().next() {
            return Err(error);
        }
    }
    Ok(None)
}

/// `docker cp` replaces an existing host file without asking, so a name already taken is refused.
fn save_target(folder: &Path, name: &str) -> std::result::Result<PathBuf, String> {
    let target = folder.join(name);
    if target.symlink_metadata().is_ok() {
        return Err(format!("{} already exists. dockwatch does not overwrite it; move it or start dockwatch in another folder.", target.display()));
    }
    Ok(target)
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

fn matches(filter: &str, fields: &[&str]) -> bool {
    let filter = filter.to_lowercase();
    fields.iter().any(|field| field.to_lowercase().contains(&filter))
}

fn pretty(value: &Value) -> Vec<String> {
    serde_json::to_string_pretty(value).unwrap_or_default().lines().map(String::from).collect()
}

fn compose_file_lines(files: &[String]) -> Vec<String> {
    if files.is_empty() {
        return vec!["No compose file is known for this project.".into()];
    }
    let mut lines = Vec::new();
    for file in files {
        lines.push(format!("# {file}"));
        // A label can name any host path, `/dev/zero` or a FIFO included.
        match std::fs::metadata(file) {
            Ok(meta) if !meta.is_file() || meta.len() > PREVIEW_LIMIT => lines.push("Not a readable compose file.".into()),
            _ => match std::fs::read_to_string(file) {
                Ok(text) => lines.extend(text.lines().map(logs::strip_ansi)),
                Err(error) => lines.push(format!("Cannot read it: {error}")),
            },
        }
        lines.push(String::new());
    }
    lines
}

/// `ps` columns aligned to their widest cell; the last column, the command, runs free.
pub fn top_lines(top: &Value) -> Vec<String> {
    let cells = |row: &Value| -> Vec<String> { row.as_array().into_iter().flatten().map(|cell| cell.as_str().unwrap_or_default().to_string()).collect() };
    let mut rows = vec![cells(&top["Titles"])];
    rows.extend(top["Processes"].as_array().into_iter().flatten().map(cells));
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns).map(|column| rows.iter().filter_map(|row| row.get(column)).map(|cell| cell.chars().count()).max().unwrap_or(0)).collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (column, cell) in row.iter().enumerate() {
                if column + 1 == row.len() {
                    line += cell;
                } else {
                    line += &format!("{cell:<width$}  ", width = widths[column]);
                }
            }
            line
        })
        .collect()
}

fn history_lines(history: &Value) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for layer in history.as_array().into_iter().flatten() {
        let size = stats::format_bytes(layer["Size"].as_u64().unwrap_or(0));
        let created = stats::age(layer["Created"].as_i64().unwrap_or(0));
        let command = layer["CreatedBy"].as_str().unwrap_or_default().replace("/bin/sh -c #(nop) ", "").replace("RUN /bin/sh -c ", "RUN ").replace("/bin/sh -c ", "RUN ");
        lines.push(format!("{size:>10}  {created:>4}  {}", logs::strip_ansi(command.trim())));
    }
    Ok(lines)
}

/// Text lines of a file, or a note for binary content (a NUL in the first 8 KiB).
fn preview_lines(bytes: &[u8]) -> Vec<String> {
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return vec![format!("Binary file, {}.", stats::format_bytes(bytes.len() as u64))];
    }
    String::from_utf8_lossy(bytes).lines().map(logs::strip_ansi).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::mpsc;

    fn app_with(containers: Vec<Container>) -> App {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx, None);
        app.apply_snapshot(Snapshot { containers, ..Snapshot::default() });
        app
    }

    fn container(id: &str, state: &str) -> Container {
        Container { id: id.into(), name: id.into(), state: state.into(), ..Container::default() }
    }

    #[test]
    fn selection_follows_the_item_across_refreshes() {
        let mut app = app_with(vec![container("a", "running"), container("b", "exited"), container("c", "running")]);
        app.move_selection(1);
        assert_eq!(app.selected_container().unwrap().id, "b");
        app.apply_snapshot(Snapshot { containers: vec![container("b", "exited"), container("c", "running")], ..Snapshot::default() });
        assert_eq!(app.selected_container().unwrap().id, "b");
        app.narrow[Panel::Containers.index()] = true;
        assert_eq!(app.selected_container().unwrap().id, "c");
    }

    #[test]
    fn a_removed_item_hands_the_selection_to_its_neighbour() {
        let mut app = app_with(vec![container("a", "exited"), container("b", "exited"), container("c", "exited")]);
        app.move_selection(1);
        app.apply_snapshot(Snapshot { containers: vec![container("a", "exited"), container("c", "exited")], ..Snapshot::default() });
        assert_eq!(app.selected_container().unwrap().id, "c");
    }

    #[test]
    fn a_scrolled_up_log_stays_inside_the_capped_buffer() {
        let mut app = app_with(Vec::new());
        let line = |n: usize| LogLine::parse(&format!("2025-01-01T00:00:00.{n:09}Z line {n}"));
        app.handle(Msg::Logs(app.view.id, (0..10).map(line).collect()));
        app.view.scroll = 1;
        app.handle(Msg::Logs(app.view.id, (10..10_050).map(line).collect()));
        assert_eq!(app.view.logs.len(), 10_000);
        assert!(app.view.scroll < app.view.logs.len());
    }

    #[test]
    fn the_compose_tab_reads_the_local_file_without_a_runtime() {
        let folder = std::env::temp_dir().join(format!("dockwatch-compose-tab-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("compose.yaml");
        std::fs::write(&file, "services: {}\n").unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx, compose::local_project(&folder));
        app.focus = Panel::Projects;
        app.tabs[Panel::Projects.index()] = 2;
        app.tick();
        std::fs::remove_dir_all(&folder).unwrap();
        let Content::Lines(lines) = &app.view.content else { panic!("the Compose tab is still loading") };
        assert_eq!(lines[..2], [format!("# {}", file.display()), "services: {}".to_string()]);
    }

    #[test]
    fn the_compose_tab_skips_files_that_are_not_regular() {
        assert_eq!(compose_file_lines(&["/dev/zero".into()]), ["# /dev/zero", "Not a readable compose file.", ""]);
    }

    #[test]
    fn saving_never_overwrites_a_host_file() {
        let folder = std::env::temp_dir();
        let taken = format!("dockwatch-save-{}", std::process::id());
        std::fs::write(folder.join(&taken), "keep").unwrap();
        let refused = save_target(&folder, &taken);
        std::fs::remove_file(folder.join(&taken)).unwrap();
        assert!(refused.unwrap_err().contains("already exists"));
        assert_eq!(save_target(&folder, &taken), Ok(folder.join(&taken)));
    }

    #[test]
    fn names_anonymous_volumes_by_their_mount() {
        let hash = "21f4cc2731146696d11cb67322ff899a";
        let mut db = container("rarv-database-1", "exited");
        db.volumes = vec![crate::docker::Mount { name: hash.into(), destination: "/var/lib/postgresql/data".into() }];
        let app = app_with(vec![db]);
        let volume = |name: &str, anonymous| Volume { name: name.into(), anonymous, created_at: "2025-03-04T10:20:30Z".into(), ..Volume::default() };
        assert_eq!(app.volume_label(&volume(hash, true)), "rarv-database-1:/var/lib/postgresql/data");
        assert_eq!(app.volume_label(&volume("5f9d6d220f7ca83c4310", true)), "unused anonymous, created 2025-03-04 10:20");
        assert_eq!(app.volume_label(&volume("rarv_redis_dev_data", false)), "rarv_redis_dev_data");
    }

    #[test]
    fn names_the_containers_that_use_a_resource() {
        let mut web = container("shop-web-1", "running");
        web.networks = vec!["shop_default".into()];
        let mut db = container("shop-db-1", "exited");
        db.networks = vec!["shop_default".into()];
        let app = app_with(vec![web, db]);
        assert_eq!(app.used_by(|container| container.networks.contains(&"shop_default".to_string())), ["Used by: shop-web-1, shop-db-1", ""]);
        assert_eq!(app.used_by(|_| false), ["Used by: no container", ""]);
    }

    #[test]
    fn filters_lists_case_insensitively() {
        let mut app = app_with(vec![container("Web", "running"), container("db", "running")]);
        app.filters[Panel::Containers.index()] = "WE".into();
        let names: Vec<_> = app.containers().iter().map(|container| container.name.as_str()).collect();
        assert_eq!(names, ["Web"]);
    }

    #[test]
    fn view_key_changes_when_the_container_starts() {
        let mut app = app_with(vec![container("a", "exited")]);
        let stopped = app.view_key();
        app.apply_snapshot(Snapshot { containers: vec![container("a", "running")], ..Snapshot::default() });
        assert_ne!(app.view_key(), stopped);
    }

    #[test]
    fn aligns_top_columns() {
        let top = json!({"Titles": ["PID", "USER", "CMD"], "Processes": [["1", "root", "nginx -g daemon off;"], ["12345", "www", "nginx: worker"]]});
        assert_eq!(top_lines(&top), ["PID    USER  CMD", "1      root  nginx -g daemon off;", "12345  www   nginx: worker"]);
    }

    #[test]
    fn previews_text_and_flags_binary() {
        assert_eq!(preview_lines(b"a\nb\n"), ["a", "b"]);
        assert_eq!(preview_lines(b"\x7fELF\0\0"), ["Binary file, 6 B."]);
    }

    #[test]
    fn history_shows_one_run_for_classic_and_buildkit_layers() {
        let history = serde_json::json!([{"CreatedBy": "/bin/sh -c apk add curl"}, {"CreatedBy": "RUN /bin/sh -c apk add git # buildkit"}, {"CreatedBy": "/bin/sh -c #(nop)  CMD [\"sh\"]"}]);
        let lines = history_lines(&history).unwrap();
        assert!(lines[0].ends_with("  RUN apk add curl"), "{}", lines[0]);
        assert!(lines[1].ends_with("  RUN apk add git # buildkit"), "{}", lines[1]);
        assert!(lines[2].ends_with("  CMD [\"sh\"]"), "{}", lines[2]);
    }
}
