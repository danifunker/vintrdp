# shellcheck shell=bash disable=SC2034
# Shared helpers for the scripts in scripts/dist/. Sourced, never run.
#
# Every distribution build goes through these scripts. The GitHub workflows only
# install toolchains, call a script and upload what it left in dist/, so
# anything CI builds you can build the same way locally.

set -euo pipefail

DIST_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$DIST_ROOT"

log()  { printf '==> %s\n' "$*" >&2; }
warn() { if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::warning::$*" >&2; else printf 'warning: %s\n' "$*" >&2; fi; }
die()  { if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::error::$*" >&2; else printf 'error: %s\n' "$*" >&2; fi; exit 1; }

group_start() { if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::group::$*" >&2; else log "$*"; fi; }
group_end()   { if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::endgroup::" >&2; fi; }

need() {
    local cmd
    for cmd in "$@"; do
        command -v "$cmd" >/dev/null 2>&1 || die "'$cmd' is required but not installed"
    done
}

host_target() { rustc -vV | awk '/^host:/ {print $2}'; }

ensure_target() {
    command -v rustup >/dev/null 2>&1 || return 0
    rustup target list --installed 2>/dev/null | grep -qx "$1" && return 0
    log "adding rust target $1"
    rustup target add "$1"
}

# deb and rpm versions need a leading digit: 1.2.3 stays, dev.1a2b3c4d becomes
# 0~dev.1a2b3c4d so a real release always sorts above it.
pkg_version() {
    case "$1" in
        [0-9]*) echo "$1" ;;
        *) echo "0~$1" ;;
    esac
}

# Common option parsing. Sets VERSION, TARGET, OUT_DIR, SKIP_BUILD and leaves
# anything it does not recognise in REST.
VERSION=""
TARGET=""
OUT_DIR="$DIST_ROOT/dist"
SKIP_BUILD=0
REST=()

parse_common_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --version)    VERSION="$2"; shift 2 ;;
            --target)     TARGET="$2"; shift 2 ;;
            --out)        OUT_DIR="$2"; shift 2 ;;
            --skip-build) SKIP_BUILD=1; shift ;;
            *)            REST+=("$1"); shift ;;
        esac
    done
    [ -n "$VERSION" ] || VERSION=$("$DIST_ROOT/scripts/dist/version.sh" dev)
    [ -n "$TARGET" ]  || TARGET=$(host_target)
    case "$OUT_DIR" in /*) ;; *) OUT_DIR="$DIST_ROOT/$OUT_DIR" ;; esac
    mkdir -p "$OUT_DIR"
    # crates/vintrdp/build.rs bakes this into the About window and --version.
    export VINTRDP_VERSION="$VERSION"
}

common_usage() {
    cat <<USAGE
Common options:
  --version V    version to stamp (default: dev.<commit>)
  --target TRIPLE  rust target (default: the host)
  --out DIR      where finished files go (default: dist/)
  --skip-build   package a binary already in target/ instead of building
USAGE
}

release_dir() { echo "${CARGO_TARGET_DIR:-$DIST_ROOT/target}/$1/release"; }

build_app() {  # <target>
    [ "$SKIP_BUILD" = 1 ] && return 0
    ensure_target "$1"
    group_start "cargo build vintrdp ($1)"
    cargo build --release --locked --target "$1" -p vintrdp
    group_end
}

stage_dir() { mktemp -d "${TMPDIR:-/tmp}/vintrdp-dist.XXXXXX"; }

# Fail if an archive is missing any of the given top-level members.
verify_members() {  # <archive> <member>...
    local archive="$1"; shift
    local listing want
    case "$archive" in
        *.zip) listing=$(unzip -Z1 "$archive" 2>/dev/null || 7z l -ba -slt "$archive" | sed -n 's/^Path = //p') ;;
        *)     listing=$(tar tzf "$archive") ;;
    esac
    for want in "$@"; do
        printf '%s\n' "$listing" | grep -qx "$want" \
            || die "$(basename "$archive") is missing member '$want'"
    done
}
