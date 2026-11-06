#!/usr/bin/env bash
# Download llvm-mingw, the clang-based MinGW toolchain that builds all three
# Windows targets (x86, x64, ARM64) from a Linux host, and print its bin directory.
#
#   PATH="$(scripts/dist/llvm-mingw.sh [DIR]):$PATH"
#
# DIR defaults to target/llvm-mingw. Nothing is downloaded if it's already there.
source "$(dirname "$0")/lib.sh"

VERSION=20261104
case "$(uname -m)" in
    x86_64)        HOST=x86_64;  SHA=919380403de87dca3c581c2053230cd298af5e891286628ba0613b7fed370926 ;;
    aarch64|arm64) HOST=aarch64; SHA=595e95bdffabfc62559fe26e30bdfbccf277cbaed4902793555015ee47306b94 ;;
    *) die "no llvm-mingw for $(uname -m)" ;;
esac
DIR="${1:-$DIST_ROOT/target/llvm-mingw}"
NAME="llvm-mingw-$VERSION-ucrt-ubuntu-22.04-$HOST"
if [ ! -x "$DIR/$NAME/bin/clang" ]; then
    mkdir -p "$DIR"
    curl -fsSL --retry 5 --retry-all-errors -o "$DIR/$NAME.tar.xz" \
        "https://github.com/mstorsjo/llvm-mingw/releases/download/$VERSION/$NAME.tar.xz"
    got=$(sha256sum "$DIR/$NAME.tar.xz" | cut -d' ' -f1)
    [ "$got" = "$SHA" ] || die "$NAME.tar.xz: sha256 is $got, expected $SHA"
    tar -xf "$DIR/$NAME.tar.xz" -C "$DIR"
    rm "$DIR/$NAME.tar.xz"
fi
echo "$DIR/$NAME/bin"
