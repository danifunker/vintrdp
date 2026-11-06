#!/usr/bin/env bash
# Build and package vintrdp for Linux as deb, rpm and Arch pkg, plus a tarball.
#
#   scripts/dist/package-linux.sh [options] [--formats deb,rpm,pkg,tar]
#
# GnuTLS and friends link statically (see build-deps.sh), so the packages only
# depend on GTK 4, ALSA and the C library. Cross builds (a --target whose arch differs
# from the host) strip with <triple>-strip and read the rpm's library
# requirements with <triple>-objdump, and expect the cross linker and pkg-config
# environment to be set up already (see build.yml's riscv64 entry).
#
# Needs: cargo-deb, cargo-generate-rpm, dpkg-deb, rpm, zstd, m4.
source "$(dirname "$0")/lib.sh"

FORMATS="deb,rpm,pkg,tar"
parse_common_args "$@"
set -- ${REST[@]+"${REST[@]}"}
while [ $# -gt 0 ]; do
    case "$1" in
        --formats) FORMATS="$2"; shift 2 ;;
        -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; common_usage; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

case "$TARGET" in
    x86_64-*)  ARCH=x64;     DEB_ARCH=amd64;   PKG_ARCH=x86_64 ;;
    aarch64-*) ARCH=arm64;   DEB_ARCH=arm64;   PKG_ARCH=aarch64 ;;
    riscv64*)  ARCH=riscv64; DEB_ARCH=riscv64; PKG_ARCH=riscv64 ;;
    *) die "unsupported Linux target $TARGET" ;;
esac

CROSS=0
[ "$(host_target | cut -d- -f1)" = "${TARGET%%-*}" ] || CROSS=1
if [ "$CROSS" = 1 ]; then
    PREFIX="${TARGET%%-*}"; PREFIX="${PREFIX%gc}-linux-gnu"
    STRIP="$PREFIX-strip"; OBJDUMP="$PREFIX-objdump"
else
    STRIP="strip"; OBJDUMP="objdump"
fi

if [ "$SKIP_BUILD" = 0 ]; then
    "$DIST_ROOT/scripts/dist/build-deps.sh" --target "$TARGET"
    VINTRDP_DEPS="$DIST_ROOT/target/deps/$TARGET" build_app "$TARGET"
fi
BIN="$(release_dir "$TARGET")/vintrdp"
[ -x "$BIN" ] || die "$BIN was not built"
"$STRIP" "$BIN"

PKGVER=$(pkg_version "$VERSION")
BASE="vintrdp-linux-${ARCH}-${VERSION}"
has_format() { case ",$FORMATS," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }

if has_format deb; then
    group_start "deb"
    need cargo-deb dpkg-deb
    cargo deb -p vintrdp --target "$TARGET" --no-build --no-strip \
        --deb-version "$PKGVER" -o "$OUT_DIR/$BASE.deb"
    dpkg-deb -f "$OUT_DIR/$BASE.deb" Package Version Architecture Depends
    [ "$(dpkg-deb -f "$OUT_DIR/$BASE.deb" Architecture)" = "$DEB_ARCH" ] \
        || die "$BASE.deb is not Architecture: $DEB_ARCH"
    # cargo-deb only warns when $auto cannot resolve library deps, and then
    # ships an empty Depends.
    DEPENDS=$(dpkg-deb -f "$OUT_DIR/$BASE.deb" Depends)
    case "$DEPENDS" in *libgtk-4*) ;; *) die "deb Depends did not resolve (no libgtk-4): $DEPENDS" ;; esac
    case "$DEPENDS" in *-cross*) die "deb Depends names a cross-toolchain package: $DEPENDS" ;; esac
    group_end
fi

if has_format rpm; then
    group_start "rpm"
    need cargo-generate-rpm rpm
    META="version = \"$PKGVER\""
    RPM_ARGS=()
    if [ "$CROSS" = 1 ]; then
        # Auto-requires runs the host ldd, which cannot load a foreign ELF.
        REQS=$("$OBJDUMP" -p "$BIN" \
            | awk '$1 == "NEEDED" { printf "%s\"%s()(64bit)\" = \"*\"", sep, $2; sep = ", " }')
        [ -n "$REQS" ] || die "no NEEDED entries in $BIN"
        META="$META
requires = { $REQS }"
        RPM_ARGS+=(--auto-req disabled)
    fi
    GEN="${CARGO_TARGET_DIR:-target}/$TARGET/generate-rpm"
    rm -rf "$GEN"
    cargo generate-rpm -p crates/vintrdp --target "$TARGET" --arch "$PKG_ARCH" \
        ${RPM_ARGS[@]+"${RPM_ARGS[@]}"} --set-metadata "$META"
    mv "$GEN/"*.rpm "$OUT_DIR/$BASE.rpm"
    rpm -qp --qf '%{NAME} %{VERSION} %{ARCH}\n' "$OUT_DIR/$BASE.rpm"
    [ "$(rpm -qp --qf '%{ARCH}' "$OUT_DIR/$BASE.rpm")" = "$PKG_ARCH" ] \
        || die "$BASE.rpm is not arch $PKG_ARCH"
    group_end
fi

if has_format pkg; then
    group_start "Arch pkg"
    need zstd
    S=$(stage_dir)
    install -Dm755 "$BIN" "$S/usr/bin/vintrdp"
    for size in 16 32 48 64 128 256; do
        install -Dm644 "crates/vintrdp/assets/icons/icon-${size}.png" \
            "$S/usr/share/icons/hicolor/${size}x${size}/apps/vintrdp.png"
    done
    install -Dm644 crates/vintrdp/linux/vintrdp.desktop "$S/usr/share/applications/vintrdp.desktop"
    install -Dm644 LICENSE "$S/usr/share/licenses/vintrdp/LICENSE"
    # pacman's pkgver may not contain '-' or ':'; ours never do.
    cat > "$S/.PKGINFO" <<PKGINFO
pkgname = vintrdp
pkgbase = vintrdp
pkgver = ${VERSION}-1
pkgdesc = RDP 4 and 5 client for Windows NT 4.0 Terminal Server to Server 2003
url = https://github.com/danifunker/vintrdp
builddate = $(date -u +%s)
packager = vintrdp release builds
size = $(du -sb "$S/usr" | cut -f1)
arch = ${PKG_ARCH}
license = GPL-3.0-or-later
depend = gtk4
depend = alsa-lib
PKGINFO
    (cd "$S" && tar --zstd -cf "$OUT_DIR/$BASE.pkg.tar.zst" .PKGINFO usr)
    rm -rf "$S"
    group_end
fi

if has_format tar; then
    group_start "tarball"
    S=$(stage_dir)
    cp "$BIN" crates/vintrdp/linux/vintrdp.desktop crates/vintrdp/assets/icons/icon-256.png LICENSE "$S/"
    tar -C "$S" -czf "$OUT_DIR/$BASE.tar.gz" vintrdp vintrdp.desktop icon-256.png LICENSE
    verify_members "$OUT_DIR/$BASE.tar.gz" vintrdp LICENSE
    rm -rf "$S"
    group_end
fi

log "done:"; ls -lh "$OUT_DIR" >&2
