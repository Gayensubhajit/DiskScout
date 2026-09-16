# Directive: Test and Build Verification

## Goal
Verify the integrity of the Rust + Slint codebase, ensure all unit/integration tests pass, check for compiler warnings and clippy lints.

## Tools & Execution Scripts
- Execution script: `execution/verify_build.py`
- Underlying commands:
  - `cargo test`
  - `cargo check`
  - `cargo clippy -- -D warnings`

## Steps
1. Run `python3 execution/verify_build.py`.
2. Inspect test counts and any clippy suggestions.
3. If failures occur:
   - Read compiler error messages and stack traces.
   - Fix source code in `src/` or `ui/`.
   - Re-run verification until all tests pass with zero warnings.
