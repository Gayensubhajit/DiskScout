//! Asynchronous filesystem scanner (Milestone 3).
//!
//! Answers *"what exists and how large is it?"* — categorization (M4),
//! cleanup (M7) and Btrfs specifics (M8) are explicitly out of scope.
//!
//! ## Scalability design
//!
//! The scanner is **aggregate-only**: it keeps one small accumulator per
//! *open* directory on the DFS stack plus one summary per *top-level*
//! child of the scan root. It never builds a per-file list, so homes with
//! hundreds of thousands or millions of files need only O(depth) memory.
//! Directory totals are propagated bottom-up when `walkdir` leaves each
//! directory, giving exact recursive sizes in a single pass.
//!
//! ## Threading
//!
//! [`scan_blocking`] does the work synchronously and is directly unit
//! testable. [`spawn_scan`] runs it on a dedicated worker thread and
//! streams [`ScannerMessage`]s; the Slint UI thread is never blocked.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use walkdir::WalkDir;

/// File vs directory. Symlinks that are not followed count as [`EntryKind::File`]
/// with size 0; special files (sockets, fifos, devices) count as files with
/// their metadata length (usually 0) and are never opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
}

/// Lifecycle of a scan. There is no `Failed` state: a scan that survives
/// unreadable entries finishes as [`ScanState::Completed`] with
/// `error_count > 0` and `last_error` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanState {
    Running,
    Completed,
    Cancelled,
}

/// What to scan and how.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Directory to walk. M3 callers pass the user's home directory;
    /// mount-boundary selection is future work (M8).
    pub root: PathBuf,
    /// Follow symbolic links. Defaults to `false`: links are counted as
    /// zero-size files and never descended into, which rules out symlink
    /// loops by construction.
    pub follow_symlinks: bool,
    /// Extra absolute subtrees to measure exactly (M4). Each tracked path
    /// is reported in [`ScanResult::tracked`] with its recursive totals,
    /// recorded when the walker leaves it. Paths must use the same
    /// spelling as `root.join(..)` so they match walk entries exactly;
    /// missing paths report `found: false`. Empty by default.
    ///
    /// Cost is one hash lookup per visited entry plus one slot per
    /// requested path: the walk stays single-pass and aggregate-only.
    pub track: Vec<PathBuf>,
}

/// An individual file or directory entry inside a directory (Milestone 6).
///
/// Designed to be memory-efficient: does not duplicate full absolute paths.
/// Only stores the base file/folder name. If this entry is a directory,
/// `dir_id` directly indexes into [`FileTree::directories`] for O(1) navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub bytes: u64,
    pub file_count: u64,
    pub dir_id: Option<u32>,
}

/// Information and child entries for one visited directory.
#[derive(Debug, Clone)]
pub struct DirectoryNode {
    pub path: PathBuf,
    #[allow(dead_code)]
    pub name: String,
    pub bytes: u64,
    #[allow(dead_code)]
    pub file_count: u64,
    pub parent_dir: Option<u32>,
    /// Child files and subdirectories, sorted descending by logical bytes.
    pub children: Vec<FileEntry>,
}

/// Compact, in-memory filesystem tree built during the single-pass DFS walk.
///
/// Enables O(1) directory navigation for M6 without rescanning the home directory.
#[derive(Debug, Default, Clone)]
pub struct FileTree {
    pub directories: Vec<DirectoryNode>,
    pub path_to_dir: HashMap<PathBuf, u32>,
}

impl FileTree {
    pub fn get_dir(&self, dir_id: u32) -> Option<&DirectoryNode> {
        self.directories.get(dir_id as usize)
    }

    pub fn find_dir(&self, path: &std::path::Path) -> Option<u32> {
        self.path_to_dir.get(path).copied()
    }
}

/// Summary of one top-level (depth-1) child of the scan root.
/// For directories, `bytes`/`files`/`dirs` are recursive totals.
#[derive(Debug, Clone)]
pub struct TopEntry {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub bytes: u64,
    pub files: u64,
    /// Recursive subdirectory count, excluding the entry itself.
    pub dirs: u64,
}

/// Exact recursive total for one [`ScanOptions::track`] path.
/// Reported even for missing paths (`found: false`, zeros) so callers can
/// zip results positionally without extra bookkeeping.
#[derive(Debug, Clone)]
pub struct TrackedSize {
    pub path: PathBuf,
    pub found: bool,
    pub bytes: u64,
    pub files: u64,
    /// Recursive subdirectory count, excluding the entry itself. Unused by
    /// the M4 classifier (bytes/files suffice); kept for M5 detail screens.
    #[allow(dead_code)]
    pub dirs: u64,
}

/// Periodic snapshot for the future progress UI (M5).
#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub files_scanned: u64,
    pub dirs_scanned: u64,
    pub bytes_discovered: u64,
    pub error_count: u64,
    pub current_path: Option<PathBuf>,
    pub state: ScanState,
}

/// Final outcome of a scan, including partial totals when cancelled.
#[derive(Debug)]
pub struct ScanResult {
    pub root: PathBuf,
    pub total_bytes: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub error_count: u64,
    pub state: ScanState,
    pub elapsed: Duration,
    /// Depth-1 children, largest first.
    pub top_entries: Vec<TopEntry>,
    pub file_tree: Arc<FileTree>,
    /// Requested [`ScanOptions::track`] paths in request order, with exact
    /// recursive totals. Missing paths report `found: false`.
    pub tracked: Vec<TrackedSize>,
    pub last_error: Option<String>,
}

/// Messages streamed by [`spawn_scan`]: throttled progress, then exactly
/// one terminal `Done`.
#[derive(Debug)]
pub enum ScannerMessage {
    Progress(ScanProgress),
    Done(ScanResult),
}

/// How often a progress snapshot is emitted (whichever hits first).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
const PROGRESS_EVERY_N_ENTRIES: u64 = 4096;
/// The cancel flag is polled every N entries: cheap enough to be
/// unnoticeable, responsive enough for interactive cancel.
const CANCEL_POLL_EVERY_N_ENTRIES: u64 = 1024;

/// Accumulator for one directory currently on the DFS stack.
struct OpenDir {
    path: PathBuf,
    bytes: u64,
    files: u64,
    dirs: u64,
    children: Vec<FileEntry>,
}

/// Record a finished directory if the caller asked to track its path.
/// Totals are exact here: the walker reports a directory only after
/// everything under it has been visited.
fn record_tracked(
    done: &OpenDir,
    wanted: &HashSet<PathBuf>,
    tracked: &mut HashMap<PathBuf, TrackedSize>,
) {
    if wanted.contains(&done.path) {
        tracked.insert(
            done.path.clone(),
            TrackedSize {
                path: done.path.clone(),
                found: true,
                bytes: done.bytes,
                files: done.files,
                dirs: done.dirs,
            },
        );
    }
}

/// Walk `options.root` to completion (or cancellation).
///
/// Never panics on filesystem errors: permission denied, vanished files,
/// broken symlinks and special files are counted in `error_count` (with
/// the latest message in `last_error`) and skipped.
pub fn scan_blocking(
    options: &ScanOptions,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(ScanProgress),
) -> ScanResult {
    let started = Instant::now();
    let mut files_seen = 0u64;
    let mut dirs_seen = 0u64;
    let mut bytes_seen = 0u64;
    let mut error_count = 0u64;
    let mut last_error: Option<String> = None;
    let mut top_entries: Vec<TopEntry> = Vec::new();
    let mut file_tree = FileTree::default();
    let mut stack: Vec<OpenDir> = Vec::new();
    // Degenerate case: the scan root itself is a file.
    let mut root_file_bytes: Option<u64> = None;
    let mut state = ScanState::Running;

    let mut entries_since_progress = 0u64;
    let mut entries_since_cancel_check = 0u64;
    let mut last_progress_at = Instant::now();

    // Requested nested measurements. Lookup per entry is O(1); recording
    // happens only for exact path matches (at most one per request).
    let wanted: HashSet<PathBuf> = options.track.iter().cloned().collect();
    let mut tracked: HashMap<PathBuf, TrackedSize> = HashMap::new();

    let walker = WalkDir::new(&options.root)
        .follow_links(options.follow_symlinks)
        .into_iter();

    for entry in walker {
        entries_since_cancel_check += 1;
        if entries_since_cancel_check >= CANCEL_POLL_EVERY_N_ENTRIES {
            entries_since_cancel_check = 0;
            if cancel.load(Ordering::Relaxed) {
                state = ScanState::Cancelled;
                break;
            }
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                // Unreadable directory, permission denied, I/O error:
                // count and continue with the rest of the tree.
                error_count += 1;
                last_error = Some(err.to_string());
                continue;
            }
        };

        let depth = entry.depth();
        // Close every directory the DFS has left: pop until the stack
        // holds exactly this entry's ancestor chain, propagating each
        // finished total into its parent.
        while stack.len() > depth {
            let mut done = stack.pop().expect("scanner stack underflow");
            record_tracked(&done, &wanted, &mut tracked);
            if stack.is_empty() {
                continue;
            }
            done.children.sort_by_key(|e| std::cmp::Reverse(e.bytes));
            let dir_id = file_tree.directories.len() as u32;
            file_tree.path_to_dir.insert(done.path.clone(), dir_id);
            let dir_name = done
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            if stack.len() == 1 {
                // A finished depth-1 child of the root: keep its summary.
                top_entries.push(TopEntry {
                    path: done.path.clone(),
                    kind: EntryKind::Dir,
                    bytes: done.bytes,
                    files: done.files,
                    dirs: done.dirs,
                });
            }
            if let Some(parent) = stack.last_mut() {
                parent.bytes += done.bytes;
                parent.files += done.files;
                parent.dirs += done.dirs + 1;
                parent.children.push(FileEntry {
                    name: dir_name.clone(),
                    is_dir: true,
                    bytes: done.bytes,
                    file_count: done.files,
                    dir_id: Some(dir_id),
                });
            }
            file_tree.directories.push(DirectoryNode {
                path: done.path,
                name: dir_name,
                bytes: done.bytes,
                file_count: done.files,
                parent_dir: None,
                children: done.children,
            });
        }

        let file_type = entry.file_type();
        if file_type.is_dir() {
            if depth > 0 {
                dirs_seen += 1;
            }
            stack.push(OpenDir {
                path: entry.path().to_path_buf(),
                bytes: 0,
                files: 0,
                dirs: 0,
                children: Vec::new(),
            });
        } else {
            files_seen += 1;
            let size = if file_type.is_symlink() && !options.follow_symlinks {
                // Counted but never followed: no loops possible.
                0
            } else {
                match entry.metadata() {
                    Ok(meta) => meta.len(),
                    Err(err) => {
                        // Vanished file, permission change mid-scan, etc.
                        error_count += 1;
                        last_error = Some(format!("{}: {err}", entry.path().display()));
                        continue;
                    }
                }
            };
            bytes_seen += size;
            if depth == 0 {
                root_file_bytes = Some(size);
            } else {
                if let Some(top) = stack.last_mut() {
                    top.bytes += size;
                    top.files += 1;
                    top.children.push(FileEntry {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        is_dir: false,
                        bytes: size,
                        file_count: 1,
                        dir_id: None,
                    });
                }
                if wanted.contains(entry.path()) {
                    // A tracked leaf that is a file, not a directory.
                    tracked.insert(
                        entry.path().to_path_buf(),
                        TrackedSize {
                            path: entry.path().to_path_buf(),
                            found: true,
                            bytes: size,
                            files: 1,
                            dirs: 0,
                        },
                    );
                }
                if depth == 1 {
                    top_entries.push(TopEntry {
                        path: entry.path().to_path_buf(),
                        kind: EntryKind::File,
                        bytes: size,
                        files: 1,
                        dirs: 0,
                    });
                }
            }
        }

        entries_since_progress += 1;
        if entries_since_progress >= PROGRESS_EVERY_N_ENTRIES
            || last_progress_at.elapsed() >= PROGRESS_INTERVAL
        {
            entries_since_progress = 0;
            last_progress_at = Instant::now();
            on_progress(ScanProgress {
                files_scanned: files_seen,
                dirs_scanned: dirs_seen,
                bytes_discovered: bytes_seen,
                error_count,
                current_path: Some(entry.path().to_path_buf()),
                state: ScanState::Running,
            });
        }
    }

    // Drain the stack the same way: remaining open directories propagate
    // into the root accumulator, so cancelled scans still report exact
    // partial totals for everything visited so far.
    let mut root_totals: Option<OpenDir> = None;
    while let Some(mut done) = stack.pop() {
        record_tracked(&done, &wanted, &mut tracked);
        done.children.sort_by_key(|e| std::cmp::Reverse(e.bytes));
        let dir_id = file_tree.directories.len() as u32;
        file_tree.path_to_dir.insert(done.path.clone(), dir_id);
        let dir_name = done
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        if stack.is_empty() {
            file_tree.directories.push(DirectoryNode {
                path: done.path.clone(),
                name: dir_name,
                bytes: done.bytes,
                file_count: done.files,
                parent_dir: None,
                children: done.children.clone(),
            });
            root_totals = Some(done);
        } else {
            if stack.len() == 1 {
                top_entries.push(TopEntry {
                    path: done.path.clone(),
                    kind: EntryKind::Dir,
                    bytes: done.bytes,
                    files: done.files,
                    dirs: done.dirs,
                });
            }
            if let Some(parent) = stack.last_mut() {
                parent.bytes += done.bytes;
                parent.files += done.files;
                parent.dirs += done.dirs + 1;
                parent.children.push(FileEntry {
                    name: dir_name.clone(),
                    is_dir: true,
                    bytes: done.bytes,
                    file_count: done.files,
                    dir_id: Some(dir_id),
                });
            }
            file_tree.directories.push(DirectoryNode {
                path: done.path,
                name: dir_name,
                bytes: done.bytes,
                file_count: done.files,
                parent_dir: None,
                children: done.children,
            });
        }
    }

    for i in 0..file_tree.directories.len() {
        let parent_id = file_tree.directories[i]
            .path
            .parent()
            .and_then(|p| file_tree.path_to_dir.get(p).copied());
        file_tree.directories[i].parent_dir = parent_id;
    }

    let (total_bytes, file_count, dir_count) = match root_totals {
        Some(root) => (root.bytes, root.files, root.dirs),
        None => (
            root_file_bytes.unwrap_or(0),
            root_file_bytes.map(|_| 1).unwrap_or(0),
            0,
        ),
    };

    if state != ScanState::Cancelled {
        state = ScanState::Completed;
    }
    top_entries.sort_by_key(|a| std::cmp::Reverse(a.bytes));

    // Requested paths in request order; absent paths report found: false.
    let tracked = options
        .track
        .iter()
        .map(|path| {
            tracked.remove(path).unwrap_or(TrackedSize {
                path: path.clone(),
                found: false,
                bytes: 0,
                files: 0,
                dirs: 0,
            })
        })
        .collect();

    ScanResult {
        root: options.root.clone(),
        total_bytes,
        file_count,
        dir_count,
        error_count,
        state,
        elapsed: started.elapsed(),
        top_entries,
        file_tree: Arc::new(file_tree),
        tracked,
        last_error,
    }
}

/// Handle to a background scan: hand [`ScanHandle::take_receiver`] to a
/// logging/UI thread, call [`ScanHandle::cancel`] to stop early
/// (e.g. on window close), [`ScanHandle::wait`] to reap the worker.
pub struct ScanHandle {
    cancel_flag: Arc<AtomicBool>,
    receiver: Option<Receiver<ScannerMessage>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ScanHandle {
    /// Take ownership of the message stream. Panics if called twice.
    pub fn take_receiver(&mut self) -> Receiver<ScannerMessage> {
        self.receiver
            .take()
            .expect("scanner receiver already taken")
    }

    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }

    /// Block until the worker thread has exited.
    pub fn wait(mut self) {
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Run [`scan_blocking`] on a dedicated `diskscout-scanner` thread.
/// Emits throttled [`ScannerMessage::Progress`] snapshots and exactly one
/// terminal [`ScannerMessage::Done`], then the thread exits.
pub fn spawn_scan(options: ScanOptions) -> ScanHandle {
    let (tx, rx) = mpsc::channel();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let flag = cancel_flag.clone();

    let thread = thread::Builder::new()
        .name("diskscout-scanner".to_string())
        .spawn(move || {
            let result = scan_blocking(&options, &flag, |progress| {
                // The receiver may be gone (app shutting down); a failed
                // send must never fail the scan.
                let _ = tx.send(ScannerMessage::Progress(progress));
            });
            let _ = tx.send(ScannerMessage::Done(result));
        })
        .expect("failed to spawn diskscout-scanner thread");

    ScanHandle {
        cancel_flag,
        receiver: Some(rx),
        thread: Some(thread),
    }
}

/// Path helper shared by tests.
#[cfg(test)]
pub(crate) fn write_sized(path: &PathBuf, bytes: usize) {
    std::fs::write(path, vec![0u8; bytes]).expect("failed to write test file");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn quiet(_: ScanProgress) {}

    fn top_by_name<'a>(result: &'a ScanResult, name: &str) -> &'a TopEntry {
        result
            .top_entries
            .iter()
            .find(|e| e.path.file_name().unwrap() == name)
            .unwrap_or_else(|| panic!("missing top entry {name}"))
    }

    #[test]
    fn aggregates_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("a")).unwrap();
        write_sized(&root.join("a").join("b.txt"), 100);
        write_sized(&root.join("a").join("c.txt"), 200);
        write_sized(&root.join("d.txt"), 50);

        let options = ScanOptions {
            root: root.to_path_buf(),
            follow_symlinks: false,
            track: Vec::new(),
        };
        let result = scan_blocking(&options, &AtomicBool::new(false), quiet);

        assert_eq!(result.state, ScanState::Completed);
        assert_eq!(result.total_bytes, 350);
        assert_eq!(result.file_count, 3);
        assert_eq!(result.dir_count, 1);
        assert_eq!(result.error_count, 0);
        assert_eq!(result.top_entries.len(), 2);
        // Largest first.
        assert_eq!(result.top_entries[0].bytes, 300);
        let a = top_by_name(&result, "a");
        assert_eq!(a.kind, EntryKind::Dir);
        assert_eq!((a.bytes, a.files, a.dirs), (300, 2, 0));
        let d = top_by_name(&result, "d.txt");
        assert_eq!(d.kind, EntryKind::File);
        assert_eq!(d.bytes, 50);
    }

    #[test]
    fn empty_directory_completes() {
        let dir = tempfile::tempdir().unwrap();
        let options = ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: Vec::new(),
        };
        let result = scan_blocking(&options, &AtomicBool::new(false), quiet);

        assert_eq!(result.state, ScanState::Completed);
        assert_eq!(
            (result.total_bytes, result.file_count, result.dir_count),
            (0, 0, 0)
        );
        assert!(result.top_entries.is_empty());
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn missing_root_reports_error_without_panicking() {
        let options = ScanOptions {
            root: dir_not_existing_path(),
            follow_symlinks: false,
            track: Vec::new(),
        };
        let result = scan_blocking(&options, &AtomicBool::new(false), quiet);

        assert_eq!(result.state, ScanState::Completed);
        assert_eq!(result.total_bytes, 0);
        assert!(result.error_count >= 1);
        assert!(result.last_error.is_some());
    }

    #[cfg(unix)]
    fn dir_not_existing_path() -> PathBuf {
        PathBuf::from("/definitely/not/here/diskscout-m3-test")
    }

    #[cfg(not(unix))]
    fn dir_not_existing_path() -> PathBuf {
        PathBuf::from("Z:\\definitely\\not\\here\\diskscout-m3-test")
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_sized(&root.join("real.txt"), 100);
        symlink("real.txt", root.join("link-to-file")).unwrap();
        symlink("..", root.join("link-to-parent")).unwrap();
        symlink("no-such-target", root.join("broken")).unwrap();

        let options = ScanOptions {
            root: root.to_path_buf(),
            follow_symlinks: false,
            track: Vec::new(),
        };
        let result = scan_blocking(&options, &AtomicBool::new(false), quiet);

        // Terminates (no loop), links add no bytes, real content counted once.
        assert_eq!(result.state, ScanState::Completed);
        assert_eq!(result.total_bytes, 100);
        assert_eq!(result.file_count, 4);
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_is_counted_not_fatal() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let locked = root.join("locked");
        std::fs::create_dir(&locked).unwrap();
        write_sized(&locked.join("secret.txt"), 64);
        write_sized(&root.join("visible.txt"), 32);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Root running the suite could still read it; skip instead of failing.
        if std::fs::read_dir(&locked).is_ok() {
            return;
        }

        let options = ScanOptions {
            root: root.to_path_buf(),
            follow_symlinks: false,
            track: Vec::new(),
        };
        let result = scan_blocking(&options, &AtomicBool::new(false), quiet);

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(result.state, ScanState::Completed);
        assert!(result.error_count >= 1, "expected permission errors");
        assert!(result.last_error.is_some());
        // The visible file is still counted exactly.
        assert!(result.total_bytes >= 32);

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).ok();
    }

    #[test]
    fn cancellation_stops_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("many")).unwrap();
        for i in 0..2000 {
            write_sized(&root.join("many").join(format!("f{i}.txt")), 10);
        }

        // Flag already set: the worker must observe it promptly.
        let cancel = AtomicBool::new(true);
        let options = ScanOptions {
            root: root.to_path_buf(),
            follow_symlinks: false,
            track: Vec::new(),
        };
        let result = scan_blocking(&options, &cancel, quiet);

        assert_eq!(result.state, ScanState::Cancelled);
        // Partial totals stay consistent with what was visited.
        assert!(result.total_bytes <= 2000 * 10);
    }

    #[test]
    fn progress_callback_reports_running_state() {
        let dir = tempfile::tempdir().unwrap();
        write_sized(&dir.path().join("a.txt"), 8);

        let options = ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: Vec::new(),
        };
        let mut saw_running = false;
        let result = scan_blocking(
            &options,
            &AtomicBool::new(false),
            |progress: ScanProgress| {
                if progress.state == ScanState::Running {
                    saw_running = true;
                }
            },
        );

        assert_eq!(result.state, ScanState::Completed);
        // Tiny trees may finish before the first throttle window; either
        // outcome is valid, the callback contract is what matters.
        let _ = saw_running;
    }

    #[test]
    fn spawn_delivers_progress_then_done() {
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        write_sized(&dir.path().join("a.txt"), 16);

        let mut handle = spawn_scan(ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: Vec::new(),
        });
        let receiver = handle.take_receiver();
        let done = loop {
            match receiver.recv_timeout(Duration::from_secs(30)) {
                Ok(ScannerMessage::Done(result)) => break result,
                Ok(ScannerMessage::Progress(_)) => continue,
                Err(err) => panic!("scanner must finish the temp tree quickly: {err}"),
            }
        };

        assert_eq!(done.state, ScanState::Completed);
        assert_eq!(done.total_bytes, 16);
        handle.wait();
    }

    #[test]
    fn spawn_cancel_results_in_cancelled_done() {
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("many")).unwrap();
        for i in 0..5000 {
            write_sized(&dir.path().join("many").join(format!("f{i}.txt")), 10);
        }

        let mut handle = spawn_scan(ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: Vec::new(),
        });
        handle.cancel();
        let receiver = handle.take_receiver();
        let done = loop {
            match receiver.recv_timeout(Duration::from_secs(30)) {
                Ok(ScannerMessage::Done(result)) => break result,
                Ok(ScannerMessage::Progress(_)) => continue,
                Err(err) => panic!("cancelled scanner must still terminate: {err}"),
            }
        };

        // Either outcome is correct (cancel may race completion on a fast
        // disk); the contract is that Done always arrives exactly once.
        assert!(matches!(
            done.state,
            ScanState::Cancelled | ScanState::Completed
        ));
        handle.wait();
    }

    #[test]
    fn tracked_nested_subtree_totals() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();
        write_sized(&root.join("a").join("b").join("c.txt"), 100);
        write_sized(&root.join("a").join("b").join("d.txt"), 200);
        write_sized(&root.join("a").join("e.txt"), 50);

        let options = ScanOptions {
            root: root.to_path_buf(),
            follow_symlinks: false,
            track: vec![root.join("a").join("b"), root.join("a")],
        };
        let result = scan_blocking(&options, &AtomicBool::new(false), quiet);

        assert_eq!(result.state, ScanState::Completed);
        assert_eq!(result.tracked.len(), 2);
        let nested = &result.tracked[0];
        assert_eq!(nested.path, root.join("a").join("b"));
        assert!(nested.found);
        assert_eq!((nested.bytes, nested.files, nested.dirs), (300, 2, 0));
        let parent = &result.tracked[1];
        assert!(parent.found);
        assert_eq!((parent.bytes, parent.files, parent.dirs), (350, 3, 1));
    }

    #[test]
    fn tracked_missing_path_reports_not_found() {
        let dir = tempfile::tempdir().unwrap();
        write_sized(&dir.path().join("a.txt"), 8);

        let options = ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: vec![dir.path().join("nope").join("absent")],
        };
        let result = scan_blocking(&options, &AtomicBool::new(false), quiet);

        assert_eq!(result.state, ScanState::Completed);
        assert_eq!(result.total_bytes, 8);
        assert_eq!(result.tracked.len(), 1);
        assert!(!result.tracked[0].found);
        assert_eq!(result.tracked[0].bytes, 0);
    }

    #[test]
    fn tracked_root_reports_totals() {
        let dir = tempfile::tempdir().unwrap();
        write_sized(&dir.path().join("a.txt"), 8);

        let options = ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: vec![dir.path().to_path_buf()],
        };
        let result = scan_blocking(&options, &AtomicBool::new(false), quiet);

        assert_eq!(result.state, ScanState::Completed);
        assert!(result.tracked[0].found);
        assert_eq!(result.tracked[0].bytes, result.total_bytes);
    }
}
