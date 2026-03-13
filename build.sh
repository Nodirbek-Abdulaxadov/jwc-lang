#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

profile="debug"
for arg in "$@"; do
    case "$arg" in
        --release|-r)
            profile="release"
            ;;
        --debug)
            profile="debug"
            ;;
        *)
            ;;
    esac
done

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found. Install Rust toolchain first: https://rustup.rs" >&2
    exit 1
fi

os_name="$(uname -s)"
arch_name="$(uname -m)"

echo "Native build target"
echo "  OS: $os_name"
echo "  Arch: $arch_name"
echo "  Profile: $profile"

if [[ "$profile" == "release" ]]; then
    cargo build --release
else
    cargo build
fi

bin_path="$ROOT_DIR/target/$profile/jwc"
if [[ ! -f "$bin_path" ]]; then
    echo "Build completed but binary was not found: $bin_path" >&2
    exit 1
fi

echo "Build OK"
echo "Binary: $bin_path"
