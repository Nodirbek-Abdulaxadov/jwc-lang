#!/usr/bin/env bash
set -euo pipefail

install_dir="${HOME}/.local/bin"
exe_dst="$install_dir/jwc"

if [[ -f "$exe_dst" ]]; then
    rm -f "$exe_dst"
    echo "Removed: $exe_dst"
else
    echo "Not installed: $exe_dst"
fi

echo "If needed, remove '$install_dir' from your PATH manually."
