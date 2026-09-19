#![allow(
    clippy::collapsible_if,
    clippy::too_many_arguments,
    clippy::manual_div_ceil,
    clippy::manual_is_multiple_of,
    clippy::unnecessary_sort_by,
    clippy::needless_range_loop,
    clippy::manual_clamp,
    clippy::get_first,
    clippy::double_ended_iterator_last
)]
pub mod applications;
mod classify;
mod cleanup;
mod detail;
mod explorer;
mod platform;
mod scan;
mod storage;
mod sysinfo_classify;
mod xdg;

#[cfg(test)]
mod shell_test;

use slint::Model;
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
    let app_store: applications::ApplicationStore =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let system_store: sysinfo_classify::SystemStore =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let icon_resolver =
        std::sync::Arc::new(std::sync::Mutex::new(applications::IconResolver::new()));
    let filtered_store = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let current_filter = std::sync::Arc::new(std::sync::Mutex::new("apps".to_string()));
    let current_search = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let current_sort = std::sync::Arc::new(std::sync::Mutex::new("size_desc".to_string()));
    let ui_image_cache =
        std::rc::Rc::new(std::cell::RefCell::new(applications::UiImageCache::new()));
    let app_model_slot: AppModelSlot = Default::default();

    wire_selection(
        &app,
        detail_store.clone(),
        app_store.clone(),
        icon_resolver.clone(),
        filtered_store.clone(),
        current_filter.clone(),
        current_search.clone(),
        current_sort.clone(),
        ui_image_cache.clone(),
        app_model_slot.clone(),
    );

    // Spawn background discovery for installed applications (Pacman/Foreign, Flatpak, Steam).
    // The worker only fills the store + path cache, then raises `apps_pending`.
    // Model population happens on the UI thread (icon_fill_timer) because the
    // image cache and model handle are UI-thread `Rc`s that cannot cross threads.
    let apps_pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let app_store_worker = app_store.clone();
        let app_weak_worker = app.as_weak();
        let icon_resolver_worker = icon_resolver.clone();
        let apps_pending_worker = apps_pending.clone();
        std::thread::spawn(move || {
            let t_discover = std::time::Instant::now();
            let apps = applications::discover_applications();
            eprintln!(
                "[diskscout perf] app inventory discovery: {:.2}ms ({} apps)",
                t_discover.elapsed().as_secs_f64() * 1000.0,
                apps.len()
            );
            // Pre-warm icon path cache on the background thread FIRST.
            // Doing this before setting app_store ensures zero lock contention with UI thread.
            {
                let mut resolver = icon_resolver_worker.lock().expect("resolver poisoned");
                let names: Vec<&str> = apps.iter().map(|a| a.icon_name.as_str()).collect();
                resolver.prewarm(&names);
            }
            // Store applications into shared store once ready
            {
                let mut guard = app_store_worker.lock().expect("app store poisoned");
                *guard = Some(apps.clone());
            }
            apps_pending_worker.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak_worker.upgrade() {
                    app.invoke_inventory_ready();
                }
            });
        });
    }

    // Spawn background system directory measurement (non-blocking).
    // Results feed the System category in the storage classification view.
    {
        let home =
            platform::default_scan_root().unwrap_or_else(|_| std::path::PathBuf::from("/home"));
        let system_store_worker = system_store.clone();
        let app_weak_sys = app.as_weak();
        let store_sys = detail_store.clone();
        let sys_store_cb = system_store.clone();
        sysinfo_classify::spawn_system_measurement(home, system_store_worker, move |measurement| {
            let app_weak = app_weak_sys.clone();
            let store = store_sys.clone();
            let sys_store = sys_store_cb.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak.upgrade() {
                    eprintln!(
                        "[diskscout] system measurement ready: {} bytes across {} dirs",
                        measurement.total_bytes,
                        measurement.dirs.len()
                    );
                    let guard = store.lock().expect("detail store poisoned");
                    if let Some((rules, classification, file_tree)) = guard.as_ref() {
                        let rules = rules.clone();
                        let result = scan::ScanResult {
                            root: rules.home().to_path_buf(),
                            total_bytes: classification.total_bytes,
                            file_count: classification.total_files,
                            dir_count: 0,
                            error_count: classification.error_count,
                            elapsed: std::time::Duration::from_secs(0),
                            top_entries: Vec::new(),
                            file_tree: file_tree.clone(),
                            tracked: Vec::new(),
                            last_error: None,
                            state: scan::ScanState::Completed,
                        };
                        drop(guard);
                        apply_classification(&app.as_weak(), rules, &result, &store, &sys_store);
                    }
                }
            });
        });
    }

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

    // Viewport-driven icon fill (M7.2.2): observe the apps list scroll
    // position on the event-loop thread and decode icons only for the
    // visible window + prefetch buffer. Each tick does bounded work
    // (already-cached rows are skipped), so scrolling stays smooth at
    // 500 or 2,000+ entries. Never blocks on the filesystem: path
    // lookups are cached, failed decodes are memoized in UiImageCache.
    //
    // The same tick also drains `apps_pending`: when background discovery
    // finishes while the user is on the Applications page, the model is
    // populated here on the UI thread (the image cache and model handle
    // are UI-thread `Rc`s, so the worker cannot populate directly).
    let icon_fill_timer = slint::Timer::default();
    {
        let weak = app.as_weak();
        let filtered_ref = filtered_store.clone();
        let resolver_ref = icon_resolver.clone();
        let cache_ref = ui_image_cache.clone();
        let slot_ref = app_model_slot.clone();
        let last_window = std::rc::Rc::new(std::cell::Cell::new((usize::MAX, usize::MAX)));
        icon_fill_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(50),
            move || {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                if app.get_detail_category() != "apps" || app.get_app_detail_active() {
                    return;
                }

                let slot = slot_ref.borrow();
                let Some(model) = slot.as_ref() else {
                    return;
                };
                let filtered = filtered_ref.lock().expect("filtered store poisoned");
                if filtered.is_empty() {
                    return;
                }
                let visible_h: f32 = app.get_apps_flick_h();
                if visible_h <= 0.0 {
                    return;
                }
                let scroll_offset = (-app.get_apps_scroll_y()).max(0.0);
                let pitch = app_row_pitch(&app.get_global_icon_size());
                let page_idx = (app.get_app_current_page().max(0)) as usize;
                let start = page_idx * APP_PAGE_SIZE;
                let end = (start + APP_PAGE_SIZE).min(filtered.len());
                let page_slice = if start < filtered.len() {
                    &filtered[start..end]
                } else {
                    &[]
                };
                let window = app_icon_window(
                    page_slice.len().min(model.row_count()),
                    scroll_offset,
                    visible_h,
                    pitch,
                );
                if window == last_window.get() {
                    return;
                }
                last_window.set(window);
                let mut resolver = resolver_ref.lock().expect("resolver poisoned");
                let mut img_cache = cache_ref.borrow_mut();
                let (decoded, _) = fill_app_icon_window_budgeted(
                    model,
                    page_slice,
                    &mut resolver,
                    &mut img_cache,
                    scroll_offset,
                    visible_h,
                    pitch,
                    Some(6),
                );
                // If all visible rows have icons, remember window to stop redundant work.
                // If there are still uncached icons, keep MAX so next 50ms tick continues decoding.
                if decoded == 0 {
                    last_window.set(window);
                } else {
                    last_window.set((usize::MAX, usize::MAX));
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
    wire_cleanup(&app, detail_store.clone(), system_store.clone());
    let scanner = trigger_scan(&app, detail_store.clone(), system_store.clone());

    match std::env::var("DISKSCOUT_START_PAGE").as_deref() {
        Ok("cleanup") => app.set_current_page("cleanup".into()),
        Ok("apps_detail") | Ok("apps_apps") => {
            app.set_detail_total("20.4 GB".into());
            app.invoke_category_selected("apps".into());
            app.invoke_app_filter_selected("apps".into());
        }
        Ok("apps_games") => {
            app.set_detail_total("20.4 GB".into());
            app.invoke_category_selected("apps".into());
            app.invoke_app_filter_selected("games".into());
        }
        Ok("apps_runtimes") => {
            app.set_detail_total("20.4 GB".into());
            app.invoke_category_selected("apps".into());
            app.invoke_app_filter_selected("runtimes".into());
        }
        Ok("apps_all") => {
            app.set_detail_total("20.4 GB".into());
            app.invoke_category_selected("apps".into());
            app.invoke_app_filter_selected("all".into());
        }
        Ok("apps_small") => {
            app.set_detail_total("20.4 GB".into());
            app.invoke_category_selected("apps".into());
            app.invoke_global_icon_size_selected("small".into());
        }
        Ok("apps_medium") => {
            app.set_detail_total("20.4 GB".into());
            app.invoke_category_selected("apps".into());
            app.invoke_global_icon_size_selected("medium".into());
        }
        Ok("apps_large") => {
            app.set_detail_total("20.4 GB".into());
            app.invoke_category_selected("apps".into());
            app.invoke_global_icon_size_selected("large".into());
        }
        Ok("apps_detail_drilldown") => {
            app.set_detail_total("20.4 GB".into());
            app.invoke_category_selected("apps".into());
            app.invoke_app_selected(1);
        }
        Ok("apps_search") => {
            app.set_detail_total("20.4 GB".into());
            app.invoke_category_selected("apps".into());
            app.invoke_app_search_changed("brave".into());
        }
        _ => {}
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
            std::sync::Arc<scan::FileTree>,
        )>,
    >,
>;

/// Handle to the live applications VecModel, shared between populate calls
/// (which replace it) and the scroll-fill timer (which patches rows in
/// place via `set_row_data`). UI-thread only.
type AppModelSlot =
    std::rc::Rc<std::cell::RefCell<Option<std::rc::Rc<slint::VecModel<AppRowData>>>>>;

/// Rows of prefetch buffer above/below the visible window for icon decoding.
/// Small and constant: scales from 508 to 2,000+ entries because only this
/// window is ever decoded, never the whole list.
const APP_ICON_PREFETCH_ROWS: usize = 8;

/// Row pitch (height + spacing) per global icon size. Must match the
/// `AppInventoryRow` heights in DetailRow.slint (50/58/68) + 8px spacing.
fn app_row_pitch(icon_size: &str) -> f32 {
    match icon_size {
        "small" => 58.0,
        "large" => 76.0,
        _ => 66.0,
    }
}

/// Visible-row window `[start, end)` for lazy icon decoding, derived from
/// actual scroll geometry: `scroll_offset` is px scrolled from the top,
/// `visible_h` the Flickable viewport height, `pitch` the row pitch.
/// Pure function of geometry; unit-tested. Never touches the filesystem.
fn app_icon_window(
    total_rows: usize,
    scroll_offset: f32,
    visible_h: f32,
    pitch: f32,
) -> (usize, usize) {
    if total_rows == 0
        || !scroll_offset.is_finite()
        || !visible_h.is_finite()
        || !pitch.is_finite()
        || visible_h <= 0.0
        || pitch <= 0.0
    {
        return (0, 0);
    }
    let first = (scroll_offset.max(0.0) / pitch) as usize;
    let last = ((scroll_offset.max(0.0) + visible_h) / pitch).ceil() as usize;
    let start = first.saturating_sub(APP_ICON_PREFETCH_ROWS).min(total_rows);
    let end = (last + APP_ICON_PREFETCH_ROWS).min(total_rows).max(start);
    (start, end)
}

/// Decode icons only for the visible window; every other row keeps its
/// fallback until scrolled into view. Rows already showing an icon are
/// skipped via the image cache. Returns `(newly_decoded, reused_cached)`.
fn fill_app_icon_window_budgeted(
    model: &slint::VecModel<AppRowData>,
    filtered: &[applications::Application],
    resolver: &mut applications::IconResolver,
    img_cache: &mut applications::UiImageCache,
    scroll_offset: f32,
    visible_h: f32,
    pitch: f32,
    max_decodes: Option<usize>,
) -> (usize, usize) {
    let total = filtered.len().min(model.row_count());
    let (start, end) = app_icon_window(total, scroll_offset, visible_h, pitch);
    if start >= end {
        return (0, 0);
    }
    let timer = std::time::Instant::now();
    let mut decoded = 0usize;
    let mut reused = 0usize;
    let limit = max_decodes.unwrap_or(usize::MAX);
    for idx in start..end {
        if decoded >= limit {
            break;
        }
        let row = match model.row_data(idx) {
            Some(row) => row,
            None => continue,
        };
        if row.has_icon {
            reused += 1;
            continue;
        }
        let app = &filtered[idx];
        let path = match resolver.resolve_path(&app.icon_name) {
            Some(path) => path,
            None => continue,
        };
        let image = if let Some(cached) = img_cache.get(&path) {
            reused += 1;
            cached
        } else {
            match img_cache.get_or_load(&path) {
                Some(image) => {
                    decoded += 1;
                    image
                }
                None => continue,
            }
        };
        let mut updated = row;
        updated.icon = image;
        updated.has_icon = true;
        model.set_row_data(idx, updated);
    }
    if decoded > 0 {
        eprintln!(
            "[diskscout perf] app visible icons: {decoded} decoded, {reused} reused in {:.2}ms (rows {start}..{end})",
            timer.elapsed().as_secs_f64() * 1000.0,
        );
    }
    (decoded, reused)
}

#[cfg_attr(not(test), allow(dead_code))]
fn fill_app_icon_window(
    model: &slint::VecModel<AppRowData>,
    filtered: &[applications::Application],
    resolver: &mut applications::IconResolver,
    img_cache: &mut applications::UiImageCache,
    scroll_offset: f32,
    visible_h: f32,
    pitch: f32,
) -> (usize, usize) {
    fill_app_icon_window_budgeted(
        model,
        filtered,
        resolver,
        img_cache,
        scroll_offset,
        visible_h,
        pitch,
        None,
    )
}

/// Wire dashboard row clicks to lazy detail views. Everything renders
/// from the stored snapshot: no rescan, no filesystem I/O. Clicks before
/// the first completed scan are ignored (dashboard keeps placeholders).
/// Back navigation and tab switches are pure Slint state changes, so the
/// dashboard model (and its scroll position) is never disturbed.
#[derive(Debug, Clone)]
struct NavBreadcrumb {
    title: String,
    dir_id: u32,
    #[allow(dead_code)]
    bytes: u64,
}

#[derive(Debug, Clone)]
struct DetailViewFrame {
    title: String,
    total: String,
    rows: Vec<detail::DetailItem>,
    back_text: String,
}

#[derive(Debug, Clone)]
struct ExplorerNavState {
    category: Option<classify::Category>,
    current_detail_rows: Vec<detail::DetailItem>,
    detail_stack: Vec<DetailViewFrame>,
    current_scope: classify::ContributionScope,
    history: Vec<NavBreadcrumb>,
    page_idx: usize,
    view_mode: String,
    icon_zoom: String,
    sort_mode: explorer::SortMode,
}

impl Default for ExplorerNavState {
    fn default() -> Self {
        Self {
            category: None,
            current_detail_rows: Vec::new(),
            detail_stack: Vec::new(),
            current_scope: classify::ContributionScope::default(),
            history: Vec::new(),
            page_idx: 0,
            view_mode: "compact".to_string(),
            icon_zoom: "medium".to_string(),
            sort_mode: explorer::SortMode::SizeDesc,
        }
    }
}

fn update_explorer_ui(app: &AppWindow, nav_state: &ExplorerNavState, tree: &scan::FileTree) {
    let Some(category) = nav_state.category else {
        return;
    };
    let Some(current) = nav_state.history.last() else {
        app.set_explorer_active(false);
        return;
    };

    let parent_title = if nav_state.history.len() <= 1 {
        category.display_name().to_string()
    } else {
        nav_state.history[nav_state.history.len() - 2].title.clone()
    };

    let mut crumbs: Vec<&str> = Vec::with_capacity(nav_state.history.len() + 1);
    crumbs.push(category.display_name());
    for item in &nav_state.history {
        crumbs.push(item.title.as_str());
    }
    let breadcrumb = crumbs.join(" › ");

    let page = explorer::load_dir(
        tree,
        current.dir_id,
        &nav_state.current_scope,
        nav_state.sort_mode,
        nav_state.page_idx,
        explorer::DEFAULT_PAGE_SIZE,
        &current.title,
        &parent_title,
        &breadcrumb,
    );

    let rows: Vec<ExplorerRowData> = page
        .rows
        .iter()
        .map(|r| ExplorerRowData {
            name: r.name.clone().into(),
            meta: r.meta.clone().into(),
            file_type: r.file_type.clone().into(),
            size: format_bytes(r.bytes).into(),
            icon_name: r.icon_name.clone().into(),
            is_dir: r.is_dir,
        })
        .collect();

    let window = app.window();
    let win_size = window.size().to_logical(window.scale_factor());
    let content_w = (win_size.width - 230.0 - 64.0).min(1300.0).max(400.0);

    app.set_explorer_title(page.title.into());
    app.set_explorer_total(format_bytes(page.total_bytes).into());
    app.set_explorer_parent_title(page.parent_title.into());
    app.set_explorer_breadcrumb(page.breadcrumb.into());
    app.set_explorer_current_page(page.current_page as i32);
    app.set_explorer_total_pages(page.total_pages as i32);
    app.set_explorer_summary(page.summary.into());
    app.set_explorer_empty(page.empty);
    app.set_explorer_error(page.error.unwrap_or_default().into());
    app.set_explorer_viewport_h(explorer::explorer_viewport_height(
        page.rows.len(),
        &nav_state.view_mode,
        &nav_state.icon_zoom,
        content_w,
    ));
    app.set_explorer_list_h(detail::detail_list_height(win_size.height));
    app.set_explorer_view_mode(nav_state.view_mode.clone().into());
    app.set_explorer_icon_zoom(nav_state.icon_zoom.clone().into());
    app.set_explorer_sort_label(nav_state.sort_mode.label().into());
    app.set_explorer_rows(slint::ModelRc::new(std::rc::Rc::new(
        slint::VecModel::from(rows),
    )));
    app.set_explorer_active(true);
}

fn build_app_row(
    app: &applications::Application,
    icon: slint::Image,
    has_icon: bool,
) -> AppRowData {
    AppRowData {
        name: app.display_name.clone().into(),
        version: app.version.clone().into(),
        source: app.source.as_str().into(),
        size: format_bytes(app.display_size()).into(),
        description: app.description.clone().into(),
        icon_name: app.icon_name.clone().into(),
        location: app
            .location
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
            .into(),
        is_dir: app.location.as_ref().map(|p| p.is_dir()).unwrap_or(false),
        icon,
        has_icon,
        kind: app.kind.as_str().into(),
    }
}

const APP_PAGE_SIZE: usize = 50;

fn populate_app_rows(
    app: &AppWindow,
    apps: &[applications::Application],
    filter: &str,
    search_query: &str,
    sort_key: &str,
    page_idx: usize,
    filtered_store: &std::sync::Arc<std::sync::Mutex<Vec<applications::Application>>>,
    model_slot: &AppModelSlot,
) {
    let t_start = std::time::Instant::now();

    // 1. Accurate category counts across whole inventory
    let count_all = apps.len() as i32;
    let count_apps = apps.iter().filter(|a| a.matches_filter("apps")).count() as i32;
    let count_games = apps.iter().filter(|a| a.matches_filter("games")).count() as i32;
    let count_runtimes = apps.iter().filter(|a| a.matches_filter("runtimes")).count() as i32;

    app.set_count_all(count_all);
    app.set_count_apps(count_apps);
    app.set_count_games(count_games);
    app.set_count_runtimes(count_runtimes);
    app.set_app_filter(filter.into());
    app.set_app_search_query(search_query.into());
    app.set_app_sort_key(sort_key.into());

    // 2. Filter by category & search query in memory
    let mut filtered: Vec<applications::Application> = apps
        .iter()
        .filter(|a| a.matches_filter(filter) && a.matches_search(search_query))
        .cloned()
        .collect();

    // 3. Sort in memory
    applications::model::sort_applications(&mut filtered, sort_key);
    let t_filter_sort = t_start.elapsed();

    // 4. Pagination & Model prep: build page rows immediately (<1ms).
    let total_pages = ((filtered.len() + APP_PAGE_SIZE.saturating_sub(1)) / APP_PAGE_SIZE).max(1);
    let page_idx = page_idx.min(total_pages.saturating_sub(1));
    app.set_app_current_page(page_idx as i32);
    app.set_app_total_pages(total_pages as i32);
    app.set_app_count(filtered.len() as i32);

    let start = page_idx * APP_PAGE_SIZE;
    let end = (start + APP_PAGE_SIZE).min(filtered.len());
    let page_slice = if start < filtered.len() {
        &filtered[start..end]
    } else {
        &[]
    };

    let t_prep_start = std::time::Instant::now();
    let app_rows: Vec<AppRowData> = page_slice
        .iter()
        .map(|a| build_app_row(a, slint::Image::default(), false))
        .collect();
    let model = std::rc::Rc::new(slint::VecModel::from(app_rows));
    app.set_app_rows(slint::ModelRc::new(model.clone()));
    *model_slot.borrow_mut() = Some(model);
    let t_prep = t_prep_start.elapsed();

    let t_total = t_start.elapsed();
    eprintln!(
        "[diskscout perf] app inventory filter+sort: {:.2}ms, model prep: {:.2}ms, total: {:.2}ms (page {}/{} - {} visible)",
        t_filter_sort.as_secs_f64() * 1000.0,
        t_prep.as_secs_f64() * 1000.0,
        t_total.as_secs_f64() * 1000.0,
        page_idx + 1,
        total_pages,
        page_slice.len()
    );

    if let Ok(mut guard) = filtered_store.lock() {
        *guard = filtered;
    }
}

fn wire_selection(
    app: &AppWindow,
    store: DetailStore,
    app_store: applications::ApplicationStore,
    icon_resolver: std::sync::Arc<std::sync::Mutex<applications::IconResolver>>,
    filtered_store: std::sync::Arc<std::sync::Mutex<Vec<applications::Application>>>,
    current_filter: std::sync::Arc<std::sync::Mutex<String>>,
    current_search: std::sync::Arc<std::sync::Mutex<String>>,
    current_sort: std::sync::Arc<std::sync::Mutex<String>>,
    ui_image_cache: std::rc::Rc<std::cell::RefCell<applications::UiImageCache>>,
    app_model_slot: AppModelSlot,
) {
    let nav_state = std::rc::Rc::new(std::cell::RefCell::new(ExplorerNavState::default()));

    // 1. Category click -> Category detail view
    let weak = app.as_weak();
    let nav = nav_state.clone();
    let store_ref = store.clone();
    let app_store_ref = app_store.clone();
    let filtered_store_ref = filtered_store.clone();
    let current_filter_ref = current_filter.clone();
    let current_search_ref = current_search.clone();
    let current_sort_ref = current_sort.clone();
    let model_slot_ref = app_model_slot.clone();
    let _icon_resolver_ref = icon_resolver.clone();
    let _ui_image_cache_ref = ui_image_cache.clone();
    app.on_category_selected(move |key: slint::SharedString| {
        let Some(app) = weak.upgrade() else {
            return;
        };

        if key == "apps" {
            let window = app.window();
            let height = window.size().to_logical(window.scale_factor()).height;

            let mut st = nav.borrow_mut();
            st.category = Some(classify::Category::Applications);
            st.current_detail_rows.clear();
            st.history.clear();
            st.page_idx = 0;
            app.set_explorer_active(false);

            app.set_detail_title("Applications".into());
            {
                let guard = store_ref.lock().expect("detail store poisoned");
                if let Some((rules, classification, _)) = guard.as_ref() {
                    let detail = detail::detail_for(
                        classification,
                        rules.home(),
                        classify::Category::Applications,
                    );
                    app.set_detail_total(format_bytes(detail.total_bytes).into());
                } else {
                    app.set_detail_total("20.4 GB".into());
                }
            }
            app.set_detail_list_h(detail::detail_list_height(height));
            app.set_detail_category(key.clone());
            app.set_app_detail_active(false);

            let apps_opt = {
                let guard = app_store_ref.lock().expect("app store poisoned");
                guard.clone()
            };
            match apps_opt {
                Some(cached) => {
                    app.set_app_loading(false);
                    let filter = {
                        let f_guard = current_filter_ref.lock().expect("filter poisoned");
                        f_guard.clone()
                    };
                    let search = {
                        let s_guard = current_search_ref.lock().expect("search poisoned");
                        s_guard.clone()
                    };
                    let sort = {
                        let so_guard = current_sort_ref.lock().expect("sort poisoned");
                        so_guard.clone()
                    };
                    // NOTE: populate_app_rows does NOT use resolver/img_cache.
                    // Do NOT lock icon_resolver here — the background prewarm thread may
                    // still hold it, which would freeze the UI for 200+ ms.
                    populate_app_rows(
                        &app,
                        &cached,
                        &filter,
                        &search,
                        &sort,
                        0,
                        &filtered_store_ref,
                        &model_slot_ref,
                    );
                }
                None => {
                    // Non-blocking navigation: immediately show loading state while discovery runs!
                    app.set_app_loading(true);
                    app.set_app_count(0);
                    app.set_app_rows(slint::ModelRc::new(std::rc::Rc::new(
                        slint::VecModel::default(),
                    )));
                    // Drop the stale model handle so the scroll timer cannot
                    // patch rows the user no longer sees.
                    *model_slot_ref.borrow_mut() = None;
                }
            }
            return;
        }

        let guard = store_ref.lock().expect("detail store poisoned");
        let Some((rules, classification, _)) = guard.as_ref() else {
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
                description: row.description.clone().into(),
                size: format_bytes(row.bytes).into(),
                meta: detail::files_meta(row.files).into(),
                icon_name: row.icon_name.clone().into(),
            })
            .collect();
        let window = app.window();
        let height = window.size().to_logical(window.scale_factor()).height;

        let mut st = nav.borrow_mut();
        st.category = Some(category);
        st.current_detail_rows = detail.rows.clone();
        st.detail_stack.clear();
        st.history.clear();
        st.page_idx = 0;
        app.set_explorer_active(false);
        app.set_detail_back_text("Back to Storage".into());

        app.set_detail_title(category.display_name().into());
        app.set_detail_total(format_bytes(detail.total_bytes).into());
        app.set_detail_rows(slint::ModelRc::new(std::rc::Rc::new(
            slint::VecModel::from(rows),
        )));
        app.set_detail_viewport_h(detail::detail_viewport_height(detail.rows.len()));
        app.set_detail_list_h(detail::detail_list_height(height));
        app.set_detail_category(key.clone());
        app.set_app_detail_active(false);
    });

    // 2. Contributor click -> Explorer root view
    let weak = app.as_weak();
    let nav = nav_state.clone();
    let store_ref = store.clone();
    app.on_contributor_selected(move |idx: i32| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let guard = store_ref.lock().expect("detail store poisoned");
        let Some((_, _, file_tree)) = guard.as_ref() else {
            return;
        };
        let mut st = nav.borrow_mut();
        let Some(item) = st.current_detail_rows.get(idx as usize).cloned() else {
            return;
        };
        let Some(category) = st.category else {
            return;
        };

        if !item.sub_items.is_empty() {
            // Drill down into sub-contributor breakdown view!
            let prev_rows = st.current_detail_rows.clone();
            st.detail_stack.push(DetailViewFrame {
                title: app.get_detail_title().to_string(),
                total: app.get_detail_total().to_string(),
                rows: prev_rows,
                back_text: app.get_detail_back_text().to_string(),
            });

            st.current_detail_rows = item.sub_items.clone();
            app.set_detail_title(item.label.clone().into());
            app.set_detail_total(format_bytes(item.bytes).into());
            app.set_detail_back_text(format!("Back to {}", category.display_name()).into());

            let rows: Vec<DetailRowData> = item
                .sub_items
                .iter()
                .map(|sub| DetailRowData {
                    label: sub.label.clone().into(),
                    description: sub.description.clone().into(),
                    size: format_bytes(sub.bytes).into(),
                    meta: detail::files_meta(sub.files).into(),
                    icon_name: sub.icon_name.clone().into(),
                })
                .collect();
            let window = app.window();
            let height = window.size().to_logical(window.scale_factor()).height;
            app.set_detail_rows(slint::ModelRc::new(std::rc::Rc::new(
                slint::VecModel::from(rows),
            )));
            app.set_detail_viewport_h(detail::detail_viewport_height(item.sub_items.len()));
            app.set_detail_list_h(detail::detail_list_height(height));
            return;
        }

        let (nav_path, nav_title) = if category == classify::Category::Trash {
            let files_path = item.path.join("files");
            if file_tree.find_dir(&files_path).is_some() {
                (files_path, "Deleted files".to_string())
            } else {
                (item.path.clone(), item.label.clone())
            }
        } else {
            (item.path.clone(), item.label.clone())
        };

        match explorer::resolve_contributor_root(file_tree, &nav_path) {
            Ok(dir_id) => {
                st.current_scope = item.scope.clone();
                st.history.clear();
                st.history.push(NavBreadcrumb {
                    title: nav_title,
                    dir_id,
                    bytes: item.bytes,
                });
                st.page_idx = 0;
                update_explorer_ui(&app, &st, file_tree);
            }
            Err(err) => {
                let window = app.window();
                let height = window.size().to_logical(window.scale_factor()).height;
                app.set_explorer_title(item.label.clone().into());
                app.set_explorer_total(format_bytes(item.bytes).into());
                app.set_explorer_parent_title(category.display_name().into());
                app.set_explorer_breadcrumb(
                    format!("{} › {}", category.display_name(), item.label).into(),
                );
                app.set_explorer_empty(false);
                app.set_explorer_error(err.into());
                app.set_explorer_rows(slint::ModelRc::new(std::rc::Rc::new(
                    slint::VecModel::default(),
                )));
                app.set_explorer_total_pages(0);
                app.set_explorer_summary("".into());
                app.set_explorer_list_h(detail::detail_list_height(height));
                app.set_explorer_active(true);
            }
        }
    });

    // Wire category detail back navigation (drills up to parent or back to storage)
    let weak_back = app.as_weak();
    let nav_back = nav_state.clone();
    app.on_detail_back(move || {
        let Some(app) = weak_back.upgrade() else {
            return;
        };
        let mut st = nav_back.borrow_mut();
        if let Some(prev) = st.detail_stack.pop() {
            st.current_detail_rows = prev.rows.clone();
            app.set_detail_title(prev.title.into());
            app.set_detail_total(prev.total.into());
            app.set_detail_back_text(prev.back_text.into());
            let rows: Vec<DetailRowData> = prev
                .rows
                .iter()
                .map(|r| DetailRowData {
                    label: r.label.clone().into(),
                    description: r.description.clone().into(),
                    size: format_bytes(r.bytes).into(),
                    meta: detail::files_meta(r.files).into(),
                    icon_name: r.icon_name.clone().into(),
                })
                .collect();
            let window = app.window();
            let height = window.size().to_logical(window.scale_factor()).height;
            app.set_detail_rows(slint::ModelRc::new(std::rc::Rc::new(
                slint::VecModel::from(rows),
            )));
            app.set_detail_viewport_h(detail::detail_viewport_height(prev.rows.len()));
            app.set_detail_list_h(detail::detail_list_height(height));
        } else {
            app.set_detail_category("".into());
            app.set_app_detail_active(false);
            st.category = None;
        }
    });

    // 3. Child folder click -> Nested directory navigation
    let weak = app.as_weak();
    let nav = nav_state.clone();
    let store_ref = store.clone();
    app.on_explorer_navigate(move |idx: i32| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let guard = store_ref.lock().expect("detail store poisoned");
        let Some((_, _, file_tree)) = guard.as_ref() else {
            return;
        };
        let mut st = nav.borrow_mut();
        let Some(current) = st.history.last() else {
            return;
        };
        let page = explorer::load_dir(
            file_tree,
            current.dir_id,
            &st.current_scope,
            st.sort_mode,
            st.page_idx,
            explorer::DEFAULT_PAGE_SIZE,
            &current.title,
            "",
            "",
        );
        let Some(row) = page.rows.get(idx as usize) else {
            return;
        };

        if row.is_dir {
            if let Some(child_dir_id) = row.dir_id {
                st.history.push(NavBreadcrumb {
                    title: row.name.clone(),
                    dir_id: child_dir_id,
                    bytes: row.bytes,
                });
                st.page_idx = 0;
                update_explorer_ui(&app, &st, file_tree);
            }
        }
    });

    // 4. Back button click -> Pop directory history
    let weak = app.as_weak();
    let nav = nav_state.clone();
    let store_ref = store.clone();
    app.on_explorer_back(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let guard = store_ref.lock().expect("detail store poisoned");
        let Some((_, _, file_tree)) = guard.as_ref() else {
            return;
        };
        let mut st = nav.borrow_mut();
        st.history.pop();
        st.page_idx = 0;
        if st.history.is_empty() {
            app.set_explorer_active(false);
        } else {
            update_explorer_ui(&app, &st, file_tree);
        }
    });

    // 5. Pagination change
    let weak = app.as_weak();
    let nav = nav_state.clone();
    let store_ref = store.clone();
    app.on_explorer_page_change(move |page: i32| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let guard = store_ref.lock().expect("detail store poisoned");
        let Some((_, _, file_tree)) = guard.as_ref() else {
            return;
        };
        let mut st = nav.borrow_mut();
        st.page_idx = page.max(0) as usize;
        update_explorer_ui(&app, &st, file_tree);
    });

    // 6. View mode toggle (Compact / Details / Icons)
    let weak = app.as_weak();
    let nav = nav_state.clone();
    let store_ref = store.clone();
    app.on_explorer_view_mode_selected(move |mode: slint::SharedString| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let guard = store_ref.lock().expect("detail store poisoned");
        let Some((_, _, file_tree)) = guard.as_ref() else {
            return;
        };
        let mut st = nav.borrow_mut();
        st.view_mode = mode.to_string();
        update_explorer_ui(&app, &st, file_tree);
    });

    // 7. Icon zoom (S / M / L) - strictly for Icons mode
    let weak = app.as_weak();
    let nav = nav_state.clone();
    let store_ref = store.clone();
    app.on_explorer_icon_zoom_selected(move |zoom: slint::SharedString| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let guard = store_ref.lock().expect("detail store poisoned");
        let Some((_, _, file_tree)) = guard.as_ref() else {
            return;
        };
        let mut st = nav.borrow_mut();
        st.icon_zoom = zoom.to_string();
        update_explorer_ui(&app, &st, file_tree);
    });

    // 8. Sort selection — called from the sort popover with an explicit key string
    let weak = app.as_weak();
    let nav = nav_state.clone();
    let store_ref = store.clone();
    app.on_explorer_sort_selected(move |key: slint::SharedString| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let guard = store_ref.lock().expect("detail store poisoned");
        let Some((_, _, file_tree)) = guard.as_ref() else {
            return;
        };
        let mut st = nav.borrow_mut();
        st.sort_mode = explorer::SortMode::from_key(key.as_str());
        st.page_idx = 0;
        update_explorer_ui(&app, &st, file_tree);
    });

    // Global icon size toggle callback
    let weak = app.as_weak();
    app.on_global_icon_size_selected(move |size: slint::SharedString| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        app.set_global_icon_size(size.clone());
        app.set_explorer_icon_zoom(size);
    });

    // App filter toggle callback
    let weak = app.as_weak();
    let app_store_ref = app_store.clone();
    let filtered_store_ref = filtered_store.clone();
    let current_filter_ref = current_filter.clone();
    let current_search_ref = current_search.clone();
    let current_sort_ref = current_sort.clone();
    let model_slot_ref = app_model_slot.clone();
    let _icon_resolver_ref = icon_resolver.clone();
    let _ui_image_cache_ref = ui_image_cache.clone();
    app.on_app_filter_selected(move |filter: slint::SharedString| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        app.set_app_filter(filter.clone());
        {
            let mut f_guard = current_filter_ref.lock().expect("filter poisoned");
            *f_guard = filter.to_string();
        }
        let guard = app_store_ref.lock().expect("app store poisoned");
        let Some(apps) = guard.as_ref() else {
            return;
        };
        let search = {
            let s_guard = current_search_ref.lock().expect("search poisoned");
            s_guard.clone()
        };
        let sort = {
            let so_guard = current_sort_ref.lock().expect("sort poisoned");
            so_guard.clone()
        };
        populate_app_rows(
            &app,
            apps,
            filter.as_str(),
            &search,
            &sort,
            0,
            &filtered_store_ref,
            &model_slot_ref,
        );
    });

    // App search callback
    let weak = app.as_weak();
    let app_store_ref = app_store.clone();
    let filtered_store_ref = filtered_store.clone();
    let current_filter_ref = current_filter.clone();
    let current_search_ref = current_search.clone();
    let current_sort_ref = current_sort.clone();
    let model_slot_ref = app_model_slot.clone();
    let _icon_resolver_ref = icon_resolver.clone();
    let _ui_image_cache_ref = ui_image_cache.clone();
    app.on_app_search_changed(move |query: slint::SharedString| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        app.set_app_search_query(query.clone());
        {
            let mut s_guard = current_search_ref.lock().expect("search poisoned");
            *s_guard = query.to_string();
        }
        let guard = app_store_ref.lock().expect("app store poisoned");
        let Some(apps) = guard.as_ref() else {
            return;
        };
        let filter = {
            let f_guard = current_filter_ref.lock().expect("filter poisoned");
            f_guard.clone()
        };
        let sort = {
            let so_guard = current_sort_ref.lock().expect("sort poisoned");
            so_guard.clone()
        };
        populate_app_rows(
            &app,
            apps,
            &filter,
            query.as_str(),
            &sort,
            0,
            &filtered_store_ref,
            &model_slot_ref,
        );
    });

    // App sort callback
    let weak = app.as_weak();
    let app_store_ref = app_store.clone();
    let filtered_store_ref = filtered_store.clone();
    let current_filter_ref = current_filter.clone();
    let current_search_ref = current_search.clone();
    let current_sort_ref = current_sort.clone();
    let model_slot_ref = app_model_slot.clone();
    let _icon_resolver_ref = icon_resolver.clone();
    let _ui_image_cache_ref = ui_image_cache.clone();
    app.on_app_sort_selected(move |sort_key: slint::SharedString| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        app.set_app_sort_key(sort_key.clone());
        {
            let mut so_guard = current_sort_ref.lock().expect("sort poisoned");
            *so_guard = sort_key.to_string();
        }
        let guard = app_store_ref.lock().expect("app store poisoned");
        let Some(apps) = guard.as_ref() else {
            return;
        };
        let filter = {
            let f_guard = current_filter_ref.lock().expect("filter poisoned");
            f_guard.clone()
        };
        let search = {
            let s_guard = current_search_ref.lock().expect("search poisoned");
            s_guard.clone()
        };
        populate_app_rows(
            &app,
            apps,
            &filter,
            &search,
            sort_key.as_str(),
            0,
            &filtered_store_ref,
            &model_slot_ref,
        );
    });

    // App page change callback (Milestone 7.2.3 Pagination)
    let weak = app.as_weak();
    let app_store_ref = app_store.clone();
    let filtered_store_ref = filtered_store.clone();
    let current_filter_ref = current_filter.clone();
    let current_search_ref = current_search.clone();
    let current_sort_ref = current_sort.clone();
    let model_slot_ref = app_model_slot.clone();
    app.on_app_page_change(move |page: i32| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let page_idx = page.max(0) as usize;
        let guard = app_store_ref.lock().expect("app store poisoned");
        let Some(apps) = guard.as_ref() else {
            return;
        };
        let filter = current_filter_ref.lock().expect("filter poisoned").clone();
        let search = current_search_ref.lock().expect("search poisoned").clone();
        let sort = current_sort_ref.lock().expect("sort poisoned").clone();
        populate_app_rows(
            &app,
            apps,
            &filter,
            &search,
            &sort,
            page_idx,
            &filtered_store_ref,
            &model_slot_ref,
        );
    });

    // 8.5 Inventory ready callback dispatched when background discovery completes
    let weak = app.as_weak();
    let app_store_ref = app_store.clone();
    let filtered_store_ref = filtered_store.clone();
    let current_filter_ref = current_filter.clone();
    let current_search_ref = current_search.clone();
    let current_sort_ref = current_sort.clone();
    let model_slot_ref = app_model_slot.clone();
    let icon_resolver_ref = icon_resolver.clone();
    let ui_image_cache_ref = ui_image_cache.clone();
    app.on_inventory_ready(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        app.set_app_loading(false);
        if app.get_detail_category() == "apps" && !app.get_app_detail_active() {
            let cached = {
                let guard = app_store_ref.lock().expect("app store poisoned");
                guard.clone()
            };
            if let Some(apps) = cached {
                let filter = current_filter_ref.lock().expect("filter poisoned").clone();
                let search = current_search_ref.lock().expect("search poisoned").clone();
                let sort = current_sort_ref.lock().expect("sort poisoned").clone();
                populate_app_rows(
                    &app,
                    &apps,
                    &filter,
                    &search,
                    &sort,
                    0,
                    &filtered_store_ref,
                    &model_slot_ref,
                );
                if std::env::var("DISKSCOUT_START_PAGE").as_deref() == Ok("apps_detail_drilldown") {
                    let guard = filtered_store_ref.lock().expect("filtered store poisoned");
                    if let Some(selected) = guard.get(1).or_else(|| guard.get(0)).cloned() {
                        drop(guard);
                        let mut resolver = icon_resolver_ref.lock().expect("resolver poisoned");
                        let mut img_cache = ui_image_cache_ref.borrow_mut();
                        show_app_detail(&app, &selected, &mut resolver, &mut img_cache);
                    }
                } else if std::env::var("DISKSCOUT_START_PAGE").as_deref() == Ok("apps_search") {
                    app.invoke_app_search_changed("brave".into());
                }
            }
        }
    });

    // 9. Application inventory callbacks (Milestone 7.2 & 7.2.1)
    let weak = app.as_weak();
    let filtered_store_ref = filtered_store.clone();
    let icon_resolver_ref = icon_resolver.clone();
    let ui_image_cache_ref = ui_image_cache.clone();
    fn show_app_detail(
        app: &AppWindow,
        selected: &applications::Application,
        resolver: &mut applications::IconResolver,
        img_cache: &mut applications::UiImageCache,
    ) {
        app.set_app_detail_name(selected.display_name.clone().into());
        app.set_app_detail_source(selected.source.as_str().into());
        app.set_app_detail_version(selected.version.clone().into());
        app.set_app_detail_size(format_bytes(selected.display_size()).into());
        app.set_app_detail_description(selected.description.clone().into());
        app.set_app_detail_package_name(selected.name.clone().into());

        let scope_label = match selected.install_scope {
            applications::InstallScope::System => "System application",
            applications::InstallScope::User => {
                if selected.source == applications::ApplicationSource::Flatpak {
                    "Flatpak user installation"
                } else if selected.source == applications::ApplicationSource::Steam {
                    "Steam user library"
                } else {
                    "User installation"
                }
            }
        };
        app.set_app_detail_scope(scope_label.into());

        let t_detail_start = std::time::Instant::now();
        let resolved_img = if let Some(path) = resolver.resolve_path(&selected.icon_name) {
            img_cache.get_or_load(&path)
        } else {
            None
        };

        let has_icon = resolved_img.is_some();
        app.set_app_detail_has_icon(has_icon);
        if let Some(img) = resolved_img {
            app.set_app_detail_icon(img);
        } else {
            app.set_app_detail_icon(slint::Image::default());
        }
        eprintln!(
            "[diskscout perf] app detail transition: {:.2}ms for '{}'",
            t_detail_start.elapsed().as_secs_f64() * 1000.0,
            selected.display_name
        );

        let loc_str = selected
            .location
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_dir = selected
            .location
            .as_ref()
            .map(|p| p.is_dir())
            .unwrap_or(false);
        app.set_app_detail_location(loc_str.into());
        app.set_app_detail_can_explore(is_dir);

        // Steam Workshop content size (Wallpaper Engine app ID 431960, etc.)
        if selected.source == applications::ApplicationSource::Steam {
            // Extract numeric app ID from "steam:431960" format
            let steam_app_id = selected.id.strip_prefix("steam:").unwrap_or("");
            if let Some(workshop_bytes) = applications::steam::workshop_content_size(steam_app_id) {
                app.set_app_detail_workshop_size(storage::format_bytes(workshop_bytes).into());
                app.set_app_detail_workshop_title(
                    format!("{} workshop wallpapers / content", selected.display_name).into(),
                );
            } else {
                app.set_app_detail_workshop_size("".into());
                app.set_app_detail_workshop_title("".into());
            }
        } else {
            app.set_app_detail_workshop_size("".into());
            app.set_app_detail_workshop_title("".into());
        }

        app.set_app_detail_active(true);
    }

    app.on_app_selected(move |idx: i32| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let page_idx = (app.get_app_current_page().max(0)) as usize;
        let global_idx = page_idx * APP_PAGE_SIZE + (idx as usize);
        let guard = filtered_store_ref.lock().expect("filtered store poisoned");
        let Some(selected) = guard.get(global_idx).cloned() else {
            return;
        };
        drop(guard);
        let mut resolver = icon_resolver_ref.lock().expect("resolver poisoned");
        let mut img_cache = ui_image_cache_ref.borrow_mut();
        show_app_detail(&app, &selected, &mut resolver, &mut img_cache);
    });

    let weak = app.as_weak();
    app.on_app_detail_back(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        app.set_app_detail_active(false);
    });

    let weak = app.as_weak();
    let store_ref = store.clone();
    let nav = nav_state.clone();
    app.on_app_detail_explore(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let loc = app.get_app_detail_location().to_string();
        if loc.is_empty() {
            return;
        }
        let loc_path = std::path::PathBuf::from(loc);
        if !loc_path.is_dir() {
            return;
        }
        let guard = store_ref.lock().expect("detail store poisoned");
        let Some((_, _, file_tree)) = guard.as_ref() else {
            return;
        };

        if let Ok(dir_id) = explorer::resolve_contributor_root(file_tree, &loc_path) {
            let mut st = nav.borrow_mut();
            st.current_scope = classify::ContributionScope::whole_subtree(loc_path.clone());
            st.history.clear();
            st.history.push(NavBreadcrumb {
                title: app.get_app_detail_name().to_string(),
                dir_id,
                bytes: 0,
            });
            st.page_idx = 0;
            update_explorer_ui(&app, &st, file_tree);
        }
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
    system_store: &sysinfo_classify::SystemStore,
) {
    let started = std::time::Instant::now();
    let mut classification = rules.classify(result);
    let classify_ms = started.elapsed().as_secs_f64() * 1000.0;

    let mut tree_data = (*result.file_tree).clone();

    // Inject system directory measurements into the System category.
    // This data comes from the background sysinfo thread (non-blocking).
    if let Some(sys_meas) = system_store.lock().ok().and_then(|g| g.clone()) {
        if sys_meas.total_bytes > 0 {
            // Merge system directory tree into combined file tree for explorer navigation
            tree_data.merge(&sys_meas.file_tree);

            // Find and update the System category in classification
            if let Some(cat_total) = classification
                .categories
                .iter_mut()
                .find(|c| c.category == classify::Category::System)
            {
                cat_total.bytes = sys_meas.total_bytes;
                let total_files: u64 = sys_meas.dirs.iter().map(|d| d.files).sum();
                cat_total.files = total_files;
                // Update total_bytes to include system contribution
                // (home scan total + system dirs = actual filesystem usage)
                let system_contribution = sys_meas.total_bytes;
                // Add system bytes to classification total so Other is computed correctly
                classification.total_bytes = classification
                    .total_bytes
                    .saturating_add(system_contribution);
                for dir in &sys_meas.dirs {
                    let sub_contributions = dir
                        .sub_dirs
                        .iter()
                        .map(|sub| classify::Contribution {
                            path: sub.path.clone(),
                            bytes: sub.bytes,
                            files: sub.files,
                            detail: sub.description.to_string(),
                            scope: classify::ContributionScope::whole_subtree(sub.path.clone()),
                            sub_contributions: Vec::new(),
                        })
                        .collect();
                    cat_total.contributions.push(classify::Contribution {
                        path: dir.path.clone(),
                        bytes: dir.bytes,
                        files: dir.files,
                        detail: dir.description.to_string(),
                        scope: classify::ContributionScope::whole_subtree(dir.path.clone()),
                        sub_contributions,
                    });
                }
            }
        }
    }

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
    let combined_file_tree = std::sync::Arc::new(tree_data);

    // Snapshot for lazy M5 detail views before the UI update below.
    *store.lock().expect("detail store poisoned") =
        Some((rules, classification.clone(), combined_file_tree));
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

    let total_used = classification.total_bytes.max(1);

    let category_color = |cat: classify::Category| match cat {
        classify::Category::System => slint::Color::from_rgb_u8(30, 41, 59), // #1e293b dark navy
        classify::Category::Other => slint::Color::from_rgb_u8(100, 116, 139), // #64748b neutral slate
        classify::Category::Videos => slint::Color::from_rgb_u8(139, 92, 246), // #8b5cf6 purple
        classify::Category::Applications => slint::Color::from_rgb_u8(37, 99, 235), // #2563eb blue
        classify::Category::Downloads => slint::Color::from_rgb_u8(99, 102, 241), // #6366f1 indigo
        classify::Category::Temporary => slint::Color::from_rgb_u8(217, 119, 6), // #d97706 orange
        classify::Category::Pictures => slint::Color::from_rgb_u8(245, 158, 11), // #f59e0b amber
        classify::Category::Documents => slint::Color::from_rgb_u8(16, 185, 129), // #10b981 emerald
        classify::Category::Music => slint::Color::from_rgb_u8(236, 72, 153),  // #ec4899 pink
        classify::Category::Trash => slint::Color::from_rgb_u8(239, 68, 68),   // #ef4444 red
    };

    let segments: Vec<BarSegment> = sorted_categories
        .iter()
        .filter(|cat| cat.bytes > 0)
        .map(|cat| {
            let ratio = cat.bytes as f32 / total_used as f32;
            let pct = (cat.bytes as f64 / total_used as f64 * 100.0).round() as u64;
            let color = category_color(cat.category);
            BarSegment {
                name: cat.category.display_name().into(),
                size_text: format_bytes(cat.bytes).into(),
                percent_text: format!("{}% of used", pct).into(),
                ratio,
                color,
            }
        })
        .collect();

    let mut legend_items: Vec<LegendItem> = Vec::new();
    let mut other_small_bytes: u64 = 0;
    for (i, cat) in sorted_categories.iter().filter(|c| c.bytes > 0).enumerate() {
        if i < 5 {
            legend_items.push(LegendItem {
                name: cat.category.display_name().into(),
                size_text: format_bytes(cat.bytes).into(),
                color: category_color(cat.category),
            });
        } else {
            other_small_bytes = other_small_bytes.saturating_add(cat.bytes);
        }
    }
    if other_small_bytes > 0 {
        legend_items.push(LegendItem {
            name: "Other categories".into(),
            size_text: format_bytes(other_small_bytes).into(),
            color: slint::Color::from_rgb_u8(148, 163, 184),
        });
    }

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
            app.set_bar_legend_items(slint::ModelRc::new(std::rc::Rc::new(
                slint::VecModel::from(legend_items),
            )));
            app.set_viewport_h(viewport_h);
            match std::env::var("DISKSCOUT_START_PAGE").as_deref() {
                Ok("apps_detail") | Ok("apps_apps") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_app_filter_selected("apps".into());
                }
                Ok("apps_games") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_app_filter_selected("games".into());
                }
                Ok("apps_runtimes") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_app_filter_selected("runtimes".into());
                }
                Ok("apps_all") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_app_filter_selected("all".into());
                }
                Ok("apps_small") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_global_icon_size_selected("small".into());
                }
                Ok("apps_medium") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_global_icon_size_selected("medium".into());
                }
                Ok("apps_large") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_global_icon_size_selected("large".into());
                }
                Ok("apps_detail_drilldown") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_app_selected(1);
                }
                Ok("videos_detail") => app.invoke_category_selected("video".into()),
                Ok("other_detail") => app.invoke_category_selected("folder".into()),
                Ok("music_detail") => app.invoke_category_selected("music".into()),
                Ok("explorer_other") => {
                    app.invoke_category_selected("folder".into());
                    app.invoke_contributor_selected(0);
                }
                Ok("explorer_other_details") => {
                    app.invoke_category_selected("folder".into());
                    app.invoke_contributor_selected(0);
                    app.invoke_explorer_view_mode_selected("details".into());
                }
                Ok("explorer_other_icons") => {
                    app.invoke_category_selected("folder".into());
                    app.invoke_contributor_selected(0);
                    app.invoke_explorer_view_mode_selected("icons".into());
                }
                Ok("explorer_steam") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_contributor_selected(0);
                }
                Ok("explorer_steam_sort") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_contributor_selected(0);
                    app.invoke_trigger_sort_menu();
                }
                Ok("explorer_steam_icons") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_contributor_selected(0);
                    app.invoke_explorer_view_mode_selected("icons".into());
                }
                Ok("explorer_steam_icons_large") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_contributor_selected(0);
                    app.invoke_explorer_view_mode_selected("icons".into());
                    app.invoke_explorer_icon_zoom_selected("large".into());
                }
                Ok("explorer_steam_icons_small") => {
                    app.invoke_category_selected("apps".into());
                    app.invoke_contributor_selected(0);
                    app.invoke_explorer_view_mode_selected("icons".into());
                    app.invoke_explorer_icon_zoom_selected("small".into());
                }
                Ok("explorer_other_nested") => {
                    app.invoke_category_selected("folder".into());
                    app.invoke_contributor_selected(0);
                    app.invoke_explorer_navigate(0);
                }
                _ => {}
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
fn trigger_scan(
    app: &AppWindow,
    detail_store: DetailStore,
    system_store: sysinfo_classify::SystemStore,
) -> Option<scan::ScanHandle> {
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
    let sys_store = system_store.clone();
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
                        apply_classification(&weak, rules, &result, &detail_store, &sys_store);
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
fn wire_cleanup(
    app: &AppWindow,
    detail_store: DetailStore,
    system_store: sysinfo_classify::SystemStore,
) {
    refresh_cleanup_state(app);

    let weak = app.as_weak();
    let store = detail_store.clone();
    let sys_store = system_store.clone();
    app.on_empty_trash_requested(move || {
        if let (Some(app), Ok(home)) = (weak.upgrade(), platform::default_scan_root()) {
            match cleanup::empty_trash(&home) {
                Ok(reclaimed) => {
                    app.set_cleanup_feedback(
                        format!("Trash emptied: reclaimed {}", format_bytes(reclaimed)).into(),
                    );
                    refresh_cleanup_state(&app);
                    trigger_scan(&app, store.clone(), sys_store.clone());
                }
                Err(e) => {
                    app.set_cleanup_feedback(format!("Failed to empty trash: {e}").into());
                }
            }
        }
    });

    let weak = app.as_weak();
    let store = detail_store.clone();
    let sys_store = system_store.clone();
    app.on_clean_cache_requested(move || {
        if let (Some(app), Ok(home)) = (weak.upgrade(), platform::default_scan_root()) {
            match cleanup::clean_thumbnail_cache(&home) {
                Ok(reclaimed) => {
                    app.set_cleanup_feedback(
                        format!("Caches cleaned: reclaimed {}", format_bytes(reclaimed)).into(),
                    );
                    refresh_cleanup_state(&app);
                    trigger_scan(&app, store.clone(), sys_store.clone());
                }
                Err(e) => {
                    app.set_cleanup_feedback(format!("Failed to clean cache: {e}").into());
                }
            }
        }
    });

    let weak = app.as_weak();
    let store = detail_store.clone();
    let sys_store = system_store.clone();
    app.on_rescan_requested(move || {
        if let Some(app) = weak.upgrade() {
            if let Ok(info) = storage::query_home_filesystem() {
                apply_storage(&app, &info);
            }
            refresh_cleanup_state(&app);
            trigger_scan(&app, store.clone(), sys_store.clone());
        }
    });
}

#[cfg(test)]
mod app_icon_tests {
    use super::*;
    use crate::applications::{Application, ApplicationKind, ApplicationSource, InstallScope};

    fn fake_app(name: &str, icon_name: &str) -> Application {
        Application {
            id: name.into(),
            name: name.into(),
            display_name: name.into(),
            version: "1.0".into(),
            description: String::new(),
            source: ApplicationSource::Manual,
            kind: ApplicationKind::Application,
            install_scope: InstallScope::User,
            package_size: 1000,
            measured_size: None,
            user_data_size: None,
            runtime_size: None,
            location: None,
            desktop_file: None,
            icon_name: icon_name.into(),
            is_explicit: true,
        }
    }

    #[test]
    fn pitch_matches_row_heights() {
        // AppInventoryRow heights 50/58/68 + 8px spacing (DetailRow.slint).
        assert_eq!(app_row_pitch("small"), 58.0);
        assert_eq!(app_row_pitch("medium"), 66.0);
        assert_eq!(app_row_pitch("large"), 76.0);
        assert_eq!(app_row_pitch("bogus"), 66.0);
    }

    #[test]
    fn window_covers_visible_plus_prefetch() {
        // 30 rows, pitch 66: rows 0..2 visible in 132px + 8-row buffer.
        assert_eq!(app_icon_window(30, 0.0, 132.0, 66.0), (0, 10));
        // Scrolled to row 10: window slides, clamped by prefetch.
        assert_eq!(app_icon_window(30, 660.0, 132.0, 66.0), (2, 20));
        // Near the end: clamped to total.
        assert_eq!(app_icon_window(30, 1800.0, 132.0, 66.0), (19, 30));
    }

    #[test]
    fn window_degenerate_inputs() {
        assert_eq!(app_icon_window(0, 0.0, 500.0, 66.0), (0, 0));
        assert_eq!(app_icon_window(30, 0.0, 0.0, 66.0), (0, 0));
        assert_eq!(app_icon_window(30, 0.0, -5.0, 66.0), (0, 0));
        assert_eq!(app_icon_window(30, 0.0, 500.0, 0.0), (0, 0));
        assert_eq!(app_icon_window(30, f32::NAN, 500.0, 66.0), (0, 0));
        assert_eq!(app_icon_window(30, 0.0, f32::INFINITY, 66.0), (0, 0));
        // Scrolled far past the end: empty window at the tail.
        assert_eq!(app_icon_window(30, 100000.0, 500.0, 66.0), (30, 30));
        // Negative scroll treated as top.
        assert_eq!(app_icon_window(30, -50.0, 132.0, 66.0), (0, 10));
    }

    /// Real decode fixture: the repo's own SVG icon. Slint's default
    /// `load_from_path` supports SVG/PNG/JPEG only (BMP needs the
    /// `image-default-formats` feature, which is off), and unit tests run
    /// with the package root as CWD, so this path always exists.
    fn real_icon_name() -> String {
        std::path::Path::new("ui/icons/apps.svg")
            .canonicalize()
            .expect("repo icon fixture must exist")
            .to_string_lossy()
            .into_owned()
    }

    fn fixture_model(
        _dir: &std::path::Path,
        rows: usize,
    ) -> (Vec<Application>, std::rc::Rc<slint::VecModel<AppRowData>>) {
        let svg_name = real_icon_name();
        let apps: Vec<Application> = (0..rows)
            .map(|i| {
                // Even rows resolve to a distinct real SVG copy (so each
                // decode is counted separately); odd rows are missing.
                let icon = if i % 2 == 0 {
                    let dest = _dir.join(format!("real-{i}.svg"));
                    std::fs::copy(&svg_name, &dest).expect("copy fixture icon");
                    dest.to_string_lossy().into_owned()
                } else {
                    format!("missing-icon-{i}-xyz")
                };
                fake_app(&format!("App {i}"), &icon)
            })
            .collect();
        let initial: Vec<AppRowData> = apps
            .iter()
            .map(|a| build_app_row(a, slint::Image::default(), false))
            .collect();
        (apps, std::rc::Rc::new(slint::VecModel::from(initial)))
    }

    #[test]
    fn fill_decodes_window_and_skips_rest() {
        let dir = tempfile::tempdir().unwrap();
        let (apps, model) = fixture_model(dir.path(), 30);
        let mut resolver = applications::IconResolver::new();
        let mut img_cache = applications::UiImageCache::new();

        // First visible row + prefetch: rows 0..10 (visible 0..2, buffer 8).
        let (decoded, _reused) = fill_app_icon_window(
            &model,
            &apps,
            &mut resolver,
            &mut img_cache,
            0.0,
            132.0,
            66.0,
        );
        // Even rows 0,2,4,6,8 decode; odd rows fail (memoized, counted as neither).
        assert_eq!(decoded, 5);
        for i in [0, 2, 4, 6, 8] {
            assert!(model.row_data(i).unwrap().has_icon, "row {i} decoded");
        }
        for i in [1, 3, 5, 7, 9] {
            assert!(!model.row_data(i).unwrap().has_icon, "row {i} fallback");
        }
        // Rows outside the window are untouched: path never even resolved.
        assert!(!resolver.is_path_cached("missing-icon-20-xyz"));
        assert!(!model.row_data(20).unwrap().has_icon);

        // Scrolling to rows 10..12 decodes the new window; row 0 keeps its icon.
        let (decoded2, reused2) = fill_app_icon_window(
            &model,
            &apps,
            &mut resolver,
            &mut img_cache,
            660.0,
            132.0,
            66.0,
        );
        assert!(decoded2 > 0, "new window rows decode");
        assert!(reused2 >= 1, "overlapping rows reused from cache");
        assert!(model.row_data(0).unwrap().has_icon, "row 0 keeps icon");
        assert!(model.row_data(10).unwrap().has_icon, "row 10 decoded");
        // Missing icons stay on fallback even inside the window.
        assert!(!model.row_data(11).unwrap().has_icon, "row 11 fallback");
    }

    #[test]
    fn fill_failed_decodes_memoized() {
        let mut img_cache = applications::UiImageCache::new();
        let missing = std::path::Path::new("/nonexistent/icon-xyz.png");
        assert!(img_cache.get_or_load(missing).is_none());
        assert!(img_cache.is_failed(missing));
        // Second call returns immediately from the memo.
        assert!(img_cache.get_or_load(missing).is_none());
    }

    #[test]
    fn fill_empty_model_is_noop() {
        let model = std::rc::Rc::new(slint::VecModel::<AppRowData>::default());
        let mut resolver = applications::IconResolver::new();
        let mut img_cache = applications::UiImageCache::new();
        let (decoded, reused) =
            fill_app_icon_window(&model, &[], &mut resolver, &mut img_cache, 0.0, 500.0, 66.0);
        assert_eq!((decoded, reused), (0, 0));
    }
}
