mod platform;
mod scan;
mod storage;

#[cfg(test)]
mod shell_test;

use storage::{StorageInfo, format_bytes};

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    // Native Wayland identity (taskbar / window rules). Must precede
    // window creation. Best-effort: a missing compositor must not be fatal.
    let _ = slint::set_xdg_app_id("diskscout");

    let app = AppWindow::new()?;

    wire_titlebar(&app);
    sync_geometry(&app);

    // Keep the Rust-driven scroll geometry in sync with the native window
    // size. A repeated event-loop timer (not a thread) observes resizes;
    // values are pushed only when the size actually changed.
    let geometry_timer = slint::Timer::default();
    {
        let weak = app.as_weak();
        let last = std::rc::Rc::new(std::cell::Cell::new(None::<(u32, u32)>));
        geometry_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(250),
            move || {
                if let Some(app) = weak.upgrade() {
                    let size = app.window().size();
                    let current = (size.width, size.height);
                    if last.get() != Some(current) {
                        last.set(Some(current));
                        sync_geometry(&app);
                    }
                }
            },
        );
    }
    // Milestone 2: replace mocked capacity with the real filesystem
    // containing `$HOME`. Fast synchronous `statvfs`-backed query, so the
    // UI stays responsive. Errors surface in the status row, never panic.
    match storage::query_home_filesystem() {
        Ok(info) => apply_storage(&app, &info),
        Err(err) => {
            eprintln!("diskscout: storage query failed: {err:#}");
            apply_storage(&app, &StorageInfo::unavailable());
            app.set_drive_name("Local Disk".into());
            app.set_status_text(format!("Could not read storage: {err}").into());
            app.set_status_ok(false);
        }
    }

    // Milestone 3: scan `$HOME` on a worker thread. Progress and the final
    // summary go to the log for now; the dashboard keeps its mocked
    // categories until M4/M5 and is never blocked by the scan.
    // The scan is cancelled when the window closes.
    let scanner = match scan::ScanOptions::home() {
        Ok(options) => {
            eprintln!("diskscout: scanning {}", options.root.display());
            let mut handle = scan::spawn_scan(options);
            let receiver = handle.take_receiver();
            std::thread::Builder::new()
                .name("diskscout-scan-log".to_string())
                .spawn(move || {
                    for message in receiver {
                        match message {
                            scan::ScannerMessage::Progress(progress) => {
                                eprintln!(
                                    "diskscout: scan {:?} {} files, {} dirs, {}, {} errors at {}",
                                    progress.state,
                                    progress.files_scanned,
                                    progress.dirs_scanned,
                                    format_bytes(progress.bytes_discovered),
                                    progress.error_count,
                                    progress
                                        .current_path
                                        .as_deref()
                                        .unwrap_or(std::path::Path::new("?"))
                                        .display()
                                );
                            }
                            scan::ScannerMessage::Done(result) => {
                                eprintln!(
                                    "diskscout: scan of {} {:?} in {:.1}s: {} files, {} dirs, {}, {} errors",
                                    result.root.display(),
                                    result.state,
                                    result.elapsed.as_secs_f64(),
                                    result.file_count,
                                    result.dir_count,
                                    format_bytes(result.total_bytes),
                                    result.error_count
                                );
                                for entry in result.top_entries.iter().take(10) {
                                    eprintln!(
                                        "diskscout: top {:?} {}: {} files, {} dirs in {}",
                                        entry.kind,
                                        format_bytes(entry.bytes),
                                        entry.files,
                                        entry.dirs,
                                        entry.path.display()
                                    );
                                }
                                if let Some(err) = &result.last_error {
                                    eprintln!("diskscout: last scan error: {err}");
                                }
                                break;
                            }
                        }
                    }
                })
                .expect("failed to spawn diskscout-scan-log thread");
            Some(handle)
        }
        Err(err) => {
            eprintln!("diskscout: scanner not started: {err:#}");
            None
        }
    };

    app.run()?;

    if let Some(handle) = scanner {
        handle.cancel();
        handle.wait();
    }
    Ok(())
}

/// Client-side titlebar wiring (Ryzora-style shell). Every control maps to
/// a REAL native `slint::Window` operation; nothing is visually simulated:
/// - minimize -> `set_minimized(true)`
/// - maximize/restore -> `set_maximized(!is_maximized())`, icon synced back
/// - close -> `hide()`, which ends `run()` and flows into the existing
///   scanner cancel + wait path below (never bypassed)
/// - drag -> `set_position` from pointer deltas. This is best-effort:
///   Wayland compositors (Hyprland) ignore client positioning by design,
///   so dragging there stays a window-manager gesture (SUPER+drag, tiling).
///   Where client positioning is allowed (X11, Windows) it moves the window.
fn wire_titlebar(app: &AppWindow) {
    let weak = app.as_weak();
    app.on_minimize_requested(move || {
        if let Some(app) = weak.upgrade() {
            app.window().set_minimized(true);
        }
    });

    let weak = app.as_weak();
    app.on_maximize_requested(move || {
        if let Some(app) = weak.upgrade() {
            let window = app.window();
            let next = !window.is_maximized();
            window.set_maximized(next);
            // Optimistic icon sync; the compositor applies the state change
            // asynchronously. Slint exposes no maximized-changed signal, so
            // WM-initiated changes made outside this button won't reflect.
            app.set_titlebar_maximized(next);
        }
    });

    let weak = app.as_weak();
    app.on_close_requested(move || {
        if let Some(app) = weak.upgrade() {
            // Ends the event loop; main() then cancels the scanner and
            // waits for the worker before exiting.
            let _ = app.window().hide();
        }
    });

    let drag = std::rc::Rc::new(std::cell::RefCell::new(None::<DragAnchor>));
    let weak = app.as_weak();
    let drag_move = drag.clone();
    app.on_title_drag_move(move |x, y| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let window = app.window();
        let scale = window.scale_factor() as f64;
        // TouchArea reports logical px; Window::position is physical px.
        let cursor_x = window.position().x as f64 + x as f64 * scale;
        let cursor_y = window.position().y as f64 + y as f64 * scale;
        let mut anchor = drag_move.borrow_mut();
        if let Some(a) = anchor.as_ref() {
            let nx = (cursor_x - a.offset_x).round() as i32;
            let ny = (cursor_y - a.offset_y).round() as i32;
            window.set_position(slint::PhysicalPosition::new(nx, ny));
        } else {
            // First move after press: anchor without jumping. The window
            // position read may fail pre-show; fall back to cursor origin.
            let wx = window.position().x as f64;
            let wy = window.position().y as f64;
            *anchor = Some(DragAnchor {
                offset_x: cursor_x - wx,
                offset_y: cursor_y - wy,
            });
        }
    });

    let weak = app.as_weak();
    app.on_title_drag_end(move || {
        let _ = weak.upgrade();
        *drag.borrow_mut() = None;
    });
}

/// Cursor-to-window-origin offset (physical px) captured at drag start.
#[derive(Clone, Copy)]
struct DragAnchor {
    offset_x: f64,
    offset_y: f64,
}

/// Push window-size-derived scroll geometry into Slint properties.
///
/// Chrome above the page: titlebar 40 + header 56 + tabs 48 = 144px.
/// Fixed page content: drive card 144 + section header 40 + padding 48.
/// Floors keep degenerate (e.g. minimized, zero-size) windows sane.
/// Deliberately one-way (Rust -> Slint): Slint must never read ancestor
/// geometry back, or the layoutinfo graph goes circular.
pub(crate) fn sync_geometry(app: &AppWindow) {
    let window = app.window();
    let logical = window.size().to_logical(window.scale_factor());
    let page_h = (logical.height - 144.0).max(200.0);
    let flick_h = (page_h - 232.0).max(50.0);
    let viewport_w = (logical.width - 48.0).max(200.0);
    app.set_page_h(page_h);
    app.set_flick_h(flick_h);
    app.set_viewport_w(viewport_w);
}

fn apply_storage(app: &AppWindow, info: &StorageInfo) {
    app.set_drive_name(info.display_name().into());
    app.set_used_percent(info.usage_ratio);
    if info.total_bytes == 0 {
        app.set_used_text("—".into());
        app.set_free_text("—".into());
        app.set_available_text("—".into());
    } else {
        app.set_used_text(format!("{} used", format_bytes(info.used_bytes)).into());
        app.set_free_text(format!("{} free", format_bytes(info.available_bytes)).into());
        app.set_available_text(format_bytes(info.available_bytes).into());
    }
    app.set_status_text(info.health_text().into());
    app.set_status_ok(info.total_bytes > 0 && info.usage_ratio < 0.9);
}
