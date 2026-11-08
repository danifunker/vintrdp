#!/usr/bin/env bash
# Build vintrdp.exe for Windows NT 4.0 with ruststdwinnt4's toolchain.
#
#   scripts/dist/build-nt4.sh --arch arm|x86 [options]
#
# arm is NT 4.0 on ARM (PINT, on the Raspberry Pi), x86 is NT 4.0 on x86 (and the
# build to try on Alpha under FX!32). The front-end is pure Rust here: no TLS
# (GnuTLS), with RDP's own encryption and sound through waveOut. ruststdwinnt4's
# cargo-nt4 builds std for NT 4.0 and checks that every import is one NT 4.0 SP0
# exports; this script checks the finished .exe again. Leaves it in --out as
# vintrdp-nt4-<arch>.exe.
#
# ruststdwinnt4 is found through RUSTSTDWINNT4=path, else ../ruststdwinnt4 next to
# this repository. Needs: rustup (cargo-nt4 uses the Rust its rust-toolchain.toml
# pins, with rust-src), Python 3, and LLVM's llvm-windres with clang (which it
# preprocesses with) for the icon and manifest.
# Runs on Linux and macOS; nothing proprietary is needed.
source "$(dirname "$0")/lib.sh"

parse_common_args "$@"
set -- ${REST[@]+"${REST[@]}"}
ARCH=""
while [ $# -gt 0 ]; do
    case "$1" in
        --arch) ARCH="${2:-}"; shift 2 ;;
        -h|--help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; common_usage; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done
case "$ARCH" in
    arm) NT4_TARGET=thumbv7a-nt4-windows-msvc ;;
    x86) NT4_TARGET=i686-nt4-windows-msvc ;;
    *) die "--arch arm or --arch x86 is required" ;;
esac
export NT4_TARGET

NT4="${RUSTSTDWINNT4:-$DIST_ROOT/../ruststdwinnt4}"
[ -x "$NT4/tools/cargo-nt4" ] || die "ruststdwinnt4 not found at $NT4 (set RUSTSTDWINNT4)"
NT4=$(cd "$NT4" && pwd)
need python3

# The pinned Rust, with the std source cargo-nt4 patches.
if command -v rustup >/dev/null 2>&1; then
    CHANNEL=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$NT4/rust-toolchain.toml")
    rustup toolchain list | grep -q "^$CHANNEL-" && rustup component list --toolchain "$CHANNEL" --installed | grep -q '^rust-src' \
        || rustup toolchain install "$CHANNEL" --profile minimal --component rust-src
fi

# build.rs compiles the icon and manifest with llvm-windres, which it finds on PATH
# (it also tries llvm-windres-NN); put a versioned LLVM's bin directory there when
# only that has the plain name.
if [ -z "${WINDRES:-}" ] && ! command -v llvm-windres >/dev/null 2>&1; then
    for d in /usr/lib/llvm-*/bin /opt/homebrew/opt/llvm/bin /usr/local/opt/llvm/bin; do
        [ -x "$d/llvm-windres" ] && { PATH="$PATH:$d"; break; }
    done
fi

EXE="$OUT_DIR/vintrdp-nt4-$ARCH.exe"
BUILT="$(release_dir "$NT4_TARGET")/vintrdp.exe"
if [ "$SKIP_BUILD" != 1 ]; then
    # cargo-nt4 points the windows crates at its NT 4.0 copies, which cargo records in
    # Cargo.lock; the repository's lock file is put back afterwards.
    LOCK=$(mktemp)
    cp Cargo.lock "$LOCK"
    trap 'cp "$LOCK" Cargo.lock; rm -f "$LOCK"' EXIT
    group_start "cargo-nt4 build vintrdp ($NT4_TARGET)"
    "$NT4/tools/cargo-nt4" build --release -p vintrdp --no-default-features --features sound
    group_end
fi
[ -f "$BUILT" ] || die "no $BUILT"

cp "$BUILT" "$EXE"
group_start "NT 4.0 import check"
python3 "$NT4/tools/peinfo.py" "$EXE" --defs "$NT4/imports/$ARCH" | grep -v '^\(sections\|relocations\) ' \
    || die "vintrdp-nt4-$ARCH.exe needs what NT 4.0 SP0 lacks"
group_end
log "done: $EXE"
