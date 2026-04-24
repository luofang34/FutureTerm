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

# Project root (absolute) — anchors all paths regardless of cd
PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)"

# Path to debug bridge-daemon binary (built by `serve` command)
BRIDGE_DEBUG_BIN="${PROJECT_ROOT}/target/debug/bridge-daemon"

# Track whether serve mode built the debug bridge binary
BUILT_BRIDGE_DEBUG=false

# Cleanup function
cleanup() {
    echo ""
    echo "Stopping background processes..."
    kill $(jobs -p) 2>/dev/null
    # Remove debug bridge-daemon artifact only if we built it
    if [ "${BUILT_BRIDGE_DEBUG}" = true ] && [ -f "${BRIDGE_DEBUG_BIN}" ]; then
        echo "Cleaning debug bridge-daemon artifact..."
        rm -f "${BRIDGE_DEBUG_BIN}"
    fi
    exit 0
}

trap cleanup SIGINT SIGTERM EXIT

# ============================================================
# QUALITY CHECKS (GATE)
# ============================================================
run_quality_checks() {
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}🔍 Code Quality Gate${NC}"
    echo -e "${BLUE}================================================${NC}"
    echo ""

    local FAILED=0

    # Auto-format (always fix, never just check)
    echo -e "${YELLOW}▶ Auto-Format${NC}"
    if cargo fmt --all 2>&1; then
        echo -e "${GREEN}✓ Code formatted${NC}"
        echo ""
    else
        echo -e "${RED}✗ Format FAILED${NC}"
        echo ""
        FAILED=1
    fi

    # File size limit (500 lines per .rs file, per CLAUDE.md)
    echo -e "${YELLOW}▶ File size limit${NC}"
    if bash scripts/check_file_sizes.sh 2>&1; then
        echo -e "${GREEN}✓ File size limit passed${NC}"
        echo ""
    else
        echo -e "${RED}✗ File size limit FAILED${NC}"
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
    if cargo check --workspace --exclude transport-webserial --exclude transport-websocket --exclude app-web --all-features 2>&1; then
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
        --exclude transport-websocket \
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
# BRIDGE DAEMON (macOS only)
# ============================================================
build_bridge() {
    echo ""
    echo -e "${BLUE}================================================${NC}"
    echo -e "${BLUE}Building macOS Bridge Daemon${NC}"
    echo -e "${BLUE}================================================${NC}"
    echo ""

    if [[ "$(uname)" != "Darwin" ]]; then
        echo -e "${RED}Bridge build requires macOS (codesign + hdiutil)${NC}"
        exit 1
    fi

    local BRIDGE_DIR="apps/bridge-macos"
    local APP_BUNDLE="${BRIDGE_DIR}/FutureTerm.app"
    local DMG_PATH="${BRIDGE_DIR}/FutureTerm-Helper.dmg"
    local SIGN_IDENTITY="${CODESIGN_IDENTITY:--}"
    # Notarization keychain profile (created once with: xcrun notarytool store-credentials)
    local NOTARY_PROFILE="${APPLE_NOTARY_KEYCHAIN_PROFILE:-futureterm-notary}"

    # Build
    echo -e "${YELLOW}Building bridge-daemon (release)...${NC}"
    cargo build --release -p bridge-daemon

    # Generate app icon from SVG source (requires rsvg-convert: brew install librsvg)
    echo -e "${YELLOW}Generating app icon...${NC}"
    local ICONSET_DIR
    ICONSET_DIR=$(mktemp -d)/FutureTerm.iconset
    mkdir -p "${ICONSET_DIR}"
    if ! command -v rsvg-convert &>/dev/null; then
        echo -e "${RED}rsvg-convert not found — skipping icon (brew install librsvg)${NC}"
    else
        local SVG_SRC="${BRIDGE_DIR}/icon.svg"
        # Render all required macOS icon sizes from the single SVG source
        for size in 16 32 64 128 256 512 1024; do
            rsvg-convert -w "$size" -h "$size" "${SVG_SRC}" \
                -o "${ICONSET_DIR}/icon_${size}x${size}.png" 2>/dev/null
        done
        # @2x (HiDPI) variants — copy from next size up
        cp "${ICONSET_DIR}/icon_32x32.png"   "${ICONSET_DIR}/icon_16x16@2x.png"
        cp "${ICONSET_DIR}/icon_64x64.png"   "${ICONSET_DIR}/icon_32x32@2x.png"
        cp "${ICONSET_DIR}/icon_256x256.png" "${ICONSET_DIR}/icon_128x128@2x.png"
        cp "${ICONSET_DIR}/icon_512x512.png" "${ICONSET_DIR}/icon_256x256@2x.png"
        cp "${ICONSET_DIR}/icon_1024x1024.png" "${ICONSET_DIR}/icon_512x512@2x.png"
        # Remove non-standard sizes used only for @2x sources
        rm -f "${ICONSET_DIR}/icon_64x64.png" "${ICONSET_DIR}/icon_1024x1024.png"
        iconutil -c icns "${ICONSET_DIR}" -o "${BRIDGE_DIR}/AppIcon.icns"
        rm -rf "$(dirname "${ICONSET_DIR}")"
        echo -e "${GREEN}Icon generated: ${BRIDGE_DIR}/AppIcon.icns${NC}"
    fi

    # Create app bundle
    echo -e "${YELLOW}Creating app bundle...${NC}"
    rm -rf "${APP_BUNDLE}"
    mkdir -p "${APP_BUNDLE}/Contents/MacOS" "${APP_BUNDLE}/Contents/Resources"
    cp target/release/bridge-daemon "${APP_BUNDLE}/Contents/MacOS/bridge-daemon-bin"
    cp "${BRIDGE_DIR}/Info.plist" "${APP_BUNDLE}/Contents/"
    # Copy icon if generated
    if [ -f "${BRIDGE_DIR}/AppIcon.icns" ]; then
        cp "${BRIDGE_DIR}/AppIcon.icns" "${APP_BUNDLE}/Contents/Resources/"
    fi

    # Create launcher script (prevents macOS "not responding" dialog on URL scheme launch)
    cat > "${APP_BUNDLE}/Contents/MacOS/bridge-daemon" << 'LAUNCHER'
#!/bin/bash
DIR="$(cd "$(dirname "$0")" && pwd)"
mkdir -p ~/Library/Logs/FutureTerm
nohup "${DIR}/bridge-daemon-bin" "$@" >> ~/Library/Logs/FutureTerm/bridge.log 2>&1 &
LAUNCHER
    chmod +x "${APP_BUNDLE}/Contents/MacOS/bridge-daemon" "${APP_BUNDLE}/Contents/MacOS/bridge-daemon-bin"

    # Code sign (inside-out for proper entitlements support)
    echo -e "${YELLOW}Code signing (identity: ${SIGN_IDENTITY})...${NC}"
    if [ "${SIGN_IDENTITY}" != "-" ]; then
        codesign --force --options runtime \
            --sign "${SIGN_IDENTITY}" \
            --entitlements "${BRIDGE_DIR}/bridge-daemon.entitlements" \
            "${APP_BUNDLE}/Contents/MacOS/bridge-daemon-bin"
        codesign --force --options runtime \
            --sign "${SIGN_IDENTITY}" \
            --entitlements "${BRIDGE_DIR}/bridge-daemon.entitlements" \
            "${APP_BUNDLE}"
    else
        codesign --deep --force --sign "-" "${APP_BUNDLE}" 2>/dev/null || true
    fi

    # Create DMG with Applications symlink for drag-to-install
    echo -e "${YELLOW}Creating DMG...${NC}"
    rm -f "${DMG_PATH}"
    local STAGING_DIR
    STAGING_DIR=$(mktemp -d)
    cp -R "${APP_BUNDLE}" "${STAGING_DIR}/"
    ln -s /Applications "${STAGING_DIR}/Applications"
    hdiutil create -volname "FutureTerm Helper" \
        -srcfolder "${STAGING_DIR}" -ov -format UDZO "${DMG_PATH}" > /dev/null
    rm -rf "${STAGING_DIR}"

    # Notarize and staple (only when signed with real Developer ID)
    if [ "${SIGN_IDENTITY}" != "-" ]; then
        echo -e "${YELLOW}Notarizing DMG (this may take 1-5 minutes)...${NC}"
        if xcrun notarytool submit "${DMG_PATH}" \
            --keychain-profile "${NOTARY_PROFILE}" \
            --wait \
            --timeout 10m 2>&1; then
            echo -e "${YELLOW}Stapling notarization ticket...${NC}"
            xcrun stapler staple "${DMG_PATH}"
            echo -e "${GREEN}Notarization complete and stapled${NC}"
        else
            echo -e "${RED}Notarization FAILED — DMG is unsigned for distribution${NC}"
            echo -e "${YELLOW}To set up notarization credentials:${NC}"
            echo -e "${CYAN}  xcrun notarytool store-credentials futureterm-notary \\${NC}"
            echo -e "${CYAN}    --apple-id YOUR_APPLE_ID \\${NC}"
            echo -e "${CYAN}    --team-id YOUR_TEAM_ID \\${NC}"
            echo -e "${CYAN}    --password APP_SPECIFIC_PASSWORD${NC}"
        fi
    fi

    # Copy to bridge-helper for local serving
    local BRIDGE_HELPER_DIR="apps/web/bridge-helper"
    if [ -d "${BRIDGE_HELPER_DIR}" ]; then
        cp "${DMG_PATH}" "${BRIDGE_HELPER_DIR}/"
    fi

    echo ""
    echo -e "${GREEN}Bridge build complete: ${DMG_PATH}${NC}"
    echo -e "${CYAN}Install: cp -R ${APP_BUNDLE} /Applications/${NC}"
    echo -e "${CYAN}Test:    open futureterm://launch${NC}"
    if [ "${SIGN_IDENTITY}" != "-" ]; then
        echo -e "${CYAN}Sign:    CODESIGN_IDENTITY='Developer ID Application: Name (TEAMID)' ./dev.sh bridge${NC}"
    fi
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

    # Kill any process occupying port 8080 or 9876
    echo -e "${YELLOW}Checking ports 8080 and 9876...${NC}"
    if lsof -ti :8080 >/dev/null 2>&1; then
        echo -e "${YELLOW}Killing process on port 8080...${NC}"
        lsof -ti :8080 | xargs kill -9 2>/dev/null
        sleep 1
    fi
    if lsof -ti :9876 >/dev/null 2>&1; then
        echo -e "${YELLOW}Killing process on port 9876...${NC}"
        lsof -ti :9876 | xargs kill -9 2>/dev/null
        sleep 1
    fi

    # Build bridge daemon in debug mode (enables localhost Origin)
    echo -e "${YELLOW}Building bridge-daemon (debug)...${NC}"
    if cargo build -p bridge-daemon; then
        BUILT_BRIDGE_DEBUG=true
        echo -e "${GREEN}✓ Bridge daemon built${NC}"
        echo ""

        # Start bridge daemon in background
        echo -e "${GREEN}Starting bridge daemon on port 9876...${NC}"
        "${BRIDGE_DEBUG_BIN}" &
        BRIDGE_PID=$!
        sleep 1

        if kill -0 "$BRIDGE_PID" 2>/dev/null; then
            echo -e "${GREEN}✓ Bridge daemon running (PID ${BRIDGE_PID})${NC}"
        else
            echo -e "${YELLOW}⚠ Bridge daemon exited early — Safari/Firefox bridge unavailable${NC}"
        fi
    else
        echo -e "${YELLOW}⚠ Bridge daemon build failed — Safari/Firefox bridge unavailable${NC}"
    fi
    echo ""

    echo -e "${GREEN}Starting Trunk dev server on port 8080...${NC}"
    echo -e "${CYAN}App will be available at http://127.0.0.1:8080${NC}"
    echo -e "${CYAN}Bridge daemon (debug) at wss://local.futureterm.app:9876${NC}"
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
    echo "  test       Full test suite (check + unit tests + WASM tests) - RECOMMENDED"
    echo "  serve      Run checks + build/start bridge daemon (debug) + start dev server (default)"
    echo "  build      Full test suite + release build"
    echo "  bridge     Build macOS bridge daemon + app bundle + DMG (macOS only)"
    echo ""
    echo "  check      Quality checks only (fmt, clippy, cargo check)"
    echo "  wasm-test  WASM browser tests only"
    echo ""
    echo "Bridge signing:      CODESIGN_IDENTITY='Developer ID Application: Name (TEAMID)' ./dev.sh bridge"
    echo "Bridge notarize:     CODESIGN_IDENTITY='...' APPLE_NOTARY_KEYCHAIN_PROFILE=futureterm-notary ./dev.sh bridge"
    echo "  (one-time setup)   xcrun notarytool store-credentials futureterm-notary --apple-id ID --team-id TEAM --password PASS"
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
    bridge)
        # Build macOS bridge daemon + app bundle + DMG
        build_bridge
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
