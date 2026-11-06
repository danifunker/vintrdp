#!/usr/bin/env bash
# Write the GitHub release body (with the downloads table) for a version, or
# check that a directory holds every file that table links to.
#
#   scripts/dist/release-notes.sh --version V --tag TAG
#                                 [--repo owner/name] [--commit SHA] [-o FILE]
#   scripts/dist/release-notes.sh --version V --check DIR
#
# The asset list below is the single source of truth for what a release ships;
# build.yml checks the built files against it before anything is published.
source "$(dirname "$0")/lib.sh"

TAG=""; REPO="${GITHUB_REPOSITORY:-danifunker/vintrdp}"; COMMIT=""; OUT=""; CHECK=""
while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --tag)     TAG="$2"; shift 2 ;;
        --repo)    REPO="$2"; shift 2 ;;
        --commit)  COMMIT="$2"; shift 2 ;;
        -o)        OUT="$2"; shift 2 ;;
        --check)   CHECK="$2"; shift 2 ;;
        -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done
[ -n "$VERSION" ] || die "--version is required"
V="$VERSION"

assets() {
    local a
    for a in x86 x64 arm64; do
        echo "vintrdp-Setup-windows-${a}-${V}.exe"
        echo "vintrdp-windows-${a}-${V}.zip"
    done
    echo "vintrdp-macos-universal-${V}.dmg"
    echo "vintrdp-macos-universal-${V}.zip"
    for a in x64 arm64 riscv64; do
        [ "$a" = riscv64 ] || echo "vintrdp-linux-${a}-${V}.AppImage"
        echo "vintrdp-linux-${a}-${V}.deb"
        echo "vintrdp-linux-${a}-${V}.rpm"
        echo "vintrdp-linux-${a}-${V}.pkg.tar.zst"
        echo "vintrdp-linux-${a}-${V}.tar.gz"
    done
}

if [ -n "$CHECK" ]; then
    missing=0
    while read -r f; do
        if [ ! -s "$CHECK/$f" ]; then
            echo "missing: $f" >&2
            missing=$((missing + 1))
        fi
    done < <(assets)
    [ "$missing" = 0 ] || die "$missing expected release file(s) missing from $CHECK"
    log "all $(assets | wc -l | tr -d ' ') release files present"
    exit 0
fi

[ -n "$TAG" ] || die "--tag is required"
D="https://github.com/${REPO}/releases/download/${TAG}"
dl() { echo "[$1](${D}/$2)"; }
win() { echo "$(dl Installer "vintrdp-Setup-windows-$1-${V}.exe") · $(dl "Portable zip" "vintrdp-windows-$1-${V}.zip")"; }
lin() { [ "$1" = riscv64 ] || printf '%s · ' "$(dl AppImage "vintrdp-linux-$1-${V}.AppImage")"; echo "$(dl deb "vintrdp-linux-$1-${V}.deb") · $(dl rpm "vintrdp-linux-$1-${V}.rpm") · $(dl "Arch pkg" "vintrdp-linux-$1-${V}.pkg.tar.zst") · $(dl tar.gz "vintrdp-linux-$1-${V}.tar.gz")"; }

{
echo "**Version \`${V}\`**${COMMIT:+, built from [\`${COMMIT:0:8}\`](https://github.com/${REPO}/commit/${COMMIT})}"
cat <<NOTES

## Downloads

**Most people want one of these:**

- **Windows:** $(dl "64-bit installer" "vintrdp-Setup-windows-x64-${V}.exe")
- **macOS:** $(dl "dmg" "vintrdp-macos-universal-${V}.dmg") (one download for Apple silicon and Intel Macs)
- **Linux:** $(dl "64-bit AppImage" "vintrdp-linux-x64-${V}.AppImage") (runs on any distro, nothing to install)

**Everything else:**

| Platform | Files |
|---|---|
| Windows 10+ x64 | $(win x64) |
| Windows 10+ x86 (32-bit) | $(win x86) |
| Windows 11 ARM64 | $(win arm64) |
| macOS 13+, Apple silicon and Intel | $(dl dmg "vintrdp-macos-universal-${V}.dmg") · $(dl zip "vintrdp-macos-universal-${V}.zip") |
| Linux x64 | $(lin x64) |
| Linux arm64 | $(lin arm64) |
| Linux riscv64 | $(lin riscv64) |

Windows on an ARM PC such as a Surface or Snapdragon laptop? Take ARM64. Raspberry Pi and other ARM Linux boards: Linux arm64. Prefer your package manager on Linux? Take the deb (Debian, Ubuntu), rpm (Fedora, openSUSE) or Arch pkg.
NOTES
} > "${OUT:-/dev/stdout}"
