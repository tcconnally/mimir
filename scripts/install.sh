#!/usr/bin/env bash
set -euo pipefail

# Mimir one-line installer
# Usage: curl -sSf https://get.mimir.perseus.observer | sh
# Or:     curl -sSf https://raw.githubusercontent.com/Perseus-Computing-LLC/mimir/main/scripts/install.sh | sh
#
# Supports: Linux (x86_64, aarch64), macOS (x86_64, aarch64), WSL

BOLD="\033[1m"
GREEN="\033[32m"
YELLOW="\033[33m"
RED="\033[31m"
RESET="\033[0m"

REPO="Perseus-Computing-LLC/mimir"
BIN_DIR="${MIMIR_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${MIMIR_VERSION:-latest}"

echo -e "${BOLD}Mimir Installer${RESET}"
echo "Persistent memory for AI agents — MCP-native, local-first, zero dependencies."
echo ""

# ── Detect platform ──────────────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)  OS="unknown-linux-gnu" ;;
    Darwin) OS="apple-darwin" ;;
    *)
        echo -e "${RED}Unsupported OS: $OS${RESET}"
        echo "Mimir supports Linux (x86_64, aarch64), macOS (x86_64, aarch64), and Windows (via cargo install)."
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64|amd64)  ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *)
        echo -e "${RED}Unsupported architecture: $ARCH${RESET}"
        exit 1
        ;;
esac

TARGET="${ARCH}-${OS}"
ARCHIVE_NAME="mimir-${TARGET}"

# ── Download ─────────────────────────────────────────────────────────
if [ "$VERSION" = "latest" ]; then
    DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ARCHIVE_NAME}"
else
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE_NAME}"
fi

echo -e "→ Platform: ${BOLD}${TARGET}${RESET}"
echo -e "→ Installing to: ${BOLD}${BIN_DIR}${RESET}"
echo ""

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "→ Downloading mimir..."
if command -v curl >/dev/null 2>&1; then
    HTTP_CODE=$(curl -sSfL -w "%{http_code}" -o "$TMP_DIR/mimir" "$DOWNLOAD_URL")
    if [ "$HTTP_CODE" != "200" ] && [ "$HTTP_CODE" != "302" ]; then
        echo -e "${RED}Download failed (HTTP $HTTP_CODE)${RESET}"
        echo "No pre-built binary for ${TARGET}."
        echo ""
        echo "Build from source with cargo:"
        echo "  cargo install --git https://github.com/${REPO}"
        exit 1
    fi
elif command -v wget >/dev/null 2>&1; then
    wget -q --show-progress -O "$TMP_DIR/mimir" "$DOWNLOAD_URL"
else
    echo -e "${RED}Need curl or wget to download mimir.${RESET}"
    exit 1
fi

# ── Install ──────────────────────────────────────────────────────────
mkdir -p "$BIN_DIR"
chmod +x "$TMP_DIR/mimir"
mv "$TMP_DIR/mimir" "$BIN_DIR/mimir"

# Check if BIN_DIR is on PATH
if ! echo "$PATH" | tr ':' '\n' | grep -qxF "$BIN_DIR"; then
    case "$SHELL" in
        */zsh) RC="$HOME/.zshrc" ;;
        */bash) RC="$HOME/.bashrc" ;;
        */fish) RC="$HOME/.config/fish/config.fish" ;;
        *) RC="$HOME/.profile" ;;
    esac
    echo ""
    echo -e "${YELLOW}⚠  $BIN_DIR is not on your PATH.${RESET}"
    echo "   Add this to your shell config:"
    echo ""
    echo -e "   ${BOLD}export PATH=\"\$HOME/.local/bin:\$PATH\"${RESET}"
    echo ""
    echo "   Or run:  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> $RC"
fi

# ── Verify ───────────────────────────────────────────────────────────
echo ""
echo "→ Verifying install..."
"$BIN_DIR/mimir" --version 2>/dev/null || true
echo ""
echo -e "${GREEN}${BOLD}✓ Mimir installed to $BIN_DIR/mimir${RESET}"
echo ""
echo "Quick start:"
echo "  mimir serve --db ~/.mimir/data/mimir.db"
echo ""
echo "MCP config (Claude Desktop, Cursor, Hermes, etc.):"
echo '  {'
echo '    "mcpServers": {'
echo '      "mimir": {'
echo '        "command": "'"$BIN_DIR"'/mimir",'
echo '        "args": ["serve", "--db", "~/.mimir/data/mimir.db"]'
echo '      }'
echo '    }'
echo '  }'
echo ""
echo "Docs: https://github.com/${REPO}"
