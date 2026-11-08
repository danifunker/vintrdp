#!/usr/bin/env bash
# Wrap a vintrdp.exe from build-windows.sh in an Inno Setup installer and a
# portable zip. Run it on Windows from Git Bash (what `shell: bash` gives on
# GitHub's Windows runners), or on Linux or macOS with Inno Setup under Wine.
#
#   scripts/dist/package-windows.sh --arch x86|x64|arm64 --exe FILE [options]
#
# Needs 7-Zip and Inno Setup 6; set ISCC if the compiler isn't in the default place.
source "$(dirname "$0")/lib.sh"

ARCH=""; EXE=""
parse_common_args "$@"
set -- ${REST[@]+"${REST[@]}"}
while [ $# -gt 0 ]; do
    case "$1" in
        --arch) ARCH="$2"; shift 2 ;;
        --exe)  EXE="$2"; shift 2 ;;
        -h|--help) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; common_usage; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done
[ -n "$ARCH" ] && [ -f "$EXE" ] || die "--arch and --exe are required"

need 7z
if command -v cygpath >/dev/null 2>&1; then
    # Windows (Git Bash). It rewrites arguments that look like POSIX paths, which
    # ISCC's /D, /O and /F switches do; without MSYS2_ARG_CONV_EXCL it sees them as
    # extra script filenames.
    winpath() { cygpath -w "$1"; }
    ISCC="${ISCC:-/c/Program Files (x86)/Inno Setup 6/ISCC.exe}"
    iscc() { MSYS2_ARG_CONV_EXCL='*' "$ISCC" "$@"; }
    HINT="choco install innosetup, or set ISCC"
else
    # Linux or macOS: Inno Setup installed under Wine; ISCC is its ISCC.exe.
    need wine winepath
    winpath() { winepath -w "$1" 2>/dev/null; }
    ISCC="${ISCC:-${WINEPREFIX:-$HOME/.wine}/drive_c/Program Files (x86)/Inno Setup 6/ISCC.exe}"
    iscc() { WINEDEBUG=-all wine "$ISCC" "$@"; }
    HINT="install Inno Setup 6 under Wine, or set ISCC (and WINEPREFIX)"
fi
[ -f "$ISCC" ] || command -v "$ISCC" >/dev/null 2>&1 || die "Inno Setup compiler not found at '$ISCC' ($HINT)"
S=$(stage_dir)
cp "$EXE" "$S/vintrdp.exe"
cp LICENSE "$S/LICENSE.txt"

group_start "installer"
iscc "/DMyAppVersion=$VERSION" "/DTargetArch=$ARCH" \
    "/DSourceDir=$(winpath "$S")" "/DLicenseFile=$(winpath "$S/LICENSE.txt")" \
    "/O$(winpath "$OUT_DIR")" "/Fvintrdp-Setup-windows-${ARCH}-${VERSION}" \
    "$(winpath "$DIST_ROOT/installer/vintrdp.iss")"
[ -f "$OUT_DIR/vintrdp-Setup-windows-${ARCH}-${VERSION}.exe" ] || die "the installer was not produced"
group_end

group_start "portable zip"
ZIP="$OUT_DIR/vintrdp-windows-${ARCH}-${VERSION}.zip"
(cd "$S" && 7z a -bso0 "$(winpath "$ZIP")" vintrdp.exe LICENSE.txt)
verify_members "$ZIP" vintrdp.exe LICENSE.txt
group_end
rm -rf "$S"
log "done:"; ls -lh "$OUT_DIR" >&2
