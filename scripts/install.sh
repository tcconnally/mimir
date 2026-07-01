#!/usr/bin/env bash
set -euo pipefail

# Perseus Vault (formerly "Mneme"/"Mimir") one-line installer
# Usage: curl -sSf https://get.mimir.perseus.observer | sh
# Or:     curl -sSf https://raw.githubusercontent.com/Perseus-Computing-LLC/perseus-vault/main/scripts/install.sh | sh
#
# Supports: Linux (x86_64, aarch64), macOS (x86_64, aarch64), WSL

BOLD="\033[1m"
GREEN="\033[32m"
YELLOW="\033[33m"
RED="\033[31m"
RESET="\033[0m"

REPO="Perseus-Computing-LLC/perseus-vault"
BIN_DIR="${MIMIR_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${MIMIR_VERSION:-latest}"

echo -e "${BOLD}Perseus Vault Installer${RESET}"
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
        echo "Perseus Vault supports Linux (x86_64, aarch64), macOS (x86_64, aarch64), and Windows (via cargo install)."
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
# Perseus Vault rename (transition release): releases cut after this rename
# publish a "perseus-vault-${TARGET}" asset. Older releases publish
# "mneme-${TARGET}", and releases before that publish "mimir-${TARGET}".
# Try the newest name first and fall back through the legacy names so this
# script keeps working against every published release, not just future ones.
ARCHIVE_NAME="perseus-vault-${TARGET}"
LEGACY_ARCHIVE_NAME="mneme-${TARGET}"
LEGACY2_ARCHIVE_NAME="mimir-${TARGET}"

# ── Download ─────────────────────────────────────────────────────────
if [ "$VERSION" = "latest" ]; then
    DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ARCHIVE_NAME}"
    LEGACY_DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${LEGACY_ARCHIVE_NAME}"
    LEGACY2_DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${LEGACY2_ARCHIVE_NAME}"
else
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE_NAME}"
    LEGACY_DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${LEGACY_ARCHIVE_NAME}"
    LEGACY2_DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${LEGACY2_ARCHIVE_NAME}"
fi

echo -e "→ Platform: ${BOLD}${TARGET}${RESET}"
echo -e "→ Installing to: ${BOLD}${BIN_DIR}${RESET}"
echo ""

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "→ Downloading perseus-vault..."
if command -v curl >/dev/null 2>&1; then
    HTTP_CODE=$(curl -sSfL -w "%{http_code}" -o "$TMP_DIR/perseus-vault" "$DOWNLOAD_URL")
    if [ "$HTTP_CODE" != "200" ] && [ "$HTTP_CODE" != "302" ]; then
        echo -e "${YELLOW}→ No '${ARCHIVE_NAME}' asset yet, trying pre-rename '${LEGACY_ARCHIVE_NAME}'...${RESET}"
        HTTP_CODE=$(curl -sSfL -w "%{http_code}" -o "$TMP_DIR/perseus-vault" "$LEGACY_DOWNLOAD_URL")
    fi
    if [ "$HTTP_CODE" != "200" ] && [ "$HTTP_CODE" != "302" ]; then
        echo -e "${YELLOW}→ No '${LEGACY_ARCHIVE_NAME}' asset either, trying pre-rename '${LEGACY2_ARCHIVE_NAME}'...${RESET}"
        HTTP_CODE=$(curl -sSfL -w "%{http_code}" -o "$TMP_DIR/perseus-vault" "$LEGACY2_DOWNLOAD_URL")
    fi
    if [ "$HTTP_CODE" != "200" ] && [ "$HTTP_CODE" != "302" ]; then
        echo -e "${RED}Download failed (HTTP $HTTP_CODE)${RESET}"
        echo "No pre-built binary for ${TARGET}."
        echo ""
        echo "Build from source with cargo:"
        echo "  cargo install --git https://github.com/${REPO}"
        exit 1
    fi
elif command -v wget >/dev/null 2>&1; then
    wget -q --show-progress -O "$TMP_DIR/perseus-vault" "$DOWNLOAD_URL" \
        || wget -q --show-progress -O "$TMP_DIR/perseus-vault" "$LEGACY_DOWNLOAD_URL" \
        || wget -q --show-progress -O "$TMP_DIR/perseus-vault" "$LEGACY2_DOWNLOAD_URL"
else
    echo -e "${RED}Need curl or wget to download perseus-vault.${RESET}"
    exit 1
fi

# ── Install ──────────────────────────────────────────────────────────
mkdir -p "$BIN_DIR"
chmod +x "$TMP_DIR/perseus-vault"
mv "$TMP_DIR/perseus-vault" "$BIN_DIR/perseus-vault"
# Perseus Vault rename: keep "mneme" and "mimir" symlinks so existing MCP host
# configs/scripts that invoke either older command name keep working unchanged.
ln -sf "$BIN_DIR/perseus-vault" "$BIN_DIR/mneme"
ln -sf "$BIN_DIR/perseus-vault" "$BIN_DIR/mimir"

# macOS: ad-hoc code-sign so the binary is not killed on launch (#312). On Apple
# Silicon an unsigned binary is SIGKILLed (Killed: 9) by the OS binary policy —
# even with no quarantine xattr — so `perseus-vault --version`/`doctor` would
# produce no output. `codesign --sign -` applies an ad-hoc signature; harmless
# on Intel.
if [ "$OS" = "apple-darwin" ] && command -v codesign >/dev/null 2>&1; then
    if codesign --force --sign - "$BIN_DIR/perseus-vault" 2>/dev/null; then
        echo "→ Ad-hoc code-signed for macOS"
    else
        echo -e "${YELLOW}⚠  Could not code-sign. If 'perseus-vault' is Killed: 9, run:${RESET}"
        echo "     codesign --sign - $BIN_DIR/perseus-vault"
    fi
fi

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
"$BIN_DIR/perseus-vault" --version 2>/dev/null || true
echo ""
echo -e "${GREEN}${BOLD}✓ Perseus Vault installed to $BIN_DIR/perseus-vault${RESET}"
echo ""
echo "Quick start:"
echo "  perseus-vault serve --db ~/.mimir/data/perseus-vault.db"
echo ""
echo "MCP config (Claude Desktop, Cursor, Hermes, etc.):"
echo '  {'
echo '    "mcpServers": {'
echo '      "perseus-vault": {'
echo '        "command": "'"$BIN_DIR"'/perseus-vault",'
echo '        "args": ["serve", "--db", "~/.mimir/data/perseus-vault.db"]'
echo '      }'
echo '    }'
echo '  }'
echo ""
echo "Docs: https://github.com/${REPO}"
