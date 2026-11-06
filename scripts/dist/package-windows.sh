#!/usr/bin/env bash
# Wrap a vintrdp.exe from build-windows.sh in an Inno Setup installer and a
# portable zip. Run it on Windows from Git Bash (what `shell: bash` gives on
# GitHub's Windows runners).
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

need 7z cygpath
ISCC="${ISCC:-/c/Program Files (x86)/Inno Setup 6/ISCC.exe}"
[ -f "$ISCC" ] || command -v "$ISCC" >/dev/null 2>&1 \
    || die "Inno Setup compiler not found at '$ISCC' (choco install innosetup, or set ISCC)"

winpath() { cygpath -w "$1"; }
S=$(stage_dir)
cp "$EXE" "$S/vintrdp.exe"
cp LICENSE "$S/LICENSE.txt"

group_start "installer"
# Git Bash rewrites arguments that look like POSIX paths, which ISCC's /D, /O and
# /F switches do; without this it sees them as extra script filenames.
MSYS2_ARG_CONV_EXCL='*' "$ISCC" "/DMyAppVersion=$VERSION" "/DTargetArch=$ARCH" \
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
