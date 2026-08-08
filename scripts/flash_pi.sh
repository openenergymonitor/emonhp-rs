#!/usr/bin/env bash
# Flash firmware to the attached STM32C031 using Raspberry Pi GPIO SWD and
# openocd_pi.cfg.
#
# Usage:
#   scripts/flash_pi.sh /path/to/firmware.bin
#   scripts/flash_pi.sh /path/to/firmware.elf

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
OPENOCD_CFG="${REPO_ROOT}/openocd_pi.cfg"
FLASH_BASE_ADDRESS="0x08000000"

usage() {
    echo "Usage: $0 <firmware-binary>" >&2
}

if [[ $# -ne 1 ]]; then
    usage
    exit 2
fi

FIRMWARE="$1"

if [[ ! -f "${FIRMWARE}" ]]; then
    echo "Firmware binary not found: ${FIRMWARE}" >&2
    exit 1
fi

if [[ ! -f "${OPENOCD_CFG}" ]]; then
    echo "OpenOCD Raspberry Pi config not found: ${OPENOCD_CFG}" >&2
    exit 1
fi

case "${FIRMWARE}" in
    *.bin)
        openocd -f "${OPENOCD_CFG}" \
            -c "program ${FIRMWARE} ${FLASH_BASE_ADDRESS} verify reset exit"
        ;;
    *)
        openocd -f "${OPENOCD_CFG}" \
            -c "program ${FIRMWARE} verify reset exit"
        ;;
esac
