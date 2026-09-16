# Directives (Layer 1: What To Do)

This directory contains Standard Operating Procedures (SOPs) written in Markdown.

## Principles
1. **Clear Objectives**: Each directive describes the goal, inputs, execution tools, expected outputs, and edge cases.
2. **Deterministic Delegation**: When work needs to be done, the LLM refers to the directive and invokes deterministic Python scripts in `execution/`.
3. **Self-Annealing**: Directives are living documents. When bugs, edge cases, or optimizations are discovered, directives must be updated with the learnings.

## Available Directives
- `test_and_build.md`: Running tests, clippy lints, and build checks.
- `storage_scan.md`: Filesystem scanning, classification verification, and byte audit.
- `cleanup_actions.md`: Safe deletion, cache clearing, and trash emptying.
- `btrfs_snapper.md`: Inspecting and managing Btrfs allocations and Snapper snapshots.
