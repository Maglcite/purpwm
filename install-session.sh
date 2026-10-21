#!/bin/sh
# Compatibility entry point for the original prebuilt, system-wide installer.
set -eu
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec "$SCRIPT_DIR/install.sh" --system --no-build --non-interactive "$@"
