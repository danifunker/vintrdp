#!/usr/bin/env bash
# Build vintrdp as a self-contained AppImage with quick-sharun.
#
#   scripts/dist/appimage.sh [options] [--install-deps]
#
# Meant for the Arch Linux container CI uses (ghcr.io/pkgforge-dev/archlinux):
# quick-sharun collects libraries from the running system, and it writes into
# /usr, so run it as root in a throwaway container, not on your desktop.
# --install-deps installs the pacman packages it needs first.
#
# ANYLINUX_REF picks the Anylinux-AppImages revision the tools come from
# (default refs/heads/main; quick-sharun sha256-pins what it downloads itself).
source "$(dirname "$0")/lib.sh"

INSTALL_DEPS=0
parse_common_args "$@"
set -- ${REST[@]+"${REST[@]}"}
while [ $# -gt 0 ]; do
    case "$1" in
        --install-deps) INSTALL_DEPS=1; shift ;;
        -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; common_usage; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

case "$TARGET" in
    x86_64-*)  ARCH=x64 ;;
    aarch64-*) ARCH=arm64 ;;
    *) die "no AppImage for $TARGET: sharun has no build for it" ;;
esac

ANYLINUX_REF="${ANYLINUX_REF:-refs/heads/main}"
TOOLS="https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/${ANYLINUX_REF}/useful-tools"

if [ "$INSTALL_DEPS" = 1 ]; then
    group_start "pacman dependencies"
    pacman -Syu --noconfirm \
        base-devel rust clang pkgconf m4 gtk4 gnutls nettle libtasn1 gmp \
        wget curl git libxkbcommon wayland mesa vulkan-icd-loader \
        xorg-server-xvfb strace dbus file patchelf \
        desktop-file-utils shared-mime-info zsync
    group_end
fi
need wget

# Arch's rust has no rustup, so build for the host and copy from target/.
if [ "$SKIP_BUILD" = 0 ]; then
    group_start "cargo build vintrdp"
    cargo build --release --locked -p vintrdp
    group_end
fi
BIN="${CARGO_TARGET_DIR:-$DIST_ROOT/target}/release/vintrdp"

WORK=$(stage_dir)
group_start "debloated runtime packages + quick-sharun"
wget --retry-connrefused --tries=30 -q "$TOOLS/get-debloated-pkgs.sh" -O "$WORK/get-debloated-pkgs.sh"
wget --retry-connrefused --tries=30 -q "$TOOLS/quick-sharun.sh" -O "$WORK/quick-sharun"
chmod +x "$WORK/get-debloated-pkgs.sh" "$WORK/quick-sharun"
(cd "$WORK" && ./get-debloated-pkgs.sh --add-common --add-mesa --prefer-nano)
group_end

install -Dm755 "$BIN" /usr/bin/vintrdp
install -Dm644 crates/vintrdp/assets/icons/icon-256.png /usr/share/icons/hicolor/256x256/apps/vintrdp.png
install -Dm644 crates/vintrdp/linux/vintrdp.desktop /usr/share/applications/vintrdp.desktop

group_start "AppImage"
# appimagetool reads OUTPATH/OUTNAME/VERSION. Do not override APPIMAGETOOL_LINK:
# quick-sharun sha256-checks the appimagetool it ships with.
export OUTPATH="$OUT_DIR"
export OUTNAME="vintrdp-linux-${ARCH}-${VERSION}.AppImage"
export VERSION
export ICON=/usr/share/icons/hicolor/256x256/apps/vintrdp.png
export DESKTOP=/usr/share/applications/vintrdp.desktop
(cd "$WORK" && ./quick-sharun /usr/bin/vintrdp && ./quick-sharun --make-appimage)
group_end
rm -rf "$WORK"

[ -f "$OUT_DIR/$OUTNAME" ] || die "quick-sharun did not produce $OUTNAME"
# Update metadata we don't use; the release is just the AppImage.
rm -f "$OUT_DIR/appinfo" "$OUT_DIR"/*.zsync
log "done:"; ls -lh "$OUT_DIR" >&2
