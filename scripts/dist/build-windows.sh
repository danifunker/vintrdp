#!/usr/bin/env bash
# Cross-compile vintrdp.exe for Windows from Linux with llvm-mingw.
#
#   scripts/dist/build-windows.sh --target TRIPLE [options]
#
# TRIPLE is {x86_64,i686,aarch64}-pc-windows-gnullvm. One toolchain covers all
# three, and GnuTLS and friends link statically, so the .exe needs nothing
# beyond what Windows 10 ships (including the UCRT). Leaves it in --out as
# vintrdp-windows-<arch>.exe for package-windows.sh to wrap.
#
# Needs: rustup, curl, make, m4, pkg-config.
source "$(dirname "$0")/lib.sh"

parse_common_args "$@"
set -- ${REST[@]+"${REST[@]}"}
case "${1:-}" in
    -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; common_usage; exit 0 ;;
    "") ;;
    *) die "unknown option: $1" ;;
esac

case "$TARGET" in
    x86_64-pc-windows-gnullvm)  ARCH=x64 ;;
    i686-pc-windows-gnullvm)    ARCH=x86 ;;
    aarch64-pc-windows-gnullvm) ARCH=arm64 ;;
    *) die "unsupported Windows target $TARGET" ;;
esac

PATH="$("$DIST_ROOT/scripts/dist/llvm-mingw.sh"):$PATH"
export PATH
"$DIST_ROOT/scripts/dist/build-deps.sh" --target "$TARGET"

MINGW="${TARGET%%-*}-w64-mingw32"
UPPER=$(echo "$TARGET" | tr 'a-z-' 'A-Z_')
LOWER=$(echo "$TARGET" | tr '-' '_')
export "CC_$LOWER=$MINGW-clang" "AR_$LOWER=llvm-ar" "CARGO_TARGET_${UPPER}_LINKER=$MINGW-clang"
# crt-static links libunwind in; without it the .exe would need libunwind.dll.
export "CARGO_TARGET_${UPPER}_RUSTFLAGS=-C target-feature=+crt-static"
export VINTRDP_DEPS="$DIST_ROOT/target/deps/$TARGET"

build_app "$TARGET"

EXE="$OUT_DIR/vintrdp-windows-$ARCH.exe"
llvm-strip -o "$EXE" "$(release_dir "$TARGET")/vintrdp.exe"
# Anything outside Windows itself would be a missing DLL on a user's machine.
EXTRA=$(llvm-readobj --coff-imports "$EXE" | sed -n 's/^ *Name: //p' | grep -i '\.dll$' | sort -u \
    | grep -v -i -E '^(api-ms-win-|advapi32|bcrypt|bcryptprimitives|combase|comctl32|comdlg32|crypt32|dwmapi|gdi32|iphlpapi|kernel32|mmdevapi|msimg32|ncrypt|ntdll|ole32|oleaut32|shell32|user32|uxtheme|winmm|winspool|ws2_32)' || true)
[ -z "$EXTRA" ] || die "vintrdp.exe imports DLLs Windows doesn't ship: $(echo $EXTRA)"
log "done: $EXE"
