# Dockwatch

A terminal UI for Docker and Docker Compose, in the spirit of [lazydocker](https://github.com/jesseduffield/lazydocker). Written in Rust with ratatui.

It finds your Docker runtime on its own and talks to the Docker Engine API over its Unix socket. Compose commands (`up`, `down`, `pull`, `build`) and container shells run the `docker` CLI against the connected socket.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/rokartur/dockwatch/main/install.sh | sh
dockwatch
```

A release build checks GitHub on every start and installs a newer release in the background; it runs from the next start. `dockwatch update` does the same at once. A build from source (`cargo install --path .`) never updates itself.

Release: bump `version` in `Cargo.toml`, then push a tag `v<version>`. The release workflow builds macOS and Linux binaries and publishes them.

Start it in a folder with a `compose.yaml` and that project shows in the Projects panel before it has containers, so `u` can create it.

Development (needs Rust 1.88 or newer, `rustup update`):

```sh
cargo run
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Runtimes

Dockwatch checks these sources, in this order:

1. `DOCKER_HOST`, when it is a `unix://` socket.
2. Every Docker context in `~/.docker/contexts`. Docker Desktop, OrbStack, Colima, Rancher Desktop, Lima and Lighter all register one.
3. Known socket paths, for a runtime without a context:
   - Docker Desktop: `~/.docker/run/docker.sock`
   - OrbStack: `~/.orbstack/run/docker.sock`
   - Lighter: `~/.lighter/docker.sock`
   - Colima: `~/.colima/default/docker.sock`
   - Rancher Desktop: `~/.rd/docker.sock`
   - Lima: `~/.lima/docker/sock/docker.sock`
   - `/var/run/docker.sock`

Sockets that resolve to the same file are shown once. Dockwatch pings every runtime every 5 seconds. On start it connects to the runtime you last switched to, else to the current Docker context (`DOCKER_CONTEXT` wins over `config.json`, and `DOCKER_CONFIG` moves `~/.docker`), else to the first runtime that is running. When that runtime stops, Dockwatch connects again when it is back. Remote contexts (`ssh://`, `tcp://`) are skipped, and a non-Unix `DOCKER_HOST` gets a notice on start.

The chosen runtime and the `a` toggles are kept in `~/.config/dockwatch/settings.json`. Set `DOCKWATCH_DOCKER` to the `docker` CLI when it is not on `PATH` or in a known folder.

## Panels

The left side has six panels: Runtimes, Projects, Containers, Images, Volumes and Networks. The right side shows the selection, in tabs:

- Runtimes: engine version, system and object counts.
- Projects: merged logs of all services, a services table with CPU and memory, the compose files.
- Containers: logs, live stats, env, inspect, processes, files with a text preview. In Files, `S` saves the selection to the current directory (never over an existing file) and `U` uploads a host path into the open folder, both through `docker cp`.
- Images: inspect, layer history. Volumes and networks: inspect. Inspect of an image, volume or network starts with the containers that use it.

## Keys

| Key | Action |
| --- | --- |
| `Tab`, `Shift-Tab`, `1`-`6` | Next or previous panel, or jump to one |
| `j` `k`, arrows, `g` `G`, `Home` `End`, `PgUp` `PgDn` | Move in the list or scroll the main panel. `PgUp` `PgDn` scroll the file preview in Files |
| `Enter`, `l`, `Right` | Focus the main panel, open a folder or file. `Enter` in Runtimes connects |
| `Esc`, `h`, `Left` | Back to the list. `Esc` in a list clears the filter. `h` goes up a folder in Files, to the previous panel in a list. `Backspace` also goes up a folder in Files |
| `[` `]` | Previous or next tab |
| `/` | Filter the list, or the lines of the main panel when it has focus. `Enter` keeps it, `Esc` clears it. |
| `u` `D` `P` `b` | Projects: `docker compose up -d`, `down`, `pull`, `build`. `P` on Images pulls an image by name with `docker pull` |
| `s` `r` | Start or stop, restart. On a project, in `depends_on` order |
| `p` `e` | Containers: pause or unpause, open a shell |
| `S` `U` | Files: save the selection here, upload a host path |
| `d` | Remove the selection. A project removes its containers and networks, its volumes stay |
| `X` | Prune unused images, volumes or networks |
| `y` `n` | Confirm or cancel a removal, prune or `down` |
| `a` | Show only running containers, or only unused images, volumes, networks |
| `t` | Show or hide log timestamps |
| `R` | Rescan runtimes |
| `x` `?` | Commands of the panel |
| `q`, `Ctrl-C` | Quit. `q` waits while a job runs; `Ctrl-C` does not |
