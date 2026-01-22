#!/usr/bin/env bash
# All-in-one development script
# Enforces code quality checks before starting dev server or building

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# Cleanup function
cleanup() {
    echo ""
    echo "Stopping background processes..."
    kill $(jobs -p) 2>/dev/null
    exit 0
}

trap cleanup SIGINT

# ============================================================
# QUALITY CHECKS (GATE)
# ============================================================
run_quality_checks() {
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}🔍 Code Quality Gate${NC}"
    echo -e "${BLUE}================================================${NC}"
    echo ""

    local FAILED=0

    # Format check
    echo -e "${YELLOW}▶ Format Check${NC}"
    if cargo fmt --all -- --check 2>&1; then
        echo -e "${GREEN}✓ Format check passed${NC}"
        echo ""
    else
        echo -e "${RED}✗ Format check FAILED${NC}"
        echo -e "${CYAN}Run: cargo fmt --all${NC}"
        echo ""
        FAILED=1
    fi

    # Clippy for non-WASM
    echo -e "${YELLOW}▶ Clippy (non-WASM)${NC}"
    if cargo clippy --workspace \
        --exclude transport-webserial \
        --exclude app-web \
        -- \
        -D warnings \
        -D clippy::unwrap_used \
        -D clippy::expect_used \
        -D clippy::panic \
        -D clippy::indexing_slicing \
        -D clippy::todo \
        2>&1; then
        echo -e "${GREEN}✓ Clippy (non-WASM) passed${NC}"
        echo ""
    else
        echo -e "${RED}✗ Clippy (non-WASM) FAILED${NC}"
        echo ""
        FAILED=1
    fi

    # Clippy for WASM
    echo -e "${YELLOW}▶ Clippy (WASM)${NC}"
    if (cd apps/web && cargo clippy --target wasm32-unknown-unknown -- \
        -D warnings \
        -D clippy::unwrap_used \
        -D clippy::expect_used \
        -D clippy::panic \
        -D clippy::indexing_slicing \
        -D clippy::todo) 2>&1; then
        echo -e "${GREEN}✓ Clippy (WASM) passed${NC}"
        echo ""
    else
        echo -e "${RED}✗ Clippy (WASM) FAILED${NC}"
        echo ""
        FAILED=1
    fi

    # Cargo check
    echo -e "${YELLOW}▶ Cargo Check${NC}"
    if cargo check --workspace --all-features 2>&1; then
        echo -e "${GREEN}✓ Cargo check passed${NC}"
        echo ""
    else
        echo -e "${RED}✗ Cargo check FAILED${NC}"
        echo ""
        FAILED=1
    fi

    echo -e "${BLUE}================================================${NC}"
    if [ $FAILED -eq 0 ]; then
        echo -e "${GREEN}✅ ALL CHECKS PASSED${NC}"
        echo -e "${BLUE}================================================${NC}"
        return 0
    else
        echo -e "${RED}❌ CHECKS FAILED - Fix issues before proceeding${NC}"
        echo -e "${BLUE}================================================${NC}"
        return 1
    fi
}

# ============================================================
# TESTS
# ============================================================
run_tests() {
    echo ""
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}🧪 Running Tests${NC}"
    echo -e "${BLUE}================================================${NC}"
    echo ""

    cargo test --workspace \
        --exclude transport-webserial \
        --exclude app-web

    echo ""
    echo -e "${GREEN}✅ All tests passed${NC}"
}

# ============================================================
# BUILD
# ============================================================
build_release() {
    echo ""
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}🏗️  Building Release${NC}"
    echo -e "${BLUE}================================================${NC}"
    echo ""

    cd apps/web
    RUSTFLAGS="--cfg=web_sys_unstable_apis" trunk build --release
    cd ../..

    echo ""
    echo -e "${GREEN}✅ Build completed successfully${NC}"
    echo -e "${CYAN}Output: apps/web/dist/${NC}"
}

# ============================================================
# DEV SERVER WITH INTEGRATION ENVIRONMENT
# ============================================================
run_dev_server() {
    echo ""
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}🚀 Starting Development Environment${NC}"
    echo -e "${BLUE}================================================${NC}"
    echo ""

    # 1. Start socat in background
    echo "[1/3] Launching socat..."
    socat -d -d pty,raw,echo=0 pty,raw,echo=0 > socat.log 2>&1 &
    SOCAT_PID=$!

    sleep 2

    # Extract ports
    PORT1=$(grep "PTY is" socat.log | head -n1 | awk '{print $NF}')
    PORT2=$(grep "PTY is" socat.log | tail -n1 | awk '{print $NF}')

    if [ -z "$PORT1" ] || [ -z "$PORT2" ]; then
        echo -e "${RED}Error: Failed to extract PTYs from socat output.${NC}"
        cat socat.log
        cleanup
    fi

    echo -e "${CYAN}      Virtual Ports Created:${NC}"
    echo "      Generator Port: $PORT1"
    echo "      Web App Port:   $PORT2"
    echo ""

    # 2. Start Serial Generator
    echo "[2/3] Starting Serial Generator on $PORT1..."
    if [ ! -d ".venv" ]; then
        echo "      Creating virtual environment..."
        python3 -m venv .venv
        .venv/bin/pip install pyserial
    fi

    .venv/bin/python3 scripts/serial_generator.py --port "$PORT1" --baud 115200 &
    GEN_PID=$!

    # 3. Start Web App
    echo "[3/3] Starting Web App..."
    echo -e "${GREEN}      App will be available at http://127.0.0.1:8081${NC}"
    echo -e "${YELLOW}      ACTION REQUIRED: Connect manually to $PORT2 in the browser.${NC}"
    echo ""

    cd apps/web
    RUSTFLAGS="--cfg=web_sys_unstable_apis" trunk serve --port 8081

    wait
}

# ============================================================
# MAIN COMMAND DISPATCHER
# ============================================================
show_usage() {
    echo "Usage: ./dev.sh [command]"
    echo ""
    echo "Commands:"
    echo "  check      Run quality checks only (fmt, clippy, cargo check)"
    echo "  test       Run quality checks + tests"
    echo "  build      Run quality checks + tests + release build"
    echo "  serve      Run quality checks + start dev server (default)"
    echo ""
    echo "If no command is specified, 'serve' is assumed."
}

COMMAND="${1:-serve}"

case "$COMMAND" in
    check)
        run_quality_checks
        ;;
    test)
        run_quality_checks
        run_tests
        ;;
    build)
        run_quality_checks
        run_tests
        build_release
        ;;
    serve)
        run_quality_checks
        run_dev_server
        ;;
    help|--help|-h)
        show_usage
        ;;
    *)
        echo -e "${RED}Unknown command: $COMMAND${NC}"
        echo ""
        show_usage
        exit 1
        ;;
esac
