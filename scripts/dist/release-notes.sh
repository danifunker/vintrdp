#!/usr/bin/env bash
# Write the GitHub release body (with the downloads, by platform) for a
# version, or check that a directory holds every file it links to.
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
# One Windows row: the installer by file name, then the portable zip.
win() { echo "| $1 | $(dl "vintrdp-Setup-windows-$2-${V}.exe" "vintrdp-Setup-windows-$2-${V}.exe") | $(dl zip "vintrdp-windows-$2-${V}.zip") |"; }
# One Linux row: a format, then a link per architecture (or a dash).
lin() {
    local label=$1 ext=$2 a row
    row="| $label |"
    for a in x64 arm64 riscv64; do
        if [ "$ext" = AppImage ] && [ "$a" = riscv64 ]; then
            row="$row — |"
        else
            row="$row $(dl download "vintrdp-linux-${a}-${V}.${ext}") |"
        fi
    done
    echo "$row"
}

{
echo "**Version \`${V}\`**${COMMIT:+, built from [\`${COMMIT:0:8}\`](https://github.com/${REPO}/commit/${COMMIT})}"
cat <<NOTES

## Downloads

**Most people want one of these:**

- **Windows:** $(dl "64-bit installer" "vintrdp-Setup-windows-x64-${V}.exe")
- **macOS:** $(dl "dmg" "vintrdp-macos-universal-${V}.dmg") (one download for Apple silicon and Intel Macs)
- **Linux:** $(dl "64-bit AppImage" "vintrdp-linux-x64-${V}.AppImage") (runs on any distro, nothing to install)

### Windows

Windows 10 and 11.

| | Installer | Portable (no install) |
|---|---|---|
$(win "**64-bit** (most PCs)" x64)
$(win "**32-bit**" x86)
$(win "**ARM** (Surface, Snapdragon laptops)" arm64)

### macOS

macOS 13 or later. One app for Apple silicon and Intel Macs.

| Disk image | Zip |
|---|---|
| $(dl "vintrdp-macos-universal-${V}.dmg" "vintrdp-macos-universal-${V}.dmg") | $(dl "vintrdp-macos-universal-${V}.zip" "vintrdp-macos-universal-${V}.zip") |

### Linux

| | x64 (most PCs) | arm64 (Raspberry Pi, ARM boards) | riscv64 |
|---|---|---|---|
$(lin "**AppImage** (any distro, nothing to install)" AppImage)
$(lin "**deb** (Debian, Ubuntu, Mint)" deb)
$(lin "**rpm** (Fedora, openSUSE)" rpm)
$(lin "**Arch package**" pkg.tar.zst)
$(lin "**tar.gz**" tar.gz)
NOTES
} > "${OUT:-/dev/stdout}"
