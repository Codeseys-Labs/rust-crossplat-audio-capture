#!/bin/bash
set -e
MISSING=()
for lib in alsa libpulse libpipewire-0.3; do
    if ! pkg-config --exists "$lib"; then
        MISSING+=("$lib")
    fi
done
if [ ${#MISSING[@]} -ne 0 ]; then
    echo "Missing development packages: ${MISSING[*]}" >&2
    if [ "$RSAC_AUTO_INSTALL" = "1" ] && command -v apt-get >/dev/null; then
        packages=()
        for lib in "${MISSING[@]}"; do
            case $lib in
                alsa) packages+=("libasound2-dev") ;;
                libpulse) packages+=("libpulse-dev") ;;
                libpipewire-0.3) packages+=("libpipewire-0.3-dev") ;;
            esac
        done
        sudo apt-get update && sudo apt-get install -y "${packages[@]}"
        MISSING=()
        for lib in alsa libpulse libpipewire-0.3; do
            if ! pkg-config --exists "$lib"; then
                MISSING+=("$lib")
            fi
        done
        if [ ${#MISSING[@]} -ne 0 ]; then
            echo "Automatic install failed: ${MISSING[*]}" >&2
            exit 1
        else
            exit 0
        fi
    else
        echo "Set RSAC_AUTO_INSTALL=1 to attempt installation via apt-get." >&2
        exit 1
    fi
fi
