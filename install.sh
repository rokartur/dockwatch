#!/bin/sh
# Installs the latest dockwatch release: curl -fsSL https://raw.githubusercontent.com/rokartur/dockwatch/main/install.sh | sh
set -eu

main() {
  case "$(uname -s)" in
    Darwin) os=macos ;;
    Linux) os=linux ;;
    *) echo "dockwatch: only macOS and Linux are supported." >&2; exit 1 ;;
  esac
  case "$(uname -m)" in
    arm64 | aarch64) arch=aarch64 ;;
    x86_64) arch=x86_64 ;;
    *) echo "dockwatch: unsupported CPU $(uname -m)." >&2; exit 1 ;;
  esac

  dir="${DOCKWATCH_INSTALL_DIR:-$HOME/.local/bin}"
  mkdir -p "$dir"
  staged="$dir/.dockwatch-install-$$"
  trap 'rm -f "$staged"' EXIT
  curl -fsSL "https://github.com/rokartur/dockwatch/releases/latest/download/dockwatch-$os-$arch" -o "$staged"
  chmod 755 "$staged"
  mv "$staged" "$dir/dockwatch"
  echo "Installed $("$dir/dockwatch" --version) to $dir/dockwatch"

  case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "Add it to PATH: export PATH=\"$dir:\$PATH\"" ;;
  esac
}

# Runs only once the whole script has arrived, so a cut-off download does nothing.
main
