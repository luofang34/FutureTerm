# Development Workflow

## Philosophy
**Safety First**: Enforce strict code quality through automated checks.
**No Panic Production**: Zero unwrap/panic/unsafe in WASM code.
**Agent-Friendly**: Single unified script for all development tasks.
**Comprehensive Testing**: Dual strategy - native unit tests + browser WASM tests.

## Quick Start

All development operations use a single script: **`./dev.sh`**

```bash
./dev.sh test       # 🎯 RECOMMENDED - Full test suite (fmt + clippy + unit tests + WASM browser tests)
./dev.sh serve      # 🚀 Run quality checks + start dev server (default)
./dev.sh build      # 🏗️ Full test suite + release build
./dev.sh check      # 🔍 Quality checks only (fmt, clippy, cargo check)
./dev.sh wasm-test  # 🌐 WASM browser tests only (Chrome + Firefox headless)
```

**Default**: Running `./dev.sh` without arguments runs quality checks and starts the dev server.

## Quality Gate (Always Enforced)

Every operation (`check`, `test`, `build`, `serve`) starts with strict quality checks:

1. **Format Check**: `cargo fmt --all -- --check`
2. **Clippy (non-WASM)**: All workspace crates except WASM
3. **Clippy (WASM)**: `apps/web` with `wasm32-unknown-unknown` target
4. **Cargo Check**: Full workspace with all features

### Strict Clippy Rules (DENIED)

- ❌ `unwrap_used` - No `.unwrap()` calls
- ❌ `expect_used` - No `.expect()` calls
- ❌ `panic` - No `panic!()` calls
- ❌ `indexing_slicing` - No unsafe indexing (`arr[i]`)
- ❌ `todo` - No `todo!()` macros

All violations block development workflows.

## Configuration Files

- **`clippy.toml`**: Clippy settings (allow unwrap in tests only)
- **`rustfmt.toml`**: Consistent code formatting (100 char line width)
- **`.gitignore`**: Excludes `*.log`, `.venv`, temp files

## Testing

```bash
./dev.sh test
```

Runs **comprehensive test suite** with dual testing strategy:

### 1. Quality Checks
- Format check (`cargo fmt --all -- --check`)
- Clippy for non-WASM crates
- Clippy for WASM crates (`wasm32-unknown-unknown` target)
- Full workspace cargo check

### 2. Unit Tests (Native Rust)
Runs `cargo test` for all crates, testing pure logic without browser dependencies:
- `core-types`: Serialization, event types
- `framing`: Line splitting, COBS, SLIP
- `decoders`: MAVLink, NMEA parsing
- `analysis`: Scoring algorithms
- `app-web` (lib tests): Connection FSM, state machine invariants (51 tests with `proptest`)

**Current count: 72+ native unit tests**

### 3. WASM Tests (Browser Integration)
Runs `wasm-pack test` in **headless Chrome and Firefox** for WebSerial API integration:
- `prober.rs`: Framing detection, timeout logic
- `reconnect.rs`: Auto-reconnect retry behavior
- Browser-specific serial port behavior

**Current count: 8 wasm-bindgen tests**

### Quick Commands

```bash
./dev.sh test       # Full suite (recommended before commit)
./dev.sh wasm-test  # Browser tests only (quick iteration)
./dev.sh check      # Quality checks only (fastest)
```

**Test Distribution by Module:**
- `types.rs`: 51 tests (FSM state machine + property-based tests)
- `prober.rs`: 8 tests (5 unit + 3 browser)
- `reconnect.rs`: 5 tests (4 unit + 2 browser)
- `driver.rs`: 3 tests
- Other modules: 13 tests
- **Total: 80 tests**

## Building

```bash
./dev.sh build
```

1. Runs quality checks (format, clippy, cargo check)
2. Runs unit tests (native Rust)
3. Runs WASM tests (browser integration)
4. Builds release WASM bundle with Trunk
5. Output: `apps/web/dist/`

**Build is blocked if any step fails**, ensuring production quality.

## Development Server

```bash
./dev.sh serve
# or simply
./dev.sh
```

Starts Trunk dev server at **http://127.0.0.1:8080**

**Features**:
- Hot reload on file changes
- WASM compilation with optimized flags (`--cfg=web_sys_unstable_apis`)
- Source maps for debugging
- Automatic port cleanup (kills any process on port 8080)

**Testing**: Connect to real serial devices through browser WebSerial API (Chrome/Edge only).

## CI/CD Integration

GitHub Actions workflow (`.github/workflows/ci.yml`) uses `./dev.sh`:

```yaml
jobs:
  test:
    - name: Run comprehensive test suite
      run: ./dev.sh test  # Quality checks + unit tests + WASM browser tests

  build:
    - name: Run tests and build release
      run: ./dev.sh build  # Full test suite + release build
```

**CI runs identical checks as local development**:
- Auto-installs wasm-pack if not found
- Runs WASM tests in headless Chrome and Firefox
- Blocks merge if any test fails

**Workflow triggers**: Every push and pull request on all branches.

## Common Tasks

### Fix Formatting Issues
```bash
cargo fmt --all
```

### Run Clippy Manually (with suggestions)
```bash
cargo clippy --workspace --exclude transport-webserial --exclude app-web
```

### Build for Production
```bash
./dev.sh build
# Deploy: apps/web/dist/
```

### Check Specific Crate
```bash
cd crates/framing
cargo test
cargo clippy
```

## Project Structure

```
wasmserialtool/
├── apps/
│   ├── web/          # Leptos WASM frontend
│   │   └── src/
│   │       ├── connection/    # Connection FSM (CRITICAL - read carefully)
│   │       │   ├── types.rs   # Atomic state machine + lock-aware transitions
│   │       │   ├── driver.rs  # Port lifecycle, read loop, reconfigure
│   │       │   ├── prober.rs  # Auto-baud detection (smart probing)
│   │       │   └── reconnect.rs # Auto-reconnect on USB unplug/replug
│   │       ├── protocol/      # MAVLink v1/v2 decoding
│   │       ├── terminal/      # UI components (Leptos)
│   │       └── analysis/      # Buffer scoring (8N1, 7E1, MAVLink)
│   └── native/       # (future) Native terminal app
├── crates/
│   ├── core-types/   # Shared types and events
│   ├── framing/      # Frame detection (Lines, COBS, SLIP)
│   ├── decoders/     # Protocol decoders (MAVLink, NMEA)
│   ├── analysis/     # Scoring and auto-detection
│   └── transport-*/  # Serial transport layers
├── dev.sh            # Unified development script
├── clippy.toml       # Clippy configuration
├── rustfmt.toml      # Formatting configuration
├── CHANGELOG.md      # Version history
├── Claude.md         # AI assistant instructions (comprehensive guide)
├── DEVELOPMENT.md    # This file (workflow reference)
└── gemini.md         # Gemini AI instructions (same as DEVELOPMENT.md)
```

## Critical Architecture Patterns

### ⚠️ Connection State Machine (apps/web/src/connection/)

**ALWAYS use `atomic_state` as source of truth**, NEVER use Leptos `state` signal in critical decision points:

```rust
// ✅ CORRECT - Atomic source of truth
let current = manager.atomic_state.get();
if current == ConnectionState::Connected { /* ... */ }

// ❌ WRONG - Signal can lag behind atomic state
let current = manager.state.get_untracked();  // FORBIDDEN in critical paths
```

**Why:** `atomic_state` (AtomicU8) is lock-aware and uses SeqCst ordering. The Leptos `state` signal is reactive UI state that may lag.

### ⚠️ User Intent Flag

**ALWAYS check `user_initiated_disconnect` before auto-reconnect or auto-connection:**

```rust
// ✅ Required pattern
if manager.user_initiated_disconnect.get() {
    web_sys::console::log_1(&"Auto-action aborted - user initiated disconnect".into());
    return;
}
```

**Critical scenarios:** USB unplug during probing, device re-enumeration, auto-reconnect retry loops.

### ⚠️ Resource Cleanup

**ALWAYS clean up resources in error paths:**

```rust
// ✅ Complete cleanup on error
match port.open().await {
    Ok(_) => { /* success */ }
    Err(e) => {
        // Clean up ALL resources
        self.connection_handle.borrow_mut().take();
        self.active_port.borrow_mut().take();
        self.transition_to(ConnectionState::Disconnected);
        return Err(e.into());
    }
}
```

### ⚠️ String Allocation (Consistency)

```rust
// ✅ Consistent style - use .into()
set_status.set("Message".into());

// ❌ Inconsistent - avoid .to_string()
set_status.set("Message".to_string());
```

### ⚠️ Debug Logging

Use conditional compilation for debug logs:

```rust
#[cfg(debug_assertions)]
web_sys::console::log_1(&"Debug info".into());

// Production build strips this code entirely
```

**Never use unconditional debug logs** - they bloat production WASM bundles.

## For LLM Agents (Claude, Gemini, Cursor, Copilot)

### Workflow Commands

**IMPORTANT**: Always use `./dev.sh` for all operations:
- ✅ `./dev.sh test` - **RECOMMENDED** before committing (runs everything)
- ✅ `./dev.sh check` - Before making changes (fastest quality check)
- ✅ `./dev.sh build` - To verify production build
- ✅ `./dev.sh serve` - To test in browser at http://127.0.0.1:8080

**Never bypass** quality checks or use manual `cargo` commands for CI tasks.

### Code Modification Rules

**Before modifying code:**
1. Read the file first with Read tool - **understand existing patterns**
2. Check [CHANGELOG.md](CHANGELOG.md) - See if similar issue was fixed
3. Check [Claude.md](Claude.md) - Read comprehensive architecture guide
4. Use `atomic_state` not `state` signal in critical paths
5. Check `user_initiated_disconnect` flag before auto-actions
6. Clean up resources in ALL error paths

**After modifying code:**
1. Run `./dev.sh test` - Verify all checks pass
2. Update CHANGELOG.md if user-visible change
3. Ensure no `unwrap()`, `expect()`, `panic!()`, `todo!()`, or `arr[i]` indexing

### Common Pitfalls to Avoid

**❌ NEVER:**
- Use `.unwrap()`, `.expect()`, `panic!()`, `todo!()`, or `arr[index]`
- Read `state.get_untracked()` in critical decision points (use `atomic_state.get()`)
- Skip `user_initiated_disconnect` checks before auto-reconnect
- Forget resource cleanup in error paths (`connection_handle`, `active_port`)
- Use unconditional debug logs (always use `#[cfg(debug_assertions)]`)
- Create commits with failing tests

**✅ ALWAYS:**
- Use `.ok()`, `.ok_or()`, or `?` operator for error handling
- Use `.get(index)` instead of `[index]` for slicing
- Read atomic state: `manager.atomic_state.get()`
- Clean up resources: `self.connection_handle.borrow_mut().take()`
- Run `./dev.sh test` before commit

### Testing Requirements

**All changes must include tests:**
- Unit tests for pure logic (native Rust)
- wasm-bindgen tests for WebSerial integration (browser)
- Property-based tests for state machine invariants (use `proptest`)

**Test count should not decrease** - add tests, never remove them (unless removing code).

### Documentation

For detailed architecture, state machine design, and critical patterns, see:
- **[Claude.md](Claude.md)** - Comprehensive AI assistant guide
- **[CHANGELOG.md](CHANGELOG.md)** - All fixes and technical decisions
- **Code review report** - `/tmp/connection_code_review_final.md` (if available)
- **Plan file** - `~/.claude/plans/sorted-booping-shell.md` (if available)

All checks are enforced to prevent panic-inducing code patterns.
