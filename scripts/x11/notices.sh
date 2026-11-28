#!/bin/sh
# Write THIRD-PARTY-NOTICES.txt at the top of the repository from the X11
# server's data notices (crates/vintrdp-x11/data/COPYING and
# data/fonts/COPYING), for the release packages to carry next to LICENSE.
# A test (vintrdp's notices_are_current) fails when it's out of date.
#
#   scripts/x11/notices.sh
set -e
cd "$(dirname "$0")/../.."
{
    printf '%s\n' \
        "vintrdp's X server bundles colour names and bitmap fonts made by others." \
        "Their notices follow, as their licences ask; vintrdp itself is under" \
        "LICENSE." \
        "" \
        "================================================================" \
        "crates/vintrdp-x11/data/COPYING: rgb.txt and IRIX's colour names" \
        "================================================================" \
        ""
    cat crates/vintrdp-x11/data/COPYING
    printf '%s\n' \
        "" \
        "================================================================" \
        "crates/vintrdp-x11/data/fonts/COPYING: X.org's bitmap fonts" \
        "================================================================" \
        ""
    cat crates/vintrdp-x11/data/fonts/COPYING
} > THIRD-PARTY-NOTICES.txt
