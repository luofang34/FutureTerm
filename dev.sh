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
# DEV SERVER
# ============================================================
run_dev_server() {
    echo ""
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}🚀 Starting Development Server${NC}"
    echo -e "${BLUE}================================================${NC}"
    echo ""

    # Kill any process occupying port 8080
    echo -e "${YELLOW}Checking port 8080...${NC}"
    if lsof -ti :8080 >/dev/null 2>&1; then
        echo -e "${YELLOW}Killing process on port 8080...${NC}"
        lsof -ti :8080 | xargs kill -9 2>/dev/null
        sleep 1
    fi

    echo -e "${GREEN}Starting Trunk dev server on port 8080...${NC}"
    echo -e "${CYAN}App will be available at http://127.0.0.1:8080${NC}"
    echo ""
    echo -e "${YELLOW}Connect to a real serial device through the browser.${NC}"
    echo -e "${YELLOW}Or use socat to create virtual ports for testing.${NC}"
    echo ""

    cd apps/web
    RUSTFLAGS="--cfg=web_sys_unstable_apis" trunk serve --port 8080

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
