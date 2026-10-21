#!/bin/sh
# Resolve the checkout independently of the caller's working directory.
set -eu
if ! command -v python3 >/dev/null 2>&1; then
    echo 'purpwm setup needs Python 3.9+. Install python3 and run this script again.' >&2
    exit 1
fi
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$SCRIPT_DIR/scripts/install.py" "$@"
