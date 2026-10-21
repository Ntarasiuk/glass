#!/bin/sh
# Glass installer: https://glassdesk.dev
#
#   curl -fsSL glassdesk.dev | sh
#
# Installs to ~/.glass, links `glass` into ~/.local/bin, then starts `glass setup`.
# Re-run any time to update.
set -eu

BASE="${GLASS_BASE:-https://glassdesk.dev}"
PREFIX="$HOME/.glass"
BIN_DIR="$HOME/.local/bin"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
info() { printf '  \033[36m›\033[0m %s\n' "$*"; }
fail() { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }

echo
bold "Glass: your computer, in any browser"
echo

case "$(uname -s)-$(uname -m)" in
  Darwin-*)
    major=$(sw_vers -productVersion | cut -d. -f1)
    [ "$major" -ge 13 ] || fail "Glass needs macOS 13 (Ventura) or newer; this Mac has $(sw_vers -productVersion)."
    TARBALL="glass-latest-darwin-universal.tar.gz"
    ;;
  Linux-x86_64)
    [ -n "${WAYLAND_DISPLAY:-}" ] || info "Glass on Linux needs a Wayland session (Hyprland, GNOME, KDE…); run setup from inside it."
    TARBALL="glass-latest-linux-x86_64.tar.gz"
    ;;
  Linux-*) fail "Glass on Linux currently supports x86_64; this machine is $(uname -m)." ;;
  *) fail "Glass runs on macOS and Linux. Install it on the computer you want to control." ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

info "Downloading"
curl -fsSL "$BASE/releases/$TARBALL" -o "$tmp/glass.tar.gz" || fail "Download failed. Check your connection and try again."
curl -fsSL "$BASE/releases/$TARBALL.sha256" -o "$tmp/glass.sha256" || fail "Couldn't fetch the checksum."
expected=$(cut -d' ' -f1 < "$tmp/glass.sha256")
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/glass.tar.gz" | cut -d' ' -f1)
else
  actual=$(shasum -a 256 "$tmp/glass.tar.gz" | cut -d' ' -f1)
fi
[ "$expected" = "$actual" ] || fail "Checksum mismatch: the download is corrupt or was tampered with."

info "Installing to ~/.glass"
tar -xzf "$tmp/glass.tar.gz" -C "$tmp"
rm -rf "$PREFIX.new"
mv "$tmp/glass" "$PREFIX.new"
rm -rf "$PREFIX"
mv "$PREFIX.new" "$PREFIX"

mkdir -p "$BIN_DIR"
ln -sf "$PREFIX/bin/glass" "$BIN_DIR/glass"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    case "${SHELL##*/}-$(uname -s)" in
      bash-Darwin) shell_rc="$HOME/.bash_profile" ;;
      bash-*) shell_rc="$HOME/.bashrc" ;;
      fish-*) shell_rc="$HOME/.config/fish/config.fish" ;;
      *) shell_rc="$HOME/.zshrc" ;;
    esac
    line='export PATH="$HOME/.local/bin:$PATH"'
    [ "${shell_rc##*.}" = "fish" ] && line='fish_add_path $HOME/.local/bin'
    if ! grep -qs '.local/bin' "$shell_rc"; then
      mkdir -p "$(dirname "$shell_rc")"
      printf '\n# Added by the Glass installer\n%s\n' "$line" >> "$shell_rc"
      info "Added ~/.local/bin to your PATH in ${shell_rc##*/}"
    fi
    ;;
esac

version=$("$PREFIX/bin/glass" --version | cut -d' ' -f2)
info "Installed Glass $version"

# An update replaces the binary; restart the background service onto it.
if [ -f "$HOME/Library/LaunchAgents/dev.glassdesk.host.plist" ]; then
  launchctl kickstart -k "gui/$(id -u)/dev.glassdesk.host" 2>/dev/null && info "Restarted the Glass service"
elif [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/glass.service" ]; then
  systemctl --user restart glass 2>/dev/null && info "Restarted the Glass service"
fi

echo
# Piped from curl, stdin is this script, so the interactive setup reads the terminal.
if (exec < /dev/tty) 2>/dev/null; then
  exec "$PREFIX/bin/glass" setup < /dev/tty > /dev/tty 2>&1
else
  bold "Next: run  glass setup"
fi
