#!/bin/sh
# Installs Glass with the real one-liner in a fresh Arch (Omarchy-like) container, twice
# (fresh install, then update), as a normal user. GLASS_BASE overrides the site URL.
set -eu
cd "$(dirname "$0")/../.."
base=${GLASS_BASE:-https://glassdesk.dev}
docker build --platform linux/amd64 -t glass-linux-build scripts/linux >/dev/null
docker run --rm --platform linux/amd64 -e SHELL=/bin/bash -e GLASS_BASE="$base" glass-linux-build sh -euc '
  useradd -m tester
  su tester -c "
    set -eu
    cd ~
    (curl -fsSL \"\$GLASS_BASE/install.sh\" | GLASS_BASE=\"\$GLASS_BASE\" sh) </dev/null
    test -x ~/.glass/bin/glass
    test -L ~/.local/bin/glass
    test -f ~/.glass/share/glass/web/index.html
    grep -q .local/bin ~/.bashrc
    ~/.local/bin/glass --version | grep -q \"^glass \"
    ~/.local/bin/glass status | grep -q \"Run .glass setup.\"
    # Update in place: the PATH line is not added twice.
    (curl -fsSL \"\$GLASS_BASE/install.sh\" | GLASS_BASE=\"\$GLASS_BASE\" sh) </dev/null
    test \"\$(grep -c .local/bin ~/.bashrc)\" = 1
    echo PASS: linux installer
  "
'
