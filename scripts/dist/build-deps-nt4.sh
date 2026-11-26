#!/usr/bin/env bash
# Build vintrdp's C dependencies (GMP, Nettle, libtasn1, GNU libiconv, GnuTLS) as static
# libraries for Windows NT 4.0, with ruststdwinnt4's toolchain, for the `tls` feature.
#
#   scripts/dist/build-deps-nt4.sh --arch arm|x86 [--prefix DIR]
#
# ARM (PINT) is compiled by Microsoft C for ARM under Wine, in the sandbox of PINT's SDK
# (ruststdwinnt4's tools/pint-sdk); x86 by clang on the host (tools/x86), with the SDK's
# NT 4.0 headers. Both need the SDK fetched and Microsoft C installed:
#
#   ../ruststdwinnt4/tools/pint-sdk fetch                         # the tenox7/pint40sdk image
#   ACCEPT_MSVC_LICENSE=yes ../ruststdwinnt4/tools/pint-sdk msvc-setup
#
# (the second accepts the Visual Studio Build Tools license: Microsoft C is downloaded from
# Microsoft). x86 also needs clang (clang-18, or NT4_CLANG). The SDK is only needed here:
# DIR then holds everything vintrdp links, ruststdwinnt4's C99 layer (lib/nt4c99.lib)
# included, and nothing of Microsoft's. Build vintrdp against it with
#   VINTRDP_DEPS=DIR scripts/dist/build-nt4.sh --arch ARCH --tls
#
# NT4_ROOT names the ruststdwinnt4 checkout (default ../ruststdwinnt4). DIR defaults to
# target/deps/<target>. DEPS_DOWNLOADS may name a directory already holding the source
# tarballs; DEPS_WORK a directory for the build trees (default: a temporary one, removed
# after; it must be under /home, /tmp or /run/media for ARM, which the sandbox sees).
#
# The versions and checksums are build-deps.sh's, with GNU libiconv, which gnulib needs on
# Windows. What NT 4.0 needs beyond build-deps.sh:
#   - --host=arm-w64-mingw32 or i686-w64-mingw32, with CC, AR and RANLIB ruststdwinnt4's
#     GCC-style wrappers; no assembler; static only. GMP on x86 probes the assembler with
#     nm even without assembly: told its answers (gmp_cv_asm_*).
#   - GnuTLS: nt4/gnutls-3.8.13.patch (no system trust store or system keys, random
#     numbers from RtlGenRandom, its constructor in Microsoft C's initializer table, an
#     attribute macro), the Windows SDK's API headers for _WIN32_WINNT 0x0400
#     (NT4_WINAPI=winsdk: gnulib's Windows code is written for them and checks the
#     version), libtasn1 linked statically (ASN1_STATIC), socklen_t as int. Only its
#     library is built.
#   - libiconv: its library without the version resource; Nettle: its two libraries.
source "$(dirname "$0")/lib.sh"
GMP=6.3.0        GMP_SHA=a3c2b80201b89e68616f4ad30bc66aee4927c3ce50e33929ca819d5c43538898
NETTLE=3.10.2    NETTLE_SHA=fe9ff51cb1f2abb5e65a6b8c10a92da0ab5ab6eaf26e7fc2b675c45f1fb519b5
TASN1=4.21.0     TASN1_SHA=1d8a444a223cc5464240777346e125de51d8e6abf0b8bac742ac84609167dc87
ICONV=1.19.1     ICONV_SHA=9a06844846d664d7122161700c10a64480d7c7bcead3091925a785b38c91edab
GNUTLS=3.8.13    GNUTLS_SHA=ffed8ec1bf09c2426d4f14aae377de4753b53e537d685e604e99a8b16ca9c97e
GNU_MIRROR="${GNU_MIRROR:-https://ftpmirror.gnu.org}"
ARCH="" PREFIX=""
while [ $# -gt 0 ]; do
    case "$1" in
        --arch) ARCH="${2:-}"; shift 2 ;;
        --prefix) PREFIX="${2:-}"; shift 2 ;;
        -h|--help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done
case "$ARCH" in
    arm) TARGET=thumbv7a-nt4-windows-msvc HOST=arm-w64-mingw32 ;;
    x86) TARGET=i686-nt4-windows-msvc HOST=i686-w64-mingw32 ;;
    *) die "--arch arm or --arch x86 is required" ;;
esac
NT4_ROOT=$(cd "${NT4_ROOT:-$DIST_ROOT/../ruststdwinnt4}" && pwd) || die "no ruststdwinnt4 (NT4_ROOT)"
SDK="$NT4_ROOT/tools/pint-sdk"
[ -x "$SDK" ] || die "$SDK: not a ruststdwinnt4 checkout"
if [ -f "$NT4_ROOT/nt4.env" ]; then set -a; . "$NT4_ROOT/nt4.env"; set +a; fi
if [ "$ARCH" = arm ]; then W="$NT4_ROOT/tools/sdk"; else W="$NT4_ROOT/tools/x86"; fi
[ -n "$PREFIX" ] || PREFIX="$DIST_ROOT/target/deps/$TARGET"
mkdir -p "$PREFIX"; PREFIX=$(cd "$PREFIX" && pwd)
# Built again when this script, the patch, the wrappers or the C99 layer change.
STAMP="$PREFIX/.built-$(find "$0" "$DIST_ROOT/scripts/dist/nt4" "$W" "$NT4_ROOT/crt" -type f | LC_ALL=C sort | xargs cat | cksum | cut -d' ' -f1)"
if [ -f "$STAMP" ]; then log "deps for $TARGET are up to date in $PREFIX"; exit 0; fi

# The C99 layer, which the libraries are compiled against and linked with; and, for x86,
# the import libraries the wrappers link against.
"$NT4_ROOT/tools/mkimplibs.sh" "$ARCH" >/dev/null
"$NT4_ROOT/crt/build.sh" "$ARCH" >/dev/null
C99="$NT4_ROOT/target/crt/$ARCH/nt4c99.lib"
[ -f "$C99" ] || die "no C99 layer ($NT4_ROOT/crt/build.sh $ARCH)"

if [ -n "${DEPS_WORK:-}" ]; then
    SRC="$DEPS_WORK"; mkdir -p "$SRC"
else
    SRC=$(mktemp -d "${TMPDIR:-/tmp}/vintrdp-deps-nt4.XXXXXX")
    trap 'rm -rf "$SRC"' EXIT
fi
SRC=$(cd "$SRC" && pwd)
sha256() { sha256sum "$1" | cut -d' ' -f1; }
# ftpmirror.gnu.org sends each download to some mirror, which may be down or have an expired
# certificate: then GNU's own server. The sha256 check below holds for either.
gnu_main() {  # <url> <file>
    case "$1" in
        "$GNU_MIRROR"/*) curl -fsSL --retry 3 -o "$2" "https://ftp.gnu.org/gnu/${1#"$GNU_MIRROR"/}" ;;
        *) return 1 ;;
    esac
}
fetch() {  # <url> <sha256>: verify and unpack into $SRC
    local file="$SRC/$(basename "$1")"
    if [ -f "${DEPS_DOWNLOADS:-/nonexistent}/$(basename "$1")" ]; then
        cp "$DEPS_DOWNLOADS/$(basename "$1")" "$file"
    elif [ ! -f "$file" ]; then
        curl -fsSL --retry 5 --retry-all-errors -o "$file" "$1" \
            || gnu_main "$1" "$file"
    fi
    [ "$(sha256 "$file")" = "$2" ] || die "$(basename "$1"): sha256 mismatch"
    rm -rf "$SRC/$(basename "$1" | sed -E 's/\.tar\.(gz|xz)$//')"
    tar -xf "$file" -C "$SRC"
}
CONF="--host=$HOST --build=x86_64-pc-linux-gnu --prefix=$PREFIX --enable-static --disable-shared"
TOOLS="CC=$W/nt4-gcc AR=$W/nt4-ar RANLIB=$W/nt4-ranlib NM=true CC_FOR_BUILD=gcc CFLAGS=-O2"
# Microsoft's lib keeps every input open, and under Wine each costs about two file
# descriptors, held by the Wine server: GMP's 520 objects outrun the usual soft limit of
# 1024 (LNK1104, "cannot open file", about 400 in). The hard limit allows more.
ulimit -n "$(ulimit -Hn)" 2>/dev/null || ulimit -n 65536 2>/dev/null || true
JOBS=$(nproc 2>/dev/null || echo 4)
# Run a build step: in the SDK's sandbox for ARM, with one Wine server throughout; on the
# host for x86, with the wrappers' bin (libtool's `lib`) on PATH.
step() {  # <name> <dir> <commands>
    local name="$1" dir="$2"; shift 2
    group_start "$name for $TARGET"
    if [ "$ARCH" = arm ]; then
        # The Wine server goes when the step ends, failed or not: one left running keeps
        # the limits it started with, and later steps would use it.
        (cd "$dir" && "$SDK" sh -c "wineserver -p; trap 'wineserver -k' EXIT; set -e; $*") >"$dir/nt4-build.log" 2>&1
    else
        (cd "$dir" && PATH="$W/bin:$PATH" sh -c "set -e; $*") >"$dir/nt4-build.log" 2>&1
    fi || {
        tail -40 "$dir/nt4-build.log"
        # A failed configure says why only in config.log: its first failed compile, and its end.
        if [ -f "$dir/config.log" ]; then
            echo "--- config.log, the first failed test:"
            grep -m1 -B2 -A25 'exit status is [1-9]\|failed program was' "$dir/config.log" || true
            echo "--- config.log, the end:"; grep -v '^[a-zA-Z_][a-zA-Z0-9_]*=' "$dir/config.log" | tail -60
        fi
        die "$name failed (log: $dir/nt4-build.log)"
    }
    group_end
}

if [ "$ARCH" = x86 ]; then GMP_ASM="gmp_cv_asm_underscore=yes gmp_cv_asm_w32=.long"; else GMP_ASM="gmp_cv_asm_underscore=no"; fi
fetch "$GNU_MIRROR/gmp/gmp-$GMP.tar.xz" "$GMP_SHA"
step gmp "$SRC/gmp-$GMP" \
    "./configure $CONF --disable-assembly $TOOLS $GMP_ASM; make -j$JOBS; make install"

fetch "$GNU_MIRROR/nettle/nettle-$NETTLE.tar.gz" "$NETTLE_SHA"
step nettle "$SRC/nettle-$NETTLE" \
    "./configure $CONF --libdir=$PREFIX/lib --disable-documentation --disable-assembler --disable-openssl \
         $TOOLS CPPFLAGS=-I$PREFIX/include LDFLAGS=-L$PREFIX/lib;
     make -j$JOBS libnettle.a libhogweed.a; make install-headers install-static install-pkgconfig"

fetch "$GNU_MIRROR/libtasn1/libtasn1-$TASN1.tar.gz" "$TASN1_SHA"
step libtasn1 "$SRC/libtasn1-$TASN1" \
    "./configure $CONF --disable-doc --disable-valgrind-tests $TOOLS; make -j$JOBS -C lib; make -C lib install"

fetch "$GNU_MIRROR/libiconv/libiconv-$ICONV.tar.gz" "$ICONV_SHA"
step libiconv "$SRC/libiconv-$ICONV" \
    "./configure $CONF --disable-nls $TOOLS;
     make -C libcharset; make -C libcharset install-lib libdir=\$PWD/lib includedir=\$PWD/lib;
     make -j$JOBS -C lib OBJECTS_RES_yes=; make -C lib install OBJECTS_RES_yes=;
     make -C libcharset install"

fetch "https://www.gnupg.org/ftp/gcrypt/gnutls/v${GNUTLS%.*}/gnutls-$GNUTLS.tar.xz" "$GNUTLS_SHA"
patch -s -d "$SRC" -p1 < "$DIST_ROOT/scripts/dist/nt4/gnutls-$GNUTLS.patch"
step gnutls "$SRC/gnutls-$GNUTLS" \
    "export NT4_WINAPI=winsdk PKG_CONFIG_LIBDIR=$PREFIX/lib/pkgconfig PKG_CONFIG_PATH=;
     ./configure $CONF --disable-doc --disable-tools --disable-tests --disable-cxx --disable-nls \
         --disable-guile --disable-libdane --disable-hardware-acceleration --disable-full-test-suite \
         --with-included-unistring --without-p11-kit --without-idn --without-tpm --without-tpm2 \
         --without-zlib --without-brotli --without-zstd \
         $TOOLS CPPFLAGS='-I$PREFIX/include -DASN1_STATIC' LDFLAGS=-L$PREFIX/lib gl_cv_socklen_t_equiv=int;
     make -j$JOBS -C gl; make -j$JOBS -C lib; make -C lib install"

# Everything vintrdp links, in one place: the C99 layer the libraries were built with.
cp "$C99" "$PREFIX/lib/nt4c99.lib"
rm -f "$PREFIX"/.built-*
touch "$STAMP"
log "deps for $TARGET are in $PREFIX"
