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
    if (cd apps/web && RUSTFLAGS="--cfg=web_sys_unstable_apis" cargo clippy --target wasm32-unknown-unknown -- \
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
    if RUSTFLAGS="--cfg=web_sys_unstable_apis" cargo check --workspace --all-features 2>&1; then
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
# UNIT TESTS (Native)
# ============================================================
run_unit_tests() {
    echo ""
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}🧪 Running Unit Tests (Native)${NC}"
    echo -e "${BLUE}================================================${NC}"
    echo ""

    # Run non-WASM tests (excluding WASM-only packages)
    cargo test --workspace \
        --exclude transport-webserial \
        --exclude app-web

    # Run app-web tests (lib tests, not requiring browser)
    # These tests run in native mode (not WASM), testing core logic
    echo ""
    echo -e "${YELLOW}▶ Running app-web lib tests${NC}"
    cargo test --package app-web --lib

    echo ""
    echo -e "${GREEN}✅ Unit tests passed${NC}"
}

# ============================================================
# WASM TESTS (Browser)
# ============================================================
run_wasm_tests() {
    echo ""
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}🧪 Running WASM Tests (Browser)${NC}"
    echo -e "${BLUE}================================================${NC}"
    echo ""

    # Check if wasm-pack is installed
    if ! command -v wasm-pack &> /dev/null; then
        echo -e "${YELLOW}wasm-pack not found. Installing...${NC}"
        curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
    fi

    cd apps/web
    echo -e "${YELLOW}▶ Running tests in headless Chrome${NC}"
    wasm-pack test --headless --chrome

    # Firefox tests disabled due to geckodriver stability issues on some systems
    # Uncomment to enable Firefox tests:
    # echo -e "${YELLOW}▶ Running tests in headless Firefox${NC}"
    # wasm-pack test --headless --firefox
    cd ../..

    echo ""
    echo -e "${GREEN}✅ WASM tests passed${NC}"
}

# ============================================================
# COMPREHENSIVE TEST SUITE
# ============================================================
run_all_tests() {
    echo ""
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}🎯 Running Comprehensive Test Suite${NC}"
    echo -e "${BLUE}================================================${NC}"

    run_quality_checks
    run_unit_tests
    run_wasm_tests

    echo ""
    echo -e "${BLUE}================================================${NC}"
    echo -e "${GREEN}✅ ALL TESTS PASSED${NC}"
    echo -e "${BLUE}================================================${NC}"
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
    echo "  test       🎯 Full test suite (check + unit tests + WASM tests) - RECOMMENDED"
    echo "  serve      🚀 Run checks + start dev server (default)"
    echo "  build      🏗️  Full test suite + release build"
    echo ""
    echo "  check      🔍 Quality checks only (fmt, clippy, cargo check)"
    echo "  wasm-test  🌐 WASM browser tests only"
    echo ""
    echo "If no command is specified, 'serve' is assumed for local development."
}

COMMAND="${1:-serve}"

case "$COMMAND" in
    check)
        run_quality_checks
        ;;
    test)
        # Comprehensive test suite: quality checks + unit tests + WASM tests
        run_all_tests
        ;;
    wasm-test)
        # WASM tests only (for quick iteration on browser-specific tests)
        run_wasm_tests
        ;;
    build)
        # Full validation before release
        run_all_tests
        build_release
        ;;
    serve)
        # Local development: check quality then start dev server
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
