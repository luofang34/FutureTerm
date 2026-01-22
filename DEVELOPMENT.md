# Development Workflow

## Philosophy
**Safety First**: Enforce strict code quality through automated checks.
**No Panic Production**: Zero unwrap/panic/unsafe in WASM code.
**Agent-Friendly**: Single unified script for all development tasks.

## Quick Start

All development operations use a single script: **`./dev.sh`**

```bash
./dev.sh check      # Run quality checks (fmt, clippy)
./dev.sh test       # Run checks + unit tests
./dev.sh build      # Run checks + tests + release build
./dev.sh serve      # Run checks + start dev server (default)
```

**Default**: Running `./dev.sh` without arguments starts the dev server.

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

Runs unit tests for all non-WASM crates:
- `core-types`: Serialization, event types
- `framing`: Line splitting, COBS, SLIP
- `decoders`: MAVLink, NMEA parsing
- `analysis`: Scoring algorithms

**Note**: WASM crates (`transport-webserial`, `app-web`) excluded from CLI tests.

## Building

```bash
./dev.sh build
```

1. Runs quality checks
2. Runs all tests
3. Builds release WASM bundle with Trunk
4. Output: `apps/web/dist/`

## Development Server

```bash
./dev.sh serve
# or simply
./dev.sh
```

Starts Trunk dev server at **http://127.0.0.1:8080**

**Features**:
- Hot reload on file changes
- WASM compilation with optimized flags
- Source maps for debugging

**Testing**: Connect to real serial devices through browser WebSerial API.

## CI/CD Integration

GitHub Actions workflow (`.github/workflows/ci.yml`) uses `./dev.sh`:

```yaml
- name: Run tests
  run: ./dev.sh test

- name: Build WASM
  run: ./dev.sh build
```

Ensures CI uses identical checks as local development.

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
└── CHANGELOG.md      # Version history
```

## For LLM Agents

**IMPORTANT**: Always use `./dev.sh` for all operations:
- ✅ `./dev.sh check` before making changes
- ✅ `./dev.sh test` before committing
- ✅ `./dev.sh build` to verify production build
- ✅ `./dev.sh serve` to test in browser

**Never bypass** quality checks or use manual `cargo` commands for CI tasks.

All checks are enforced to prevent panic-inducing code patterns.
