#!/bin/sh
# Glass installer: https://getglass.lol
#
#   curl -fsSL getglass.lol | sh
#
# Installs to ~/.glass, links `glass` into ~/.local/bin, then starts `glass setup`.
# Re-run any time to update.
set -eu

BASE="${GLASS_BASE:-https://getglass.lol}"
PREFIX="$HOME/.glass"
BIN_DIR="$HOME/.local/bin"
TARBALL="glass-latest-darwin-universal.tar.gz"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
info() { printf '  \033[36m›\033[0m %s\n' "$*"; }
fail() { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }

echo
bold "Glass: your Mac, in any browser"
echo

[ "$(uname -s)" = "Darwin" ] || fail "Glass runs on macOS. Install it on the Mac you want to control."
major=$(sw_vers -productVersion | cut -d. -f1)
[ "$major" -ge 13 ] || fail "Glass needs macOS 13 (Ventura) or newer; this Mac has $(sw_vers -productVersion)."

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

info "Downloading"
curl -fsSL "$BASE/releases/$TARBALL" -o "$tmp/glass.tar.gz" || fail "Download failed. Check your connection and try again."
curl -fsSL "$BASE/releases/$TARBALL.sha256" -o "$tmp/glass.sha256" || fail "Couldn't fetch the checksum."
expected=$(cut -d' ' -f1 < "$tmp/glass.sha256")
actual=$(shasum -a 256 "$tmp/glass.tar.gz" | cut -d' ' -f1)
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
    shell_rc="$HOME/.zshrc"
    [ "${SHELL##*/}" = "bash" ] && shell_rc="$HOME/.bash_profile"
    if ! grep -qs 'HOME/.local/bin' "$shell_rc"; then
      printf '\n# Added by the Glass installer\nexport PATH="$HOME/.local/bin:$PATH"\n' >> "$shell_rc"
      info "Added ~/.local/bin to your PATH in ${shell_rc##*/}"
    fi
    ;;
esac

version=$("$PREFIX/bin/glass" --version | cut -d' ' -f2)
info "Installed Glass $version"

# An update replaces the binary; restart the background service onto it.
if [ -f "$HOME/Library/LaunchAgents/lol.getglass.host.plist" ]; then
  launchctl kickstart -k "gui/$(id -u)/lol.getglass.host" 2>/dev/null && info "Restarted the Glass service"
fi

echo
# Piped from curl, stdin is this script, so the interactive setup reads the terminal.
if (exec < /dev/tty) 2>/dev/null; then
  exec "$PREFIX/bin/glass" setup < /dev/tty > /dev/tty 2>&1
else
  bold "Next: run  glass setup"
fi
