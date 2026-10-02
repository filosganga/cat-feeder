#!/usr/bin/env bash
# Rebuild bootloader/bootloader.bin, the second-stage bootloader with rollback
# enabled (ADR-0024), in ESP-IDF's Docker image.
#
#   ./dev/bootloader.sh
#
# Only for changing the bootloader. Everyday work never runs this: the binary
# is committed and espflash.toml makes every flash use it.
#
# The ESP-IDF version is pinned below, so a rebuild changes the binary only when
# that line or bootloader/sdkconfig.defaults changes. A unit's bootloader is
# written only by a USB flash, never over the air, so a new one reaches a unit
# that is already closed up only when it is next opened.
#
# The bind mount is under the repo, not $TMPDIR: Docker Desktop shares /Users
# but not /private/tmp, and a folder it does not share arrives in the container
# empty, which idf.py reports as "CMakeLists.txt not found".

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

IDF_IMAGE="espressif/idf:v6.0"

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    -*) die "bootloader: unknown option '$1'. Try --help." ;;
    *) die "bootloader: unexpected argument '$1'. Try --help." ;;
  esac
done

command -v docker >/dev/null || die "bootloader: needs Docker"

PROJECT="$PWD/bootloader"

# set-target runs fullclean first, so nothing from an earlier build or an
# edited sdkconfig survives: the result follows from sdkconfig.defaults alone.
rm -f "$PROJECT/sdkconfig"
docker run --rm -v "$PROJECT":/project -w /project "$IDF_IMAGE" \
  idf.py set-target esp32c6 bootloader

BUILT="$PROJECT/build/bootloader/bootloader.bin"
[ -f "$BUILT" ] || { echo "bootloader: the build produced no $BUILT" >&2; exit 1; }
grep -q '^CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y' "$PROJECT/sdkconfig" \
  || { echo "bootloader: rollback is not enabled in the resolved sdkconfig" >&2; exit 1; }

cp "$BUILT" "$PROJECT/bootloader.bin"
echo
echo "bootloader/bootloader.bin from $IDF_IMAGE:"
shasum -a 256 "$PROJECT/bootloader.bin"
