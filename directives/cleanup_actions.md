# Directive: Safe Cleanup and File Deletion

## Goal
Provide safe, predictable file deletion and cache cleanup matching user expectations ("delete like windows file").

## Tools & Execution Scripts
- Execution script: `execution/safe_cleanup.py`

## Principles
1. **Never Irreversibly Delete Without Confirmation**:
   - High-impact deletions require user-facing confirmation modals.
2. **Trash Preferred Over Permanent Unlink**:
   - Normal file deletion moves to XDG Trash (`~/.local/share/Trash` / `gio trash`).
3. **Safe Cache Targets**:
   - `~/.cache` subdirectories (thumbnails, browser cache, app caches).
   - Package manager caches (e.g. `/var/cache/pacman/pkg`) must only clean uninstalled/old versions.
4. **Post-Cleanup Rescan**:
   - Whenever cleanup runs, a refresh scan must be triggered to update dashboard capacity bars and category numbers.
