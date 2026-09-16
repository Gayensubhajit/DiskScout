# Directive: Btrfs & Snapper Snapshot Inspection

## Goal
Detect and visualize hidden disk space consumption caused by Btrfs subvolumes, snapshot accumulation, and unallocated chunk space (the core issue from Garuda Linux).

## Tools & Execution Scripts
- Execution script: `execution/btrfs_inspect.py`

## Inspection Checks
1. **Filesystem Type Detection**:
   - Check if `/` or `/home` is formatted as Btrfs.
2. **Btrfs Usage Breakdown**:
   - Used data vs. Device allocated vs. Unallocated space.
   - Metadata and GlobalReserve stats (`btrfs filesystem usage /`).
3. **Snapper Snapshots**:
   - Query `snapper -c root list` and `snapper -c home list`.
   - Check snapshot sizes in `/.snapshots` and `/home/.snapshots`.
4. **Systemd Snapshot Timers**:
   - Inspect active timers: `dusky_snapshot.timer`, `snapper-timeline.timer`, `snapper-cleanup.timer`.
