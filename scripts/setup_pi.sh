#!/usr/bin/env bash
# Configure a Raspberry Pi for use with the emonHP board by appending the
# required UART overlays to config.txt when they are not already present.
#
# Usage:
#   sudo scripts/setup_pi.sh
#   sudo scripts/setup_pi.sh /boot/firmware/config.txt

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
OVERLAY_SNIPPET="${REPO_ROOT}/assets/raspberry-pi-uart-overlays.txt"

START_MARKER="# BEGIN emonhp-fw Raspberry Pi UART overlays"
END_MARKER="# END emonhp-fw Raspberry Pi UART overlays"

find_config_txt() {
    if [[ $# -gt 0 ]]; then
        printf '%s\n' "$1"
    elif [[ -n "${CONFIG_TXT:-}" ]]; then
        printf '%s\n' "${CONFIG_TXT}"
    elif [[ -e /boot/firmware/config.txt ]]; then
        printf '%s\n' /boot/firmware/config.txt
    else
        printf '%s\n' /boot/config.txt
    fi
}

prompt_reboot() {
    if [[ ! -t 0 ]]; then
        echo "Restart required for overlay changes to take effect."
        return
    fi

    read -r -p "Restart now to apply UART overlay changes? [y/N] " response
    case "${response}" in
        [yY]|[yY][eE][sS])
            reboot
            ;;
        *)
            echo "Restart skipped. Restart the Raspberry Pi later to apply overlay changes."
            ;;
    esac
}

CONFIG_TXT="$(find_config_txt "$@")"

if [[ ! -f "${OVERLAY_SNIPPET}" ]]; then
    echo "Overlay snippet not found: ${OVERLAY_SNIPPET}" >&2
    exit 1
fi

if [[ ! -e "${CONFIG_TXT}" ]]; then
    echo "Config file not found: ${CONFIG_TXT}" >&2
    exit 1
fi

if grep -Fq "${START_MARKER}" "${CONFIG_TXT}"; then
    echo "Raspberry Pi UART overlays already present in ${CONFIG_TXT}"
    exit 0
fi

{
    printf '\n%s\n' "${START_MARKER}"
    cat "${OVERLAY_SNIPPET}"
    printf '%s\n' "${END_MARKER}"
} >> "${CONFIG_TXT}"

echo "Added Raspberry Pi UART overlays to ${CONFIG_TXT}"
prompt_reboot
