#!/usr/bin/env bash
# jwc — one-liner installer (Linux and macOS, x86_64 / aarch64).
#
# macOS is published as of v0.9.923. It never had a build before that —
# not removed at any cutover, simply never in the release matrix, while
# the install page claimed archives for it. This script was the honest
# one: it stopped with "Unsupported platform: darwin-*".
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
        GLIBC_FLAVOUR="x86_64-linux"
        MUSL_FLAVOUR="x86_64-unknown-linux-musl"
        ext="tar.gz"
        ;;
    # `uname -m` says aarch64 on Linux; arm64 is what macOS and some
    # minimal userlands (Android shells, Alpine images) report for the
    # same hardware. Accept both or half the arm64 hosts still dead-end.
    linux-aarch64 | linux-arm64)
        GLIBC_FLAVOUR="aarch64-linux"
        MUSL_FLAVOUR="aarch64-unknown-linux-musl"
        ext="tar.gz"
        ;;
    # macOS builds natively on both architectures and links against the
    # system libSystem, so there is no glibc/musl split here — the same
    # flavour is used for the retry path, which then never fires.
    darwin-x86_64)
        GLIBC_FLAVOUR="x86_64-macos"
        MUSL_FLAVOUR="x86_64-macos"
        ext="tar.gz"
        ;;
    darwin-arm64 | darwin-aarch64)
        GLIBC_FLAVOUR="aarch64-macos"
        MUSL_FLAVOUR="aarch64-macos"
        ext="tar.gz"
        ;;
    *)
        echo "Unsupported platform: ${os}-${arch}." >&2
        echo "Prebuilt JWC binaries ship for x86_64 and aarch64 Linux and macOS, and x86_64 Windows." >&2
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

# JWC_MUSL=1 asks for the static build up front. Otherwise start on glibc and
# let the smoke test below fall back if the host libc is too old.
if [[ "${JWC_MUSL:-0}" == "1" ]]; then
    short="${MUSL_FLAVOUR}"
else
    short="${GLIBC_FLAVOUR}"
fi

# Resolve the newest release tag *without* touching api.github.com.
#
# The API caps unauthenticated clients at 60 requests/hour per IP. Mobile
# carriers put thousands of subscribers behind one NAT address, so that budget
# is usually already spent by somebody else and the install dies with
#
#     curl: (22) The requested URL returned error: 403
#
# on a phone while the identical command works from a home network minutes
# earlier. `/releases/latest` on github.com is a plain redirect to the tag
# page and is not part of that budget, so it goes first. The API stays as a
# fallback in case the redirect shape ever changes, and uses GITHUB_TOKEN when
# one is set (5000/hour, authenticated).
resolve_latest_tag() {
    local final
    final="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
        "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)"
    case "${final}" in
        */releases/tag/*)
            printf '%s\n' "${final##*/releases/tag/}"
            return 0
            ;;
    esac

    local api="https://api.github.com/repos/${REPO}/releases/latest"
    local body
    if [[ -n "${GITHUB_TOKEN:-}" ]]; then
        body="$(curl -fsSL -H "Authorization: Bearer ${GITHUB_TOKEN}" "${api}" 2>/dev/null || true)"
    else
        body="$(curl -fsSL "${api}" 2>/dev/null || true)"
    fi
    # Dep-free parse — no `jq` required.
    printf '%s' "${body}" | grep -oE '"tag_name":\s*"[^"]+"' | head -1 | cut -d'"' -f4 || true
}

version="${JWC_VERSION:-}"
if [[ -z "${version}" ]]; then
    echo "Resolving latest release tag for ${REPO}..."
    version="$(resolve_latest_tag)"
    if [[ -z "${version}" ]]; then
        echo "Failed to resolve the latest release tag for ${REPO}." >&2
        echo >&2
        echo "If this is a phone, a hotspot or a shared/corporate network, the" >&2
        echo "likely cause is GitHub's unauthenticated API limit — 60 requests" >&2
        echo "per hour per IP address, shared with everyone behind the same NAT." >&2
        echo >&2
        # No concrete tag here on purpose — hardcoding one means another
        # version reference to keep in sync, and it only ever appears in an
        # error message. The releases page is always current.
        echo "Skip the lookup by pinning a version. Current tags:" >&2
        echo "  https://github.com/${REPO}/releases" >&2
        echo >&2
        echo "  curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | JWC_VERSION=vX.Y.Z bash" >&2
        echo >&2
        echo "(the variable goes before \`bash\`, not before \`curl\` — it is bash that needs it)" >&2
        echo >&2
        echo "Or authenticate, which raises the limit to 5000/hour:" >&2
        echo "  export GITHUB_TOKEN=<a personal access token>" >&2
        exit 1
    fi
fi

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

# Download + verify + install one flavour. Factored out so the glibc build can
# be retried as musl without re-running the whole script — `curl | bash` has no
# script file on disk, so re-exec is not an option.
fetch_and_install() {
    local flavour="$1"
    local asset="jwc-${version}-${flavour}.${ext}"
    local url
    if [[ -n "${DOWNLOAD_BASE}" ]]; then
        url="${DOWNLOAD_BASE%/}/${asset}"
    else
        url="https://github.com/${REPO}/releases/download/${version}/${asset}"
    fi

    echo "Downloading ${url}"
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
    install -m 0755 "${tmp}/jwc" "${INSTALL_DIR}/jwc"
    # `jwc-lsp` is not built at the moment: it was written against the
    # pre-1.0 parser, which v0.25.0 removed, and it returns rewritten in
    # v0.27.0. Older release archives still carry it, so install it when
    # the archive has one rather than failing on its absence.
    if [ -f "${tmp}/jwc-lsp" ]; then
        install -m 0755 "${tmp}/jwc-lsp" "${INSTALL_DIR}/jwc-lsp"
    fi
}

fetch_and_install "${short}"

# A glibc binary built against a newer libc than the host installs perfectly
# and then fails at first run:
#
#   jwc: /lib/aarch64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found
#
# Running it once here turns that into something the installer can fix. The
# release ships a fully-static musl build of every Linux target for exactly
# this case, so retry with it rather than leaving a broken binary on PATH.
#
# Deliberately a smoke test rather than an `ldd --version` comparison: that
# would need a minimum-glibc constant kept in sync with whatever runner the
# release workflow uses, and it would drift silently. Asking the binary
# whether it runs cannot drift.
if ! "${INSTALL_DIR}/jwc" --version >/dev/null 2>&1; then
    if [[ "${short}" == *-musl ]]; then
        echo >&2
        echo "ERROR: the installed jwc does not run on this host:" >&2
        "${INSTALL_DIR}/jwc" --version >&2 || true
        echo >&2
        echo "This is the static musl build, so a libc mismatch is not the cause." >&2
        echo "Please report it: https://github.com/${REPO}/issues" >&2
        exit 1
    fi
    echo
    echo "The glibc build does not run here — usually a host libc older than the"
    echo "one it was built against. Retrying with the fully-static musl build."
    echo
    fetch_and_install "${MUSL_FLAVOUR}"
    if ! "${INSTALL_DIR}/jwc" --version >/dev/null 2>&1; then
        echo >&2
        echo "ERROR: the musl build does not run here either:" >&2
        "${INSTALL_DIR}/jwc" --version >&2 || true
        echo "Please report it: https://github.com/${REPO}/issues" >&2
        exit 1
    fi
fi

echo "Installed: ${INSTALL_DIR}/jwc"
if [ -f "${INSTALL_DIR}/jwc-lsp" ]; then
    echo "Installed: ${INSTALL_DIR}/jwc-lsp"
fi

if [[ ":${PATH}:" != *":${INSTALL_DIR}:"* ]]; then
    echo
    echo "Add to PATH (drop into your shell rc file):"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
fi

echo
echo "Try:  jwc --help"
