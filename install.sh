#!/usr/bin/env sh
# harness installer — wraps the iii engine prerequisite + builds harness binaries.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/iii-experimental/harness/main/install.sh | sh
#
# What this does (in order):
#   1. Checks / installs the iii engine via install.iii.dev.
#   2. Verifies cargo + git are present.
#   3. Clones (or updates) the harness repo to ~/.harness/repo.
#   4. Builds harness, harness-tui, and harnessd in release mode.
#   5. Symlinks the binaries into ~/.local/bin.
#   6. Prints a one-line how-to-run.
#
# Override behaviour:
#   HARNESS_REPO_DIR   — clone target (default ~/.harness/repo)
#   HARNESS_BIN_DIR    — symlink target (default ~/.local/bin)
#   HARNESS_BRANCH     — branch to install from (default main)
#   HARNESS_SKIP_III=1 — assume iii is already installed, don't try to install
#
# The script is idempotent. Re-running pulls the latest main and rebuilds.

set -eu

REPO_URL="https://github.com/iii-experimental/harness.git"
REPO_DIR="${HARNESS_REPO_DIR:-$HOME/.harness/repo}"
BIN_DIR="${HARNESS_BIN_DIR:-$HOME/.local/bin}"
BRANCH="${HARNESS_BRANCH:-main}"

log() { printf '\033[1;36m[harness install]\033[0m %s\n' "$*"; }
err() { printf '\033[1;31m[harness install]\033[0m %s\n' "$*" >&2; }

require() {
    if ! command -v "$1" >/dev/null 2>&1; then
        err "missing required command: $1"
        err "$2"
        exit 1
    fi
}

# Step 1 — iii engine prerequisite
if [ "${HARNESS_SKIP_III:-0}" = "1" ]; then
    log "skipping iii install (HARNESS_SKIP_III=1)"
elif command -v iii >/dev/null 2>&1; then
    log "iii engine already installed at $(command -v iii)"
else
    log "installing iii engine via install.iii.dev"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL https://install.iii.dev/iii/main/install.sh | sh
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- https://install.iii.dev/iii/main/install.sh | sh
    else
        err "neither curl nor wget found; install one and retry"
        exit 1
    fi
fi

# Step 2 — toolchain
require git "install git from https://git-scm.com/downloads"
require cargo "install rustup from https://rustup.rs"

# Step 3 — clone or update
mkdir -p "$(dirname "$REPO_DIR")"
if [ -d "$REPO_DIR/.git" ]; then
    log "updating $REPO_DIR"
    # Reset to FETCH_HEAD rather than origin/$BRANCH so re-runs with a
    # different HARNESS_BRANCH work on shallow clones (where the
    # remote-tracking ref for the new branch may not yet exist locally).
    git -C "$REPO_DIR" fetch --depth 1 origin "$BRANCH"
    git -C "$REPO_DIR" reset --hard FETCH_HEAD
else
    log "cloning $REPO_URL @ $BRANCH into $REPO_DIR"
    git clone --depth 1 --branch "$BRANCH" "$REPO_URL" "$REPO_DIR"
fi

# Step 4 — build
log "building harness, harness-tui, harnessd (release mode; this can take a few minutes)"
cd "$REPO_DIR"
cargo build --release --bin harness --bin harness-tui --bin harnessd

# Step 5 — symlink
mkdir -p "$BIN_DIR"
for bin in harness harness-tui harnessd; do
    ln -sf "$REPO_DIR/target/release/$bin" "$BIN_DIR/$bin"
    log "linked $bin → $BIN_DIR/$bin"
done

# Step 6 — final summary
log "done."
echo
echo "  Add $BIN_DIR to your PATH if it isn't already."
echo "  Then:"
echo "    iii --use-default-config &"
echo "    export ANTHROPIC_API_KEY=sk-ant-..."
echo "    harness 'summarise this repo and list workspace crates using ls.'"
echo
