#!/usr/bin/env bash
# =============================================================================
#  Mneme One-Shot Bootstrap
#  Persistent memory engine for AI agents — MCP JSON-RPC stdio server
#
#  Usage:
#    curl -sSL https://raw.githubusercontent.com/tcconnally/mneme/main/scripts/bootstrap.sh | bash
#
#  What this does:
#    1. Installs system dependencies (Rust toolchain via rustup, build tools)
#    2. Clones and builds Mneme from source (release binary)
#    3. Installs the binary to ~/.local/bin/mneme
#    4. Creates the data directory and generates .env defaults
#    5. Verifies the installation and prints a success summary
#
#  Idempotent — safe to re-run. Existing binary is only rebuilt if
#  FORCE=1 or the repo checkout is stale.
# =============================================================================
set -euo pipefail

# ── Colors ──────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

ok()   { printf "${GREEN}✓${NC} %s\n" "$*"; }
warn() { printf "${YELLOW}⚠${NC} %s\n" "$*"; }
fail() { printf "${RED}✗${NC} %s\n" "$*" >&2; exit 1; }
info() { printf "${CYAN}→${NC} %s\n" "$*"; }
header() { printf "\n${BOLD}══ %s ══${NC}\n" "$*"; }

FORCE="${FORCE:-0}"
MNEME_REPO="https://github.com/tcconnally/mneme.git"
MNEME_DIR="${MNEME_DIR:-$HOME/.mneme}"
MNEME_BIN_DIR="${MNEME_BIN_DIR:-$HOME/.local/bin}"
MNEME_DATA_DIR="${MNEME_DATA_DIR:-$HOME/.mneme/data}"
MNEME_DB_PATH="${MNEME_DB_PATH:-$MNEME_DATA_DIR/mneme.db}"
WORKSPACE="${WORKSPACE:-$(pwd)}"

echo ""
echo "============================================"
echo "  Mneme One-Shot Bootstrap"
echo "  Persistent memory engine for AI agents"
echo "  github.com/tcconnally/mneme"
echo "============================================"

# ── Step 1: System dependencies ─────────────────────────────────────────────
header "Step 1: System dependencies"

detect_pkg_manager() {
    if command -v apt-get &>/dev/null; then echo "apt"
    elif command -v yum &>/dev/null; then echo "yum"
    elif command -v dnf &>/dev/null; then echo "dnf"
    elif command -v pacman &>/dev/null; then echo "pacman"
    elif command -v brew &>/dev/null; then echo "brew"
    elif command -v apk &>/dev/null; then echo "apk"
    else echo "unknown"; fi
}

PKG_MGR=$(detect_pkg_manager)

# Install build tools (C compiler, linker — needed by rusqlite with bundled feature)
install_build_tools() {
    case "$PKG_MGR" in
        apt)
            apt-get update -qq && apt-get install -y -qq build-essential pkg-config curl git
            ;;
        yum|dnf)
            $PKG_MGR install -y gcc gcc-c++ make pkg-config curl git
            ;;
        pacman)
            pacman -Sy --noconfirm base-devel pkg-config curl git
            ;;
        apk)
            apk add --no-cache build-base pkgconfig curl git
            ;;
        brew)
            # Xcode CLI tools should already be present on macOS
            if ! xcode-select -p &>/dev/null; then
                info "Installing Xcode Command Line Tools..."
                xcode-select --install 2>/dev/null || true
            fi
            ;;
        *)
            info "Checking for C compiler..."
            ;;
    esac
}

# Check for C compiler
if ! command -v cc &>/dev/null; then
    warn "C compiler not found. Installing build tools..."
    install_build_tools
fi
if command -v cc &>/dev/null; then
    ok "C compiler: $(cc --version 2>&1 | head -1)"
else
    fail "C compiler is required to build Mneme (rusqlite with bundled SQLite). Install build-essential or equivalent."
fi

# Check/install Rust
install_rust() {
    info "Installing Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
}

if command -v cargo &>/dev/null; then
    RUST_VER=$(cargo --version 2>&1)
    ok "Cargo: $RUST_VER"
else
    if [ -f "$HOME/.cargo/bin/cargo" ]; then
        info "Found cargo in ~/.cargo/bin — adding to PATH"
        export PATH="$HOME/.cargo/bin:$PATH"
        ok "Cargo: $(cargo --version 2>&1)"
    else
        warn "Rust toolchain not found."
        install_rust
        if ! command -v cargo &>/dev/null; then
            fail "Rust installation failed. Install manually: https://rustup.rs"
        fi
        ok "Cargo: $(cargo --version 2>&1)"
    fi
fi

# ── Step 2: Clone / update Mneme repo ───────────────────────────────────────
header "Step 2: Clone & build Mneme"

if [ -d "$MNEME_DIR/.git" ]; then
    info "Updating existing checkout at $MNEME_DIR..."
    git -C "$MNEME_DIR" fetch origin 2>/dev/null || true
    LOCAL_HASH=$(git -C "$MNEME_DIR" rev-parse HEAD 2>/dev/null || echo "unknown")
    REMOTE_HASH=$(git -C "$MNEME_DIR" rev-parse origin/main 2>/dev/null || echo "unknown")
    if [ "$LOCAL_HASH" != "$REMOTE_HASH" ] || [ "$FORCE" = "1" ]; then
        info "Pulling latest changes..."
        git -C "$MNEME_DIR" checkout main 2>/dev/null || git -C "$MNEME_DIR" checkout master 2>/dev/null || true
        git -C "$MNEME_DIR" pull origin main 2>/dev/null || git -C "$MNEME_DIR" pull origin master 2>/dev/null || true
    else
        ok "Repo is up to date"
    fi
else
    info "Cloning Mneme from GitHub..."
    rm -rf "$MNEME_DIR"
    git clone --depth 1 "$MNEME_REPO" "$MNEME_DIR"
fi

# Build release binary
info "Building Mneme (release)..."
cd "$MNEME_DIR"
cargo build --release 2>&1 | tail -5
BINARY="$MNEME_DIR/target/release/mneme"

if [ ! -f "$BINARY" ]; then
    fail "Build failed. Check the output above for errors."
fi
ok "Binary built: $BINARY ($(du -h "$BINARY" | cut -f1))"

# ── Step 3: Install binary ──────────────────────────────────────────────────
header "Step 3: Install binary"

mkdir -p "$MNEME_BIN_DIR"
cp "$BINARY" "$MNEME_BIN_DIR/mneme"
chmod +x "$MNEME_BIN_DIR/mneme"

# Ensure ~/.local/bin is on PATH
case ":$PATH:" in
    *":$MNEME_BIN_DIR:"*) ;;
    *) export PATH="$MNEME_BIN_DIR:$PATH" ;;
esac

if command -v mneme &>/dev/null; then
    MNEME_VER=$(mneme --version 2>&1 || echo "unknown")
    ok "mneme installed to $MNEME_BIN_DIR/mneme"
    ok "Version: $MNEME_VER"
else
    fail "mneme not found on PATH after install. Check $MNEME_BIN_DIR"
fi

# ── Step 4: Create data directory ───────────────────────────────────────────
header "Step 4: Data directory"

if [ -d "$MNEME_DATA_DIR" ]; then
    ok "Data directory exists: $MNEME_DATA_DIR"
else
    info "Creating data directory: $MNEME_DATA_DIR"
    mkdir -p "$MNEME_DATA_DIR"
    ok "Data directory created"
fi

# Warm up the database (creates tables + FTS5 index)
if [ ! -f "$MNEME_DB_PATH" ]; then
    info "Warming up database at $MNEME_DB_PATH..."
    # Brief serve+kill to trigger DB creation
    timeout 2 mneme --db "$MNEME_DB_PATH" 2>/dev/null || true
    if [ -f "$MNEME_DB_PATH" ]; then
        ok "Database created: $MNEME_DB_PATH"
    else
        warn "Database warm-up didn't create the file (will be created on first serve)"
    fi
else
    ok "Database exists: $MNEME_DB_PATH"
fi

# ── Step 5: .env entries ────────────────────────────────────────────────────
header "Step 5: Environment"

ENV_FILE="$WORKSPACE/.env"
MNEME_ENV_BLOCK="# ── Mneme ──────────────────────────────────────────────────────────────
# Database path (default shown)
MNEME_DB_PATH=$MNEME_DB_PATH
"

if [ -f "$ENV_FILE" ]; then
    if grep -q "MNEME_DB_PATH" "$ENV_FILE" 2>/dev/null; then
        ok "MNEME_DB_PATH already in .env"
    else
        info "Appending MNEME_DB_PATH to existing .env..."
        echo "$MNEME_ENV_BLOCK" >> "$ENV_FILE"
        ok "Appended to $ENV_FILE"
    fi
else
    BOOTSTRAP_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null || date -u)
    cat > "$ENV_FILE" << ENVEOF
# =============================================================================
#  Mneme Environment
#  Generated by Mneme bootstrap — ${BOOTSTRAP_DATE}
# =============================================================================

# Database path
MNEME_DB_PATH=$MNEME_DB_PATH

# ── Optional: LLM Provider Keys (for future versions with LLM extraction) ──
# DEEPSEEK_API_KEY=***
# OPENAI_API_KEY=***
# ANTHROPIC_API_KEY=***
ENVEOF
    ok ".env created at $ENV_FILE"
fi

# ── Step 6: Verify binary ───────────────────────────────────────────────────
header "Step 6: Verify binary"

# Quick smoke test: start server directly, check it initializes
SMOKE_OUT=$(timeout 2 mneme --db "$MNEME_DB_PATH" 2>&1 </dev/null || true)
if echo "$SMOKE_OUT" | grep -q "MCP server ready"; then
    ok "MCP server initializes correctly"
    ok "Tools: mneme_recall, mneme_store, mneme_health"
else
    warn "MCP smoke test had issues (non-critical). Manual check:"
    warn "  Run: mneme --db $MNEME_DB_PATH"
fi

# ── Step 7: Success summary ─────────────────────────────────────────────────
header "Success Summary"

echo ""
printf "  ${BOLD}%-30s${NC} %s\n" "Mneme version:" "$(mneme --version 2>&1 || echo 'unknown')"
printf "  ${BOLD}%-30s${NC} %s\n" "Binary:" "$MNEME_BIN_DIR/mneme"
printf "  ${BOLD}%-30s${NC} %s\n" "Database:" "$([ -f "$MNEME_DB_PATH" ] && echo "✓ $MNEME_DB_PATH" || echo 'created on first serve')"
printf "  ${BOLD}%-30s${NC} %s\n" "Data dir:" "$MNEME_DATA_DIR"
printf "  ${BOLD}%-30s${NC} %s\n" "MCP tools:" "mneme_recall, mneme_store, mneme_health"
printf "  ${BOLD}%-30s${NC} %s\n" "Cargo:" "$(cargo --version 2>&1)"
printf "  ${BOLD}%-30s${NC} %s\n" "OS:" "$(uname -s) $(uname -m)"
printf "  ${BOLD}%-30s${NC} %s\n" ".env:" "$([ -f "$ENV_FILE" ] && echo '✓ exists' || echo '✗ missing')"

echo ""
echo "============================================"
echo "  ${GREEN}Mneme bootstrap complete!${NC}"
echo ""
echo "  Quick commands:"
echo "    mneme --db $MNEME_DB_PATH   # Start MCP server"
echo "    mneme --version             # Show version"
echo ""
echo "  Standalone MCP server:"
echo "    mneme --db $MNEME_DB_PATH"
echo ""
echo "  Docs: https://github.com/tcconnally/mneme"
echo "============================================"
