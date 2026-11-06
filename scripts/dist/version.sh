#!/usr/bin/env bash
# Print the version a distribution build is stamped with.
#
#   version.sh main N                 X.Y.N, X.Y from Cargo.toml (a push to main)
#   version.sh release vX.Y.Z         X.Y.Z (accepts the tag or the bare version)
#   version.sh dev                    dev.<short commit>
#
# The same string goes into the About window, --version, the macOS Info.plist,
# the Windows installer and every file name. deb and rpm get a leading "0~" when
# it does not start with a digit, as dev builds don't (see pkg_version in lib.sh).
set -euo pipefail
cd "$(dirname "$0")/../.."

usage() { sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

channel="${1:-}"
[ -n "$channel" ] || usage 2
shift

case "$channel" in
    main)
        [ $# -gt 0 ] || { echo "main needs a run number" >&2; exit 2; }
        base=$(sed -n 's/^version = "\([0-9]*\.[0-9]*\)\..*"/\1/p' Cargo.toml | head -1)
        [ -n "$base" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
        echo "$base.$1"
        ;;
    release)
        [ $# -gt 0 ] || { echo "release needs a tag (vX.Y.Z)" >&2; exit 2; }
        v="${1#refs/tags/}"; v="${v#v}"
        [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "'$1' is not a vX.Y.Z release tag" >&2; exit 2; }
        echo "$v"
        ;;
    dev)
        echo "dev.$(git rev-parse --short=8 HEAD 2>/dev/null || echo unknown)"
        ;;
    -h|--help) usage ;;
    *) usage 2 ;;
esac
