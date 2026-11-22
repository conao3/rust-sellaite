#!/usr/bin/env bash
set -euo pipefail

case "$1" in
    ubuntu)
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends \
            build-essential cargo ca-certificates sway grim wayvnc coreutils \
            python3 libatspi2.0-0t64 at-spi2-core novnc dbus foot fonts-dejavu-core
        dpkg-query -W sway grim wayvnc novnc rustc
        ;;
    arch)
        pacman -Syu --noconfirm --needed \
            gcc rust ca-certificates curl sway grim wayvnc coreutils \
            python at-spi2-core dbus foot ttf-dejavu
        pacman -Q sway grim wayvnc rust
        if [[ -n $(getcap /usr/bin/sway) ]]; then
            setcap -r /usr/bin/sway
        fi
        curl -fsSL -o /tmp/novnc.tar.gz https://codeload.github.com/novnc/noVNC/tar.gz/refs/tags/v1.6.0
        echo '5066103959ef4e9b10f37e5a148627360dd8414e4cf8a7db92bdbd022e728aaa  /tmp/novnc.tar.gz' | sha256sum -c -
        install -d /usr/share/novnc
        tar -xzf /tmp/novnc.tar.gz --strip-components=1 -C /usr/share/novnc
        ;;
    *)
        echo 'Expected ubuntu or arch' >&2
        exit 2
        ;;
esac

useradd --create-home smoke
runuser -u smoke -- bash -euc '
    cd "$HOME"
    CARGO_TARGET_DIR="$HOME/target" LEANFRAN_OFFLINE=1 cargo install --locked --path /src
    export PATH="$HOME/.cargo/bin:$PATH"
    export SELLAITE_ATSPI_LIBRARY=libatspi.so.0
    timeout 60s dbus-run-session -- python3 /src/tests/install/smoke.py
'
