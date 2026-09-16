# Directive: Storage Scan and Classification

## Goal
Ensure filesystem scanning (`scan.rs`) and 9-category human classification (`classify.rs`) remain byte-exact, non-blocking, and crash-resilient.

## Categories
1. Applications
2. Documents
3. Videos
4. Pictures
5. Music
6. Downloads
7. Temporary files
8. Trash
9. Other

## Strict Equation Rule
The byte-exact audit equation must always hold:
`total_bytes == sum(categories[0..8].bytes)`
No file may be double-counted or lost in rounding.

## Edge Cases
- Permission denied paths (`/root`, restricted subfolders) must be tracked and reported, not panic.
- Symlinks must NOT be followed into loops.
- XDG user directories override default paths when configured in `~/.config/user-dirs.dirs`.
