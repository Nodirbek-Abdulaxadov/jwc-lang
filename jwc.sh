#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

profile="debug"
actual_args=()
subcommand=""

for arg in "$@"; do
    case "$arg" in
        --release|-r)
            profile="release"
            if [[ "$subcommand" == "build" || "$subcommand" == "test" || "$subcommand" == "clean" ]]; then
                actual_args+=("--release")
            fi
            ;;
        --debug)
            profile="debug"
            ;;
        *)
            if [[ -z "$subcommand" && "$arg" != -* ]]; then
                subcommand="$arg"
            fi
            actual_args+=("$arg")
            ;;
    esac
done

exe="$ROOT_DIR/target/$profile/jwc"

need_build=0
if [[ ! -f "$exe" ]]; then
    need_build=1
else
    while IFS= read -r input_file; do
        if [[ "$input_file" -nt "$exe" ]]; then
            need_build=1
            break
        fi
    done < <(
        {
            printf '%s\n' "$ROOT_DIR/Cargo.toml" "$ROOT_DIR/Cargo.lock"
            find "$ROOT_DIR/src" -type f -name '*.rs' 2>/dev/null
        }
    )
fi

if [[ "$need_build" -eq 1 ]]; then
    if [[ "$profile" == "release" ]]; then
        echo "Building jwc (release)..."
        cargo build --release
    else
        echo "Building jwc (debug)..."
        cargo build
    fi
fi

exec "$exe" "${actual_args[@]}"
