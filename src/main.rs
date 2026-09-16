mod classify;
mod cleanup;
mod detail;
mod platform;
mod scan;
mod storage;
mod xdg;

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

    // M5 selection store: the latest (rules, classification), shared
    // between the scan-log worker (writes once on Done) and the UI thread
    // (reads on every category click). No rescans, noDuplicates: detail
    // views render purely from this snapshot.
    let detail_store: DetailStore = Default::default();
    wire_selection(&app, detail_store.clone());

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
    //
    // Milestone 4: the scan also measures the nested leaves the
    // classifier needs (Trash, Flatpak, …); on completion the result is
    // classified (no rescans) and the category model is pushed to the UI
    // on the event-loop thread.
    wire_cleanup(&app, detail_store.clone());
    let scanner = trigger_scan(&app, detail_store.clone());

    if std::env::var("DISKSCOUT_START_PAGE").as_deref() == Ok("cleanup") {
        app.set_current_page("cleanup".into());
    }

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

/// Shared M5 selection snapshot: latest classified result plus the rules
/// that produced it (rules carry the scan root for label resolution).
/// Written once by the scan worker, read on every category click.
type DetailStore = std::sync::Arc<
    std::sync::Mutex<
        Option<(
            classify::ClassificationRules,
            classify::StorageClassification,
        )>,
    >,
>;

/// Wire dashboard row clicks to lazy detail views. Everything renders
/// from the stored snapshot: no rescan, no filesystem I/O. Clicks before
/// the first completed scan are ignored (dashboard keeps placeholders).
/// Back navigation and tab switches are pure Slint state changes, so the
/// dashboard model (and its scroll position) is never disturbed.
fn wire_selection(app: &AppWindow, store: DetailStore) {
    let weak = app.as_weak();
    app.on_category_selected(move |key: slint::SharedString| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let guard = store.lock().expect("detail store poisoned");
        let Some((rules, classification)) = guard.as_ref() else {
            return;
        };
        let Some(category) = classify::Category::ALL
            .iter()
            .find(|c| c.icon_name() == key.as_str())
            .copied()
        else {
            return;
        };
        let detail = detail::detail_for(classification, rules.home(), category);
        let rows: Vec<DetailRowData> = detail
            .rows
            .iter()
            .map(|row| DetailRowData {
                label: row.label.clone().into(),
                size: format_bytes(row.bytes).into(),
                meta: detail::files_meta(row.files).into(),
            })
            .collect();
        let window = app.window();
        let height = window.size().to_logical(window.scale_factor()).height;
        app.set_detail_title(category.display_name().into());
        app.set_detail_total(format_bytes(detail.total_bytes).into());
        app.set_detail_rows(slint::ModelRc::new(std::rc::Rc::new(
            slint::VecModel::from(rows),
        )));
        app.set_detail_viewport_h(detail::detail_viewport_height(detail.rows.len()));
        app.set_detail_list_h(detail::detail_list_height(height));
        app.set_detail_category(key);
    });
}

/// Classify a finished scan and push the category model to the UI.
///
/// Runs on the scan-log worker thread: classification itself is pure CPU
/// over the already-aggregated result (microseconds), while the model
/// update hops to the event-loop thread via `invoke_from_event_loop`.
/// Only `Completed` scans replace the model; anything else keeps the
/// "…" placeholders rather than flashing partials.
fn apply_classification(
    weak: &slint::Weak<AppWindow>,
    rules: classify::ClassificationRules,
    result: &scan::ScanResult,
    store: &DetailStore,
) {
    let started = std::time::Instant::now();
    let classification = rules.classify(result);
    let classify_ms = started.elapsed().as_secs_f64() * 1000.0;

    let sum = classification.category_sum_bytes();
    eprintln!(
        "diskscout: classified in {:.1}ms: categories sum {} == scan total {} ({} files): partition_ok={} (scan errors: {})",
        classify_ms,
        format_bytes(sum),
        format_bytes(classification.total_bytes),
        classification.total_files,
        classification.partition_ok(),
        classification.error_count
    );
    // Byte-exact audit equation (no rounding): total must equal the sum of
    // all nine categories in raw bytes.
    let equation: Vec<String> = classify::Category::ALL
        .iter()
        .map(|&cat| classification.of(cat).bytes.to_string())
        .collect();
    eprintln!(
        "diskscout: equation bytes: total={} = {}",
        classification.total_bytes,
        equation.join(" + ")
    );
    let mut provenance = 0usize;
    for category in classify::Category::ALL {
        let total = classification.of(category);
        provenance += total.contributions.len();
        eprintln!(
            "diskscout: category {:<13} {} ({} files, {} paths)",
            total.category.display_name(),
            format_bytes(total.bytes),
            total.files,
            total.contributions.len()
        );
        let mut top: Vec<_> = total.contributions.iter().collect();
        top.sort_by_key(|a| std::cmp::Reverse(a.bytes));
        for contribution in top.into_iter().take(3) {
            eprintln!(
                "diskscout:   {} {} ({} files) from {}",
                format_bytes(contribution.bytes),
                contribution.detail,
                contribution.files,
                contribution.path.display()
            );
        }
    }
    eprintln!("diskscout: provenance entries total: {provenance}");
    for note in &classification.notes {
        eprintln!("diskscout: note: {note}");
    }

    if result.state != scan::ScanState::Completed {
        return;
    }
    // Snapshot for lazy M5 detail views before the UI update below.
    *store.lock().expect("detail store poisoned") = Some((rules, classification.clone()));
    let total_bytes = classification.total_bytes;
    let mut sorted_categories = classification.categories.clone();
    sorted_categories.sort_by_key(|cat| std::cmp::Reverse(cat.bytes));

    let rows: Vec<CategoryRow> = sorted_categories
        .iter()
        .map(|total| {
            let pct = if total_bytes > 0 {
                (total.bytes as f64 / total_bytes as f64 * 100.0).round() as u64
            } else {
                0
            };
            CategoryRow {
                name: total.category.display_name().into(),
                size: format_bytes(total.bytes).into(),
                subtitle: total.category.subtitle().into(),
                icon_name: total.category.icon_name().into(),
                percent: format!("{}%", pct).into(),
            }
        })
        .collect();

    let segments: Vec<BarSegment> = sorted_categories
        .iter()
        .filter(|cat| cat.bytes > 0)
        .map(|cat| {
            let ratio = if total_bytes > 0 {
                (cat.bytes as f32 / total_bytes as f32)
                    * (result.total_bytes as f32 / (result.total_bytes.max(1) as f32))
            } else {
                0.0
            };
            let color = match cat.category {
                classify::Category::Applications => slint::Color::from_rgb_u8(59, 130, 246),
                classify::Category::Videos => slint::Color::from_rgb_u8(139, 92, 246),
                classify::Category::Pictures => slint::Color::from_rgb_u8(245, 158, 11),
                classify::Category::Documents => slint::Color::from_rgb_u8(16, 185, 129),
                classify::Category::Downloads => slint::Color::from_rgb_u8(99, 102, 241),
                classify::Category::Music => slint::Color::from_rgb_u8(236, 72, 153),
                classify::Category::Temporary => slint::Color::from_rgb_u8(217, 119, 6),
                classify::Category::Trash => slint::Color::from_rgb_u8(239, 68, 68),
                classify::Category::Other => slint::Color::from_rgb_u8(100, 116, 139),
            };
            BarSegment { ratio, color }
        })
        .collect();

    // Canvas arithmetic mirrors AppWindow.slint: rows*64 + gaps*10 + 16 pad.
    let viewport_h = rows.len() as f32 * 64.0 + rows.len().saturating_sub(1) as f32 * 10.0 + 16.0;
    let weak = weak.clone();
    if slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            app.set_categories(slint::ModelRc::new(std::rc::Rc::new(
                slint::VecModel::from(rows),
            )));
            app.set_bar_segments(slint::ModelRc::new(std::rc::Rc::new(
                slint::VecModel::from(segments),
            )));
            app.set_viewport_h(viewport_h);
            if std::env::var("DISKSCOUT_START_PAGE").as_deref() == Ok("apps_detail") {
                app.invoke_category_selected("apps".into());
            }
        }
    })
    .is_err()
    {
        eprintln!("diskscout: UI gone, skipping category update");
    }
}

fn apply_storage(app: &AppWindow, info: &StorageInfo) {
    app.set_drive_name(info.display_name().into());
    app.set_used_percent(info.usage_ratio);
    if info.total_bytes == 0 {
        app.set_used_text("—".into());
        app.set_free_text("—".into());
        app.set_available_text("—".into());
        app.set_total_text("—".into());
    } else {
        app.set_used_text(format!("{} used", format_bytes(info.used_bytes)).into());
        app.set_free_text(format!("{} free", format_bytes(info.available_bytes)).into());
        app.set_available_text(format_bytes(info.available_bytes).into());
        app.set_total_text(format_bytes(info.total_bytes).into());
    }
    app.set_status_text(info.health_text().into());
    app.set_status_ok(info.total_bytes > 0 && info.usage_ratio < 0.9);
}

/// Refresh sizes and Btrfs information on the Cleanup page.
fn refresh_cleanup_state(app: &AppWindow) {
    if let Ok(home) = platform::default_scan_root() {
        let trash = cleanup::query_trash_bytes(&home);
        app.set_trash_size(format_bytes(trash).into());

        let cache = cleanup::query_cache_bytes(&home);
        app.set_cache_size(format_bytes(cache).into());

        let (_is_btrfs, summary) = cleanup::query_btrfs_summary();
        app.set_btrfs_info(summary.into());
    }
}

/// Trigger background filesystem scanning asynchronously.
fn trigger_scan(app: &AppWindow, detail_store: DetailStore) -> Option<scan::ScanHandle> {
    app.set_is_scanning(true);
    let (options, rules) = match platform::default_scan_root().map(|root| {
        let rules = classify::ClassificationRules::for_home(&root);
        let options = scan::ScanOptions {
            root,
            follow_symlinks: false,
            track: rules.wanted_paths(),
        };
        (options, rules)
    }) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("diskscout: scanner not started: {err:#}");
            app.set_is_scanning(false);
            return None;
        }
    };

    eprintln!("diskscout: scanning {}", options.root.display());
    let mut handle = scan::spawn_scan(options);
    let receiver = handle.take_receiver();
    let weak = app.as_weak();
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
                        apply_classification(&weak, rules, &result, &detail_store);
                        let weak_ui = weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(app) = weak_ui.upgrade() {
                                app.set_is_scanning(false);
                            }
                        });
                        break;
                    }
                }
            }
        })
        .ok();
    Some(handle)
}

/// Wire interactive cleanup and rescan callbacks.
fn wire_cleanup(app: &AppWindow, detail_store: DetailStore) {
    refresh_cleanup_state(app);

    let weak = app.as_weak();
    let store = detail_store.clone();
    app.on_empty_trash_requested(move || {
        if let (Some(app), Ok(home)) = (weak.upgrade(), platform::default_scan_root()) {
            match cleanup::empty_trash(&home) {
                Ok(reclaimed) => {
                    app.set_cleanup_feedback(
                        format!("Trash emptied: reclaimed {}", format_bytes(reclaimed)).into(),
                    );
                    refresh_cleanup_state(&app);
                    trigger_scan(&app, store.clone());
                }
                Err(e) => {
                    app.set_cleanup_feedback(format!("Failed to empty trash: {e}").into());
                }
            }
        }
    });

    let weak = app.as_weak();
    let store = detail_store.clone();
    app.on_clean_cache_requested(move || {
        if let (Some(app), Ok(home)) = (weak.upgrade(), platform::default_scan_root()) {
            match cleanup::clean_thumbnail_cache(&home) {
                Ok(reclaimed) => {
                    app.set_cleanup_feedback(
                        format!("Caches cleaned: reclaimed {}", format_bytes(reclaimed)).into(),
                    );
                    refresh_cleanup_state(&app);
                    trigger_scan(&app, store.clone());
                }
                Err(e) => {
                    app.set_cleanup_feedback(format!("Failed to clean cache: {e}").into());
                }
            }
        }
    });

    let weak = app.as_weak();
    let store = detail_store.clone();
    app.on_rescan_requested(move || {
        if let Some(app) = weak.upgrade() {
            if let Ok(info) = storage::query_home_filesystem() {
                apply_storage(&app, &info);
            }
            refresh_cleanup_state(&app);
            trigger_scan(&app, store.clone());
        }
    });
}
