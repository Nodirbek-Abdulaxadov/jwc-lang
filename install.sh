#!/usr/bin/env bash
# jwc — one-liner installer (Linux / macOS).
#
#   curl -fsSL https://raw.githubusercontent.com/Nodirbek-Abdulaxadov/jwc-lang/main/install.sh | bash
#
# Override targets via env:
#   JWC_VERSION=v0.2.0            install a specific release tag
#   JWC_INSTALL_DIR=/opt/jwc/bin  put the binaries somewhere other than ~/.jwc/bin
#   JWC_DOWNLOAD_BASE=https://...  pull from a mirror (e.g. the project's MinIO)
#                                  instead of GitHub Releases. The script
#                                  expects an asset name of
#                                  "jwc-${VERSION}-${ARCH_SHORT}.tar.gz" there.

set -euo pipefail

REPO="Nodirbek-Abdulaxadov/jwc-lang"
INSTALL_DIR="${JWC_INSTALL_DIR:-${HOME}/.jwc/bin}"
DOWNLOAD_BASE="${JWC_DOWNLOAD_BASE:-}"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "${os}-${arch}" in
    linux-x86_64)  short="x86_64-linux";  ext="tar.gz" ;;
    *)
        echo "Unsupported platform: ${os}-${arch}." >&2
        echo "Prebuilt JWC binaries currently ship for x86_64 Linux + Windows." >&2
        echo "Build from source: ./install-from-source.sh" >&2
        exit 1
        ;;
esac

version="${JWC_VERSION:-}"
if [[ -z "${version}" ]]; then
    echo "Resolving latest release tag for ${REPO}..."
    # Cheap, dep-free version lookup. No `jq` required.
    version=$(
        curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep -oE '"tag_name":\s*"[^"]+"' | head -1 | cut -d'"' -f4
    )
    if [[ -z "${version}" ]]; then
        echo "Failed to resolve latest version. Set JWC_VERSION to pin one." >&2
        exit 1
    fi
fi

asset="jwc-${version}-${short}.${ext}"
if [[ -n "${DOWNLOAD_BASE}" ]]; then
    url="${DOWNLOAD_BASE%/}/${asset}"
else
    url="https://github.com/${REPO}/releases/download/${version}/${asset}"
fi

echo "Downloading ${url}"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

curl -fL "${url}" -o "${tmp}/${asset}"
tar -xzf "${tmp}/${asset}" -C "${tmp}"

mkdir -p "${INSTALL_DIR}"
install -m 0755 "${tmp}/jwc"     "${INSTALL_DIR}/jwc"
install -m 0755 "${tmp}/jwc-lsp" "${INSTALL_DIR}/jwc-lsp"

echo "Installed: ${INSTALL_DIR}/jwc"
echo "Installed: ${INSTALL_DIR}/jwc-lsp"

if [[ ":${PATH}:" != *":${INSTALL_DIR}:"* ]]; then
    echo
    echo "Add to PATH (drop into your shell rc file):"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
fi

echo
echo "Try:  jwc --help"
