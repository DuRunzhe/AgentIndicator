#!/usr/bin/env bash
# Functional end-to-end test for the Linux X11 focus path, entirely inside a
# container — no Linux machine or desktop required.
#
# The stack mirrors what focus() expects on a real desktop: Xvfb as the X11
# display and openbox as the window manager (EWMH window activation needs a
# WM to honour the request). An xterm hosts a fake agent process and the
# harness must bring that exact window to the foreground through the same
# xdotool calls focus_linux() makes. The Wayland guard is exercised by faking
# WAYLAND_DISPLAY and asserting the graceful fallback.
#
# Runs natively on Apple Silicon via any container runtime providing a
# `docker` CLI (OrbStack, colima, Docker Desktop). The first run pulls the
# base image and installs packages, so it takes a few minutes.
#
# Usage: bash scripts/test-linux-focus.sh [image]   (default: rust:1-slim)
set -euo pipefail

IMAGE="${1:-rust:1-slim}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

command -v docker >/dev/null 2>&1 || {
    echo "error: a 'docker' CLI is required (OrbStack, colima or Docker Desktop)" >&2
    exit 1
}

docker run --rm -i \
    --volume "$REPO_ROOT:/work" \
    "$IMAGE" bash -s <<'INNER'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

echo "== installing test desktop: Xvfb + openbox + xdotool + xterm =="
apt-get update -qq
apt-get install -y -qq xvfb openbox xdotool xterm xfonts-base procps >/dev/null

echo "== starting virtual display =="
Xvfb :99 -screen 0 1280x1024x24 >/dev/null 2>&1 &
export DISPLAY=:99
openbox >/dev/null 2>&1 &
sleep 1

echo "== compiling the focus harness against this checkout =="
mkdir /harness
cat > /harness/main.rs <<'RS'
mod focus {
    include!("/work/src/focus.rs");
}

fn main() {
    let pid: u32 = std::env::args()
        .nth(1)
        .expect("usage: asi-focus-e2e <pid>")
        .parse()
        .expect("pid must be a number");
    let focused = focus::focus(pid);
    std::fs::write("/tmp/focused", focused.to_string()).unwrap();
    println!("focus({pid}) = {focused}");
}
RS
rustc --edition 2021 -O -o /harness/asi-focus-e2e /harness/main.rs

echo "== hosting a fake agent inside xterm =="
xterm -title asi-focus-e2e -e sleep 600 &
sleep 1
agent_pid=$(pgrep -f '^sleep 600$' | head -n1)
test -n "$agent_pid" && echo "fake agent pid: $agent_pid"
expected_window=$(xdotool search --onlyvisible --name '^asi-focus-e2e$' | head -n1)
test -n "$expected_window"

echo "== x11 path: focus the terminal-emulator ancestor =="
/harness/asi-focus-e2e "$agent_pid"
test "$(cat /tmp/focused)" = "true"
sleep 0.5
active_window=$(xdotool getactivewindow)
echo "active window: $active_window, expected: $expected_window"
test "$active_window" = "$expected_window"

echo "== wayland guard: fall back without touching xdotool =="
WAYLAND_DISPLAY=wayland-0 /harness/asi-focus-e2e "$agent_pid"
test "$(cat /tmp/focused)" = "false"

echo ""
echo "linux focus e2e: PASS"
INNER
