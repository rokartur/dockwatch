# Dockwatch

A native macOS app to inspect Docker containers, in the spirit of [lazydocker](https://github.com/jesseduffield/lazydocker). Built with Swift and AppKit.

It finds your Docker runtime on its own and talks to the Docker Engine API over its Unix socket. You do not need the `docker` CLI.

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

Sockets that resolve to the same file are shown once. Dockwatch pings every runtime every 5 seconds. On start it connects to the current Docker context, or to the first runtime that is running. When that runtime stops, Dockwatch connects again when it is back. Remote contexts (`ssh://`, `tcp://`) are skipped.

## Keys

| Key | Action |
| --- | --- |
| Arrows | Next or previous row |
| `Return` | Open logs, stats, inspect and env |
| `Esc` | Back to the list |
| `⌘1`-`⌘9` | Switch runtime |
| `⌘K` | Command palette |
| `⌘R` | Rescan runtimes |

## Build

```sh
swift run
swift test
```

Release (universal binary, Developer ID signature, notarized DMG):

```sh
NOTARY_PROFILE=<notarytool profile> scripts/release.sh
```

The app icon is an Icon Composer document, `packaging/Dockwatch.icon` (open it with Icon Composer from Xcode 26+). `scripts/bundle.sh` compiles it with `actool`.
