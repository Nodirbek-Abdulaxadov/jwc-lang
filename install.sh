#!/usr/bin/env bash
# jwc — one-liner installer (Linux x86_64 / aarch64).
#
# Not macOS: no darwin binary is published, so this script stops with
# "Unsupported platform: darwin-*" there. The header used to claim macOS,
# which meant the documented one-liner promised a platform that has never
# had a build. Build from source or use the Docker image instead.
#
#   curl -fsSL https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.sh | bash
#
# Override targets via env:
#   JWC_VERSION=v0.2.0            install a specific release tag
#   JWC_INSTALL_DIR=/opt/jwc/bin  put the binaries somewhere other than ~/.jwc/bin
#   JWC_DOWNLOAD_BASE=https://...  pull from a mirror (e.g. the project's MinIO)
#                                  instead of GitHub Releases. The script
#                                  expects an asset name of
#                                  "jwc-${VERSION}-${ARCH_SHORT}.tar.gz" there.

set -euo pipefail

REPO="just-web-code/jwc-lang"
INSTALL_DIR="${JWC_INSTALL_DIR:-${HOME}/.jwc/bin}"
DOWNLOAD_BASE="${JWC_DOWNLOAD_BASE:-}"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
# Set JWC_MUSL=1 to fetch the fully-static musl tarball instead of the
# default glibc one. The musl build runs unchanged on Alpine, distroless
# and glibc-old hosts. See docs/docs/deployment/musl-static.md.
case "${os}-${arch}" in
    linux-x86_64)
        if [[ "${JWC_MUSL:-0}" == "1" ]]; then
            short="x86_64-unknown-linux-musl"
        else
            short="x86_64-linux"
        fi
        ext="tar.gz"
        ;;
    # `uname -m` says aarch64 on Linux; arm64 is what macOS and some
    # minimal userlands (Android shells, Alpine images) report for the
    # same hardware. Accept both or half the arm64 hosts still dead-end.
    linux-aarch64 | linux-arm64)
        if [[ "${JWC_MUSL:-0}" == "1" ]]; then
            short="aarch64-unknown-linux-musl"
        else
            short="aarch64-linux"
        fi
        ext="tar.gz"
        ;;
    *)
        echo "Unsupported platform: ${os}-${arch}." >&2
        echo "Prebuilt JWC binaries ship for x86_64 and aarch64 Linux, and x86_64 Windows." >&2
        echo >&2
        # The old text said "Build from source: ./install-from-source.sh",
        # which is unactionable in the documented install path: you get here
        # by piping this script from curl, so there is no ./ anything on disk
        # and nothing to run. Give the clone first.
        echo "To build from source (needs a Rust toolchain):" >&2
        echo "  git clone https://github.com/${REPO}.git" >&2
        echo "  cd jwc-lang && ./install-from-source.sh" >&2
        echo >&2
        # Deliberately not advertising `docker run ghcr.io/...` here: the
        # published packages are private, so an anonymous pull fails and the
        # suggestion would be the same kind of unfollowable advice this
        # message was rewritten to stop giving. The docs page carries the
        # authenticated variant.
        echo "Other options: https://jwc.1kb.uz/getting-started/install" >&2
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

# Verify the sha256 checksum when the release publishes one (releases after
# v0.4.1 do). Older releases lack the .sha256 asset — warn and continue.
if curl -fsSL "${url}.sha256" -o "${tmp}/${asset}.sha256" 2>/dev/null; then
    echo "Verifying sha256 checksum..."
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "${tmp}" && sha256sum -c "${asset}.sha256")
    elif command -v shasum >/dev/null 2>&1; then
        (cd "${tmp}" && shasum -a 256 -c "${asset}.sha256")
    else
        echo "WARNING: no sha256sum/shasum on PATH — skipping verification." >&2
    fi
else
    echo "WARNING: ${asset}.sha256 not published for ${version} — skipping verification." >&2
fi

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
