#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

profile="release"
exe_path=""

usage() {
    cat <<'EOF'
Usage: ./install.sh [--release|--debug] [--exe-path <path>]

Options:
  --release            Build/install release binary (default)
  --debug              Build/install debug binary
  --exe-path <path>    Use an existing jwc binary instead of building
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release)
            profile="release"
            shift
            ;;
        --debug)
            profile="debug"
            shift
            ;;
        --exe-path)
            if [[ $# -lt 2 ]]; then
                echo "Error: --exe-path requires a value" >&2
                usage
                exit 1
            fi
            exe_path="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Error: unknown argument '$1'" >&2
            usage
            exit 1
            ;;
    esac
done

exe_src=""

if [[ -n "$exe_path" ]]; then
    exe_src="$exe_path"
elif [[ -f "$ROOT_DIR/target/$profile/jwc" ]]; then
    exe_src="$ROOT_DIR/target/$profile/jwc"
elif [[ -f "$ROOT_DIR/jwc" ]]; then
    exe_src="$ROOT_DIR/jwc"
elif [[ -f "$ROOT_DIR/bin/jwc" ]]; then
    exe_src="$ROOT_DIR/bin/jwc"
else
    if ! command -v cargo >/dev/null 2>&1; then
        echo "Error: cargo not found and no prebuilt jwc binary available." >&2
        echo "Provide --exe-path <path-to-jwc> or place a prebuilt binary at ./jwc or ./bin/jwc" >&2
        exit 1
    fi

    echo "Building jwc ($profile)..."
    if [[ "$profile" == "release" ]]; then
        cargo build --release
    else
        cargo build
    fi
    exe_src="$ROOT_DIR/target/$profile/jwc"
fi

if [[ ! -f "$exe_src" ]]; then
    echo "Error: jwc binary not found at '$exe_src'" >&2
    exit 1
fi

install_dir="${HOME}/.local/bin"
mkdir -p "$install_dir"

exe_dst="$install_dir/jwc"
cp -f "$exe_src" "$exe_dst"
chmod +x "$exe_dst"

echo "Installed: $exe_dst"

if [[ ":${PATH}:" != *":${install_dir}:"* ]]; then
    echo "Note: ${install_dir} is not in your PATH."
    echo "Add this line to your shell profile (e.g. ~/.bashrc or ~/.zshrc):"
    echo "  export PATH=\"$HOME/.local/bin:$PATH\""
fi

echo "Try: jwc --help"
