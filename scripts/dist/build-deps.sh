#!/usr/bin/env bash
# Build GnuTLS and the libraries under it (GMP, Nettle, libtasn1) as static
# libraries for one target, for the engine's `tls` feature. Release builds link
# these, so Windows and macOS packages ship no third-party DLLs or dylibs, and
# deb and rpm packages don't tie themselves to one distro's GnuTLS soname.
#
#   scripts/dist/build-deps.sh --target TRIPLE [--prefix DIR]
#
# TRIPLE is a Rust target: {x86_64,i686,aarch64}-pc-windows-gnullvm (needs
# llvm-mingw on PATH, see llvm-mingw.sh), {x86_64,aarch64}-apple-darwin, or
# {x86_64,aarch64,riscv64gc}-unknown-linux-gnu.
# DIR defaults to target/deps/TRIPLE. Point VINTRDP_DEPS at it when building.
#
# Needs: curl, make, m4, pkg-config, sha256sum or shasum. DEPS_DOWNLOADS may name
# a directory already holding the source tarballs.
source "$(dirname "$0")/lib.sh"

GMP=6.3.0        GMP_SHA=a3c2b80201b89e68616f4ad30bc66aee4927c3ce50e33929ca819d5c43538898
NETTLE=3.10.2    NETTLE_SHA=fe9ff51cb1f2abb5e65a6b8c10a92da0ab5ab6eaf26e7fc2b675c45f1fb519b5
TASN1=4.21.0     TASN1_SHA=1d8a444a223cc5464240777346e125de51d8e6abf0b8bac742ac84609167dc87
GNUTLS=3.8.13    GNUTLS_SHA=ffed8ec1bf09c2426d4f14aae377de4753b53e537d685e604e99a8b16ca9c97e
GNU_MIRROR="${GNU_MIRROR:-https://ftpmirror.gnu.org}"

TARGET=""; PREFIX=""
while [ $# -gt 0 ]; do
    case "$1" in
        --target) TARGET="$2"; shift 2 ;;
        --prefix) PREFIX="$2"; shift 2 ;;
        -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done
[ -n "$TARGET" ] || die "--target is required"
[ -n "$PREFIX" ] || PREFIX="$DIST_ROOT/target/deps/$TARGET"
mkdir -p "$PREFIX"; PREFIX=$(cd "$PREFIX" && pwd)
STAMP="$PREFIX/.built-$(cat "$0" | cksum | cut -d' ' -f1)"
if [ -f "$STAMP" ]; then log "deps for $TARGET are up to date in $PREFIX"; exit 0; fi

JOBS=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)
CFLAGS_EXTRA=""
case "$TARGET" in
    *-pc-windows-gnullvm)
        HOST="${TARGET%%-*}-w64-mingw32"
        need "$HOST-clang"
        export CC="$HOST-clang" CXX="$HOST-clang++" AR=llvm-ar RANLIB=llvm-ranlib NM=llvm-nm STRIP=llvm-strip
        ;;
    *-apple-darwin)
        arch=${TARGET%%-*}; [ "$arch" = aarch64 ] && arch=arm64
        HOST="$TARGET"
        DEPLOY="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
        export MACOSX_DEPLOYMENT_TARGET="$DEPLOY"
        export CC="clang -arch $arch" CXX="clang++ -arch $arch"
        CFLAGS_EXTRA="-mmacosx-version-min=$DEPLOY"
        ;;
    *-linux-gnu)
        arch=${TARGET%%-*}; arch=${arch%gc}
        HOST="$arch-linux-gnu"
        if [ "$arch" != "$(uname -m)" ]; then
            export CC="$HOST-gcc" CXX="$HOST-g++" AR="$HOST-ar" RANLIB="$HOST-ranlib"
        fi
        CFLAGS_EXTRA="-fPIC"
        ;;
    *) die "build-deps.sh does not handle $TARGET" ;;
esac
export CFLAGS="-O2 $CFLAGS_EXTRA" CPPFLAGS="-I$PREFIX/include" LDFLAGS="-L$PREFIX/lib $CFLAGS_EXTRA"
# Only our own .pc files: a host library must never satisfy a target's check.
export PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig" PKG_CONFIG_PATH=""

SRC=$(mktemp -d "${TMPDIR:-/tmp}/vintrdp-deps.XXXXXX")
trap 'rm -rf "$SRC"' EXIT

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

fetch() {  # <url> <sha256>: verify and unpack into $SRC
    local file
    file="$SRC/$(basename "$1")"
    if [ -f "${DEPS_DOWNLOADS:-/nonexistent}/$(basename "$1")" ]; then
        cp "$DEPS_DOWNLOADS/$(basename "$1")" "$file"
    else
        curl -fsSL --retry 5 --retry-all-errors -o "$file" "$1"
    fi
    local got; got=$(sha256 "$file")
    [ "$got" = "$2" ] || die "$(basename "$1"): sha256 is $got, expected $2"
    tar -xf "$file" -C "$SRC"
}

build() {  # <name> <dir> <configure args...>
    local name="$1" dir="$2"; shift 2
    group_start "$name for $TARGET"
    (
        cd "$dir" || exit 1
        ./configure --host="$HOST" --prefix="$PREFIX" --enable-static --disable-shared "$@" >configure.out 2>&1 \
            || { tail -40 config.log; die "$name: configure failed"; }
        make -j"$JOBS" >make.out 2>&1 || { tail -40 make.out; die "$name: make failed"; }
        make install >/dev/null 2>&1 || make install
    )
    group_end
}

fetch "$GNU_MIRROR/gmp/gmp-$GMP.tar.xz" "$GMP_SHA"
build gmp "$SRC/gmp-$GMP" --disable-assembly CC_FOR_BUILD="${CC_FOR_BUILD:-cc}"
# libtool sees clang-for-Windows and names it the MSVC way.
[ ! -f "$PREFIX/lib/gmp.lib" ] || cp "$PREFIX/lib/gmp.lib" "$PREFIX/lib/libgmp.a"

fetch "$GNU_MIRROR/nettle/nettle-$NETTLE.tar.gz" "$NETTLE_SHA"
build nettle "$SRC/nettle-$NETTLE" --disable-documentation --disable-assembler --disable-openssl \
    --libdir="$PREFIX/lib" CC_FOR_BUILD="${CC_FOR_BUILD:-cc}"

fetch "$GNU_MIRROR/libtasn1/libtasn1-$TASN1.tar.gz" "$TASN1_SHA"
build libtasn1 "$SRC/libtasn1-$TASN1" --disable-doc --disable-valgrind-tests

# GnuTLS's own code never calls iconv (it converts with the included
# libunistring), and without NLS nothing else does: tell configure not to look,
# so a stray libiconv can't end up in its link line.
fetch "https://www.gnupg.org/ftp/gcrypt/gnutls/v${GNUTLS%.*}/gnutls-$GNUTLS.tar.xz" "$GNUTLS_SHA"
build gnutls "$SRC/gnutls-$GNUTLS" --disable-doc --disable-tools --disable-tests --disable-cxx --disable-nls \
    --disable-guile --disable-libdane --disable-hardware-acceleration --disable-full-test-suite \
    --with-included-unistring --without-p11-kit --without-idn --without-tpm --without-tpm2 \
    --without-zlib --without-brotli --without-zstd \
    am_cv_func_iconv=no am_cv_lib_iconv=no

rm -f "$PREFIX"/.built-*
touch "$STAMP"
log "deps for $TARGET are in $PREFIX"
