#!/bin/sh
# install.sh — install azure-support-ticket-mcp on Linux or macOS.
#
# Usage:
#   curl -sSL https://github.com/artlovan/azure_support_ticket_mcp/releases/latest/download/install.sh | sh
#
# Options (when running the script directly, not via curl|sh):
#   --version=vX.Y.Z       install a specific version (default: latest)
#   --prefix=/install/dir  override install directory
#                          (default: $HOME/.local/bin; needs sudo if not writable)
#
# What it does:
#   1. Detects OS and CPU architecture.
#   2. Downloads the matching binary + .sha256 sidecar from GitHub Releases.
#   3. Verifies the SHA256 checksum.
#   4. chmod +x and moves the binary into the install directory.
#   5. Prints a PATH hint if the install directory is not already on $PATH.

set -eu

# ---- Configuration ----------------------------------------------------------

# Repository to install from. Update these when the project moves.
OWNER="${AZURE_SUPPORT_TICKET_MCP_OWNER:-artlovan}"
REPO="${AZURE_SUPPORT_TICKET_MCP_REPO:-azure_support_ticket_mcp}"

BIN_NAME="azure-support-ticket-mcp"
VERSION="latest"
PREFIX="${HOME}/.local/bin"

# ---- Argument parsing -------------------------------------------------------

for arg in "$@"; do
    case "$arg" in
        --version=*) VERSION="${arg#--version=}" ;;
        --prefix=*)  PREFIX="${arg#--prefix=}" ;;
        --help|-h)
            sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "install.sh: unknown argument: $arg" >&2
            echo "Run with --help for usage." >&2
            exit 2
            ;;
    esac
done

# ---- Platform detection -----------------------------------------------------

os_raw="$(uname -s)"
arch_raw="$(uname -m)"

case "$os_raw" in
    Linux)   os="linux" ;;
    Darwin)  os="darwin" ;;
    *)
        echo "install.sh: unsupported operating system: $os_raw" >&2
        echo "Supported: Linux, macOS. Windows users: use install.ps1." >&2
        exit 1
        ;;
esac

case "$arch_raw" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *)
        echo "install.sh: unsupported CPU architecture: $arch_raw" >&2
        echo "Supported: x86_64, aarch64 / arm64." >&2
        exit 1
        ;;
esac

ASSET="${BIN_NAME}-${os}-${arch}"

# ---- Build download URLs ----------------------------------------------------

if [ "$VERSION" = "latest" ]; then
    BASE_URL="https://github.com/${OWNER}/${REPO}/releases/latest/download"
else
    BASE_URL="https://github.com/${OWNER}/${REPO}/releases/download/${VERSION}"
fi

BIN_URL="${BASE_URL}/${ASSET}"
SHA_URL="${BASE_URL}/${ASSET}.sha256"

# ---- Helpers ----------------------------------------------------------------

have() { command -v "$1" >/dev/null 2>&1; }

fetch() {
    # fetch <url> <destination>
    if have curl; then
        curl -fSL --retry 3 --retry-delay 2 -o "$2" "$1"
    elif have wget; then
        wget -q -O "$2" "$1"
    else
        echo "install.sh: neither curl nor wget found on PATH." >&2
        exit 1
    fi
}

sha256_of() {
    # sha256_of <file>  -> prints just the hex digest
    if have shasum; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif have sha256sum; then
        sha256sum "$1" | awk '{print $1}'
    else
        echo "install.sh: neither shasum nor sha256sum found on PATH." >&2
        exit 1
    fi
}

# ---- Download + verify ------------------------------------------------------

tmpdir="$(mktemp -d 2>/dev/null || mktemp -d -t azure-support-ticket-mcp-install)"
trap 'rm -rf "$tmpdir"' EXIT INT TERM

echo "Installing ${BIN_NAME} (${os}-${arch}, version: ${VERSION})"
echo "  source:  ${BIN_URL}"
echo "  target:  ${PREFIX}/${BIN_NAME}"

fetch "$BIN_URL" "${tmpdir}/${BIN_NAME}"
fetch "$SHA_URL" "${tmpdir}/${BIN_NAME}.sha256"

expected="$(cat "${tmpdir}/${BIN_NAME}.sha256" | awk '{print $1}')"
actual="$(sha256_of "${tmpdir}/${BIN_NAME}")"

if [ "$expected" != "$actual" ]; then
    echo "install.sh: checksum mismatch." >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    echo "Refusing to install. Please re-run; if this persists, file an issue." >&2
    exit 1
fi

echo "  sha256:  ${actual}  [verified]"

# ---- Install ---------------------------------------------------------------

chmod +x "${tmpdir}/${BIN_NAME}"

if ! mkdir -p "$PREFIX" 2>/dev/null; then
    echo "install.sh: cannot create ${PREFIX} without elevated privileges." >&2
    echo "  Retry with sudo, or pass --prefix=/another/dir (e.g. \$HOME/.local/bin)." >&2
    exit 1
fi

if mv "${tmpdir}/${BIN_NAME}" "${PREFIX}/${BIN_NAME}" 2>/dev/null; then
    :
else
    echo "install.sh: cannot write to ${PREFIX} without elevated privileges." >&2
    echo "  Retry with sudo (e.g. \`sudo --prefix=${PREFIX}\` after curl-piping), or" >&2
    echo "  pass --prefix=\$HOME/.local/bin for a no-sudo install." >&2
    exit 1
fi

echo
echo "Installed: ${PREFIX}/${BIN_NAME}"

# ---- PATH hint --------------------------------------------------------------

case ":${PATH:-}:" in
    *":${PREFIX}:"*)
        # Already on PATH, nothing to do.
        ;;
    *)
        echo
        echo "NOTE: ${PREFIX} is not currently on your PATH."
        echo "Add this line to your shell rc (~/.zshrc, ~/.bashrc, ~/.profile, etc.):"
        echo
        echo "    export PATH=\"${PREFIX}:\$PATH\""
        echo
        echo "Then open a new shell or run: source <your-shell-rc>"
        ;;
esac

echo
echo "Next: run \`${BIN_NAME} doctor\` to verify the install."
