#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Guest side of the inotify probe (runs as the default user, sudo for
# installs). The watcher sources are read from the mirrored home share.
#
#   guest.sh setup           install inotify-tools, watchdog, watchfiles,
#                            chokidar 4 and Go fsnotify into /opt/w
#   guest.sh start ROOT      (re)start the five watchers on ROOT, logs in /tmp/w
#   guest.sh stop
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
w=$here/watchers

case $1 in
setup)
    export DEBIAN_FRONTEND=noninteractive
    sudo -E apt-get update -qq
    sudo -E apt-get install -y -qq inotify-tools python3-watchdog python3-pip nodejs npm golang-go >/dev/null
    sudo pip3 install -q --break-system-packages --ignore-installed typing_extensions watchfiles
    sudo mkdir -p /opt/w && sudo chown "$USER" /opt/w
    cd /opt/w
    [[ -f package.json ]] || npm init -y >/dev/null
    npm install -s chokidar@4
    cp "$w/ck.mjs" /opt/w/
    mkdir -p gofs && cd gofs
    [[ -f go.mod ]] || go mod init gofs >/dev/null 2>&1
    cp "$w/gofs.go" main.go
    go get github.com/fsnotify/fsnotify@v1.8.0 >/dev/null 2>&1
    go build -o /opt/w/gofs-watch .
    ;;
start)
    root=$2
    "$0" stop
    mkdir -p /tmp/w
    cd /opt/w
    (inotifywait -m -r -q --format '%e %w%f' "$root" | python3 -u "$w/stamp.py" inotifywait) >/tmp/w/inotifywait.log 2>&1 &
    python3 -u "$w/wf.py" "$root" >/tmp/w/watchfiles.log 2>&1 &
    python3 -u "$w/wd.py" "$root" >/tmp/w/watchdog.log 2>&1 &
    node /opt/w/ck.mjs "$root" >/tmp/w/chokidar.log 2>&1 &
    /opt/w/gofs-watch "$root" >/tmp/w/fsnotify.log 2>&1 &
    disown -a
    sleep 2
    ;;
stop)
    pkill -f 'inotifywait -m -r' || true
    pkill -f "$w/wf.py" || true
    pkill -f "$w/wd.py" || true
    pkill -f /opt/w/ck.mjs || true
    pkill -f /opt/w/gofs-watch || true
    ;;
esac
