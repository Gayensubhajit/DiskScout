mod classify;
mod cleanup;
mod detail;
mod explorer;
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
            std::sync::Arc<scan::FileTree>,
        )>,
    >,
>;

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
struct ExplorerNavState {
    category: Option<classify::Category>,
    current_detail_rows: Vec<detail::DetailItem>,
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
    let content_w = (win_size.width - 230.0 - 64.0).max(400.0);

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

fn wire_selection(app: &AppWindow, store: DetailStore) {
    let nav_state = std::rc::Rc::new(std::cell::RefCell::new(ExplorerNavState::default()));

    // 1. Category click -> Category detail view
    let weak = app.as_weak();
    let nav = nav_state.clone();
    let store_ref = store.clone();
    app.on_category_selected(move |key: slint::SharedString| {
        let Some(app) = weak.upgrade() else {
            return;
        };
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
        st.history.clear();
        st.page_idx = 0;
        app.set_explorer_active(false);

        app.set_detail_title(category.display_name().into());
        app.set_detail_total(format_bytes(detail.total_bytes).into());
        app.set_detail_rows(slint::ModelRc::new(std::rc::Rc::new(
            slint::VecModel::from(rows),
        )));
        app.set_detail_viewport_h(detail::detail_viewport_height(detail.rows.len()));
        app.set_detail_list_h(detail::detail_list_height(height));
        app.set_detail_category(key);
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

        match explorer::resolve_contributor_root(file_tree, &item.path) {
            Ok(dir_id) => {
                st.current_scope = item.scope.clone();
                st.history.clear();
                st.history.push(NavBreadcrumb {
                    title: item.label.clone(),
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
    *store.lock().expect("detail store poisoned") =
        Some((rules, classification.clone(), result.file_tree.clone()));
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
            match std::env::var("DISKSCOUT_START_PAGE").as_deref() {
                Ok("apps_detail") => app.invoke_category_selected("apps".into()),
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
