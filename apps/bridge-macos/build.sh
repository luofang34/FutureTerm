#!/bin/bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Project root (assumes script is in apps/bridge-macos/)
PROJECT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="${SCRIPT_DIR}"
APP_BUNDLE="${BUILD_DIR}/FutureTerm.app"

echo -e "${BLUE}==> Building FutureTerm macOS App Bundle${NC}"
echo ""

# Step 1: Build bridge-daemon in release mode
echo -e "${YELLOW}[1/5] Building bridge-daemon (release)...${NC}"
cd "${PROJECT_ROOT}"
cargo build --release --bin bridge-daemon
if [ $? -ne 0 ]; then
    echo -e "${RED}ERROR: Failed to build bridge-daemon${NC}"
    exit 1
fi
echo -e "${GREEN}✓ bridge-daemon built successfully${NC}"
echo ""

# Step 2: Create app bundle directory structure
echo -e "${YELLOW}[2/5] Creating app bundle structure...${NC}"
rm -rf "${APP_BUNDLE}"
mkdir -p "${APP_BUNDLE}/Contents/MacOS"
mkdir -p "${APP_BUNDLE}/Contents/Resources"
echo -e "${GREEN}✓ App bundle structure created${NC}"
echo ""

# Step 3: Copy binary
echo -e "${YELLOW}[3/5] Copying bridge-daemon binary...${NC}"
cp "${PROJECT_ROOT}/target/release/bridge-daemon" "${APP_BUNDLE}/Contents/MacOS/"
if [ $? -ne 0 ]; then
    echo -e "${RED}ERROR: Failed to copy binary${NC}"
    exit 1
fi
echo -e "${GREEN}✓ Binary copied${NC}"
echo ""

# Step 4: Copy Info.plist
echo -e "${YELLOW}[4/5] Copying Info.plist...${NC}"
cp "${SCRIPT_DIR}/Info.plist" "${APP_BUNDLE}/Contents/"
if [ $? -ne 0 ]; then
    echo -e "${RED}ERROR: Failed to copy Info.plist${NC}"
    exit 1
fi
echo -e "${GREEN}✓ Info.plist copied${NC}"
echo ""

# Step 5: Make binary executable
echo -e "${YELLOW}[5/5] Setting executable permissions...${NC}"
chmod +x "${APP_BUNDLE}/Contents/MacOS/bridge-daemon"
if [ $? -ne 0 ]; then
    echo -e "${RED}ERROR: Failed to set executable permissions${NC}"
    exit 1
fi
echo -e "${GREEN}✓ Executable permissions set${NC}"
echo ""

# Success summary
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}✓ FutureTerm.app bundle created successfully!${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo -e "Location: ${BLUE}${APP_BUNDLE}${NC}"
echo -e "Binary size: ${BLUE}$(du -h "${APP_BUNDLE}/Contents/MacOS/bridge-daemon" | cut -f1)${NC}"
echo ""
echo -e "${YELLOW}Next steps:${NC}"
echo -e "  1. Test URL scheme: ${BLUE}open futureterm://launch${NC}"
echo -e "  2. Copy to Applications: ${BLUE}cp -R ${APP_BUNDLE} /Applications/${NC}"
echo -e "  3. Code signing: ${BLUE}codesign --deep --force --sign \"-\" ${APP_BUNDLE}${NC}"
echo ""
echo -e "${YELLOW}Note:${NC} Code signing requires a valid Developer ID certificate."
echo -e "      Ad-hoc signing (with \"-\") works for local testing only."
echo ""
