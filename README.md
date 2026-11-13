# dockwatch

A terminal UI for Docker and Docker Compose on macOS and Linux, in the spirit of [lazydocker](https://github.com/jesseduffield/lazydocker).

dockwatch is one static binary. It finds your Docker runtime on its own and talks to the Docker Engine API over the local Unix socket. You do not need a daemon, an agent or a config file.

## Features

- Finds Docker Engine, Docker Desktop, OrbStack, Colima, Rancher Desktop, Lima and Lighter, and switches between them with one key.
- Compose projects: `up`, `down`, `pull` and `build`, start and stop in `depends_on` order, the merged logs of all services.
- Containers: logs, live CPU and memory, env, inspect, processes, a shell, and a file browser with save and upload.
- Images, volumes and networks: inspect with the containers that use them, remove, prune. An anonymous volume shows where it is mounted, not its hash.
- Release builds update themselves from GitHub releases.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/rokartur/dockwatch/main/install.sh | sh
```

The script installs the binary for your system to `~/.local/bin`. To use another folder, set `DOCKWATCH_INSTALL_DIR`. You can also download a binary from the [releases page](https://github.com/rokartur/dockwatch/releases): macOS and Linux, x86_64 and arm64. The Linux binaries are static and run on any distribution.

From source, with Rust 1.88 or newer:

```sh
cargo install --git https://github.com/rokartur/dockwatch
```

dockwatch needs a running Docker runtime. Compose commands, shells, `docker pull` and file save and upload also need the `docker` CLI with the compose plugin. On Linux, your user must have access to `/var/run/docker.sock`, usually through the `docker` group. If it does not, the Runtimes panel shows the error.

## Usage

```sh
dockwatch            # start the UI
dockwatch update     # install the latest release now
dockwatch --version
```

Start dockwatch in a folder with a `compose.yaml`. That project then shows in the Projects panel before it has containers, so `u` can create it.

A release build checks GitHub once a day, at start. When it finds a newer release, it installs it in the background, and the new version runs from the next start. Before it replaces the binary, it checks the download against the release's `SHA256SUMS`. A build from source never updates itself.

To choose the releases it follows, set `update_channel` in `~/.config/dockwatch/settings.json`: `latest` (the default) takes stable releases, `pre-release` also takes pre-releases, `off` stops the check at start. GitHub limits unauthenticated checks to 60 an hour per IP. With `GH_TOKEN`, `GITHUB_TOKEN` or a `gh` login, dockwatch sends that token and gets 5000.

## Panels

The left side has six panels: Runtimes, Projects, Containers, Images, Volumes and Networks. The right side shows the selected item, in tabs:

- Runtimes: engine version, system, CPU and memory use, object counts.
- Projects: the merged logs of all services, a services table with CPU and memory, the compose files.
- Containers: logs, live stats, env, inspect, processes, files with a text preview. In Files, `S` saves the selected file or folder to the current folder and never overwrites a file. `U` uploads a host path into the open folder. Both use `docker cp`.
- Images: inspect, layer history. Volumes and networks: inspect. Inspect of an image, volume or network starts with the containers that use it.

## Keys

| Key | Action |
| --- | --- |
| `Tab`, `Shift-Tab`, `1`-`6` | Next or previous panel, or jump to one |
| `j` `k`, arrows, `g` `G`, `Home` `End`, `PgUp` `PgDn` | Move in the list or scroll the main panel. `PgUp` `PgDn` scroll the file preview in Files |
| `Enter`, `l`, `Right` | Focus the main panel, open a folder or file. `Enter` in Runtimes connects |
| `Esc`, `h`, `Left` | Back to the list. `Esc` in a list clears the filter. `h` goes up a folder in Files, to the previous panel in a list. `Backspace` also goes up a folder in Files |
| `[` `]` | Previous or next tab |
| `/` | Filter the list, or the lines of the main panel when it has focus. `Enter` keeps the filter, `Esc` clears it |
| `u` `D` `P` `b` | Projects: `docker compose up -d`, `down`, `pull`, `build`. `P` on Images pulls an image by name with `docker pull` |
| `s` `r` | Start or stop, restart. On a project, in `depends_on` order |
| `p` `e` | Containers: pause or unpause, open a shell |
| `S` `U` | Files: save the selection to the current folder, upload a host path |
| `d` | Remove the selection. A project removes its containers and networks, its volumes stay |
| `X` | Prune unused images, volumes or networks |
| `y` `n` | Confirm or cancel a removal, prune or `down` |
| `a` | Show only running containers, or only unused images, volumes, networks |
| `t` | Show or hide log timestamps |
| `R` | Rescan runtimes |
| `x` `?` | Commands of the panel |
| `q`, `Ctrl-C` | Quit. `q` waits while a job runs, `Ctrl-C` does not |

## Runtimes

dockwatch looks for runtimes in this order:

1. `DOCKER_HOST`, when it is a `unix://` socket.
2. Every Docker context in `~/.docker/contexts`. Docker Desktop, OrbStack, Colima, Rancher Desktop, Lima, Lighter and rootless Docker all register one.
3. Known socket paths, for a runtime without a context:
   - Docker Desktop: `~/.docker/run/docker.sock`
   - OrbStack: `~/.orbstack/run/docker.sock`
   - Colima: `~/.colima/default/docker.sock`, `~/.config/colima/default/docker.sock`
   - Rancher Desktop: `~/.rd/docker.sock`
   - Lima: `~/.lima/docker/sock/docker.sock`
   - Lighter: `~/.lighter/docker.sock`
   - socktainer: `~/.socktainer/container.sock`, `/opt/homebrew/var/run/socktainer/.socktainer/container.sock`
   - Docker Engine: `/var/run/docker.sock`
4. On macOS, [Apple container](https://github.com/apple/container), when the `container` CLI is installed.

Sockets that resolve to the same file show once. dockwatch pings every runtime every 5 seconds. At start it connects to the runtime you last chose. If that runtime is not running, it connects to the current Docker context, and then to the first runtime that runs. When the connected runtime stops, dockwatch connects again when it is back.

dockwatch skips remote contexts (`ssh://`, `tcp://`). If `DOCKER_HOST` is not a Unix socket, dockwatch shows a notice at start.

Apple container has no Docker socket, so dockwatch drives its `container` CLI. Compose and pause are not available there, and prune removes only anonymous volumes. For the Docker API and compose, run [socktainer](https://github.com/socktainer/socktainer) and pick its runtime instead.

## Configuration

dockwatch keeps the chosen runtime and the `a` toggles in `~/.config/dockwatch/settings.json`. If `XDG_CONFIG_HOME` is set, it replaces `~/.config`. dockwatch reads these environment variables:

| Variable | Effect |
| --- | --- |
| `DOCKER_HOST` | A `unix://` socket to list first |
| `DOCKER_CONTEXT` | The current context, instead of `currentContext` in `config.json` |
| `DOCKER_CONFIG` | The Docker config folder, instead of `~/.docker` |
| `DOCKWATCH_DOCKER` | The `docker` CLI to use, when it is not on `PATH` or in a known folder |
| `DOCKWATCH_INSTALL_DIR` | The install folder for `install.sh` |

## Development

```sh
cargo run
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

To make a release, set `version` in `Cargo.toml` and push the tag `v<version>`. The release workflow builds the macOS and Linux binaries and publishes them.
