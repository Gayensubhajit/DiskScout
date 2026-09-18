//! Headless window-shell regression test (temporary scaffolding).
//!
//! Instantiates the real `AppWindow` on Slint's testing backend (no
//! display, no desktop disruption) and pins the scroll-geometry and
//! titlebar-wiring contracts.
//!
//! NOTE: a single #[test] on purpose — the Slint backend binds to the
//! creating thread, so parallel tests would fight over initialization.

#[cfg(test)]
mod shell_tests {
    use slint::{ComponentHandle, Model};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn window_shell() {
        i_slint_backend_testing::init_integration_test_with_system_time();

        // Geometry contract at every target size: page fills the window
        // minus titlebar (40px), header (56px) and tabs (48px); the scroll
        // area takes the page minus drive card (144px), section header
        // (40px) and page padding (2x24px); the canvas always covers the 8
        // mocked cards (8x64 + 7x10 + 2x8 = 598px).
        for (width, height) in [
            (900.0, 650.0),
            (1100.0, 760.0),
            (1440.0, 900.0),
            (1920.0, 1000.0),
        ] {
            let app = crate::AppWindow::new().expect("AppWindow must instantiate");
            app.window()
                .set_size(slint::PhysicalSize::new(width as u32, height as u32));
            // No event loop runs headlessly, so drive the same sync main()
            // performs on startup and on every resize tick.
            crate::sync_geometry(&app);

            let page_h = app.get_dbg_page_h();
            let scroll_h = app.get_dbg_scroll_h();
            let viewport_h = app.get_dbg_viewport_h();
            let list_h = app.get_dbg_list_h();
            eprintln!(
                "shell-geometry {width}x{height}: page-h={page_h} scroll-h={scroll_h} viewport-h={viewport_h} list-h={list_h}"
            );

            assert!(
                (page_h - (height - 144.0)).abs() < 1.0,
                "{width}x{height}: page must fill window minus chrome, got {page_h}"
            );
            assert!(
                (scroll_h - (height - 376.0)).abs() < 1.0,
                "{width}x{height}: scroll area must take remaining page space, got {scroll_h}"
            );
            assert!(
                (viewport_h - 672.0).abs() < 1.0,
                "{width}x{height}: viewport must cover the 9 rows, got {viewport_h}"
            );
            assert!(
                (list_h - 672.0).abs() < 1.0,
                "{width}x{height}: category list must measure its 9 rows, got {list_h}"
            );
        }

        // Titlebar wiring contract: callbacks invokable without panicking.
        // Native effects (minimize/maximize/hide) are compositor-driven and
        // verified live; the maximize handler optimistically syncs the
        // restore icon, which IS observable here.
        let app = crate::AppWindow::new().expect("AppWindow must instantiate");
        app.window().set_size(slint::PhysicalSize::new(1100, 760));
        crate::wire_titlebar(&app);

        app.invoke_minimize_requested();
        app.invoke_maximize_requested();
        app.invoke_close_requested();
        app.invoke_title_drag_move(10.0, 10.0);
        app.invoke_title_drag_end();

        assert!(
            app.get_titlebar_maximized(),
            "maximize toggle must flip the titlebar icon state"
        );

        // M5 selection contract: detail renders from a stored snapshot,
        // Back preserves the dashboard model, unknown keys are ignored.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Documents")).unwrap();
        std::fs::write(dir.path().join("Documents/a.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.path().join("loose.mp3"), vec![0u8; 50]).unwrap();
        let home = dir.path();
        let xdg = crate::xdg::XdgDirs {
            documents: home.join("Documents"),
            downloads: home.join("Downloads"),
            music: home.join("Music"),
            pictures: home.join("Pictures"),
            videos: home.join("Videos"),
        };
        let rules = crate::classify::ClassificationRules::with_xdg(home, &xdg);
        let options = crate::scan::ScanOptions {
            root: home.to_path_buf(),
            follow_symlinks: false,
            track: rules.wanted_paths(),
        };
        let scan = crate::scan::scan_blocking(&options, &AtomicBool::new(false), |_| {});
        let rules = crate::classify::ClassificationRules::with_xdg(home, &xdg);
        let classification = rules.classify(&scan);
        assert!(classification.partition_ok());

        let app = crate::AppWindow::new().expect("AppWindow must instantiate");
        app.window().set_size(slint::PhysicalSize::new(1100, 760));
        let store: crate::DetailStore = Default::default();
        *store.lock().unwrap() = Some((rules, classification, scan.file_tree.clone()));
        let mock_app = crate::applications::Application {
            id: "pacman:firefox".into(),
            name: "firefox".into(),
            display_name: "Firefox".into(),
            version: "147.0".into(),
            description: "Web Browser".into(),
            source: crate::applications::ApplicationSource::Pacman,
            kind: crate::applications::ApplicationKind::Application,
            install_scope: crate::applications::InstallScope::System,
            package_size: 482_000_000,
            measured_size: None,
            user_data_size: None,
            runtime_size: None,
            location: None,
            desktop_file: None,
            icon_name: "firefox".into(),
            is_explicit: true,
        };
        let mock_game = crate::applications::Application {
            id: "steam:431960".into(),
            name: "wallpaper_engine".into(),
            display_name: "Wallpaper Engine".into(),
            version: "2.4".into(),
            description: "Steam Game".into(),
            source: crate::applications::ApplicationSource::Steam,
            kind: crate::applications::ApplicationKind::Game,
            install_scope: crate::applications::InstallScope::User,
            package_size: 800_000_000,
            measured_size: None,
            user_data_size: None,
            runtime_size: None,
            location: None,
            desktop_file: None,
            icon_name: "steam".into(),
            is_explicit: true,
        };
        let mock_runtime = crate::applications::Application {
            id: "steam:proton".into(),
            name: "proton_exp".into(),
            display_name: "Proton Experimental".into(),
            version: "1.0".into(),
            description: "Steam Runtime".into(),
            source: crate::applications::ApplicationSource::Steam,
            kind: crate::applications::ApplicationKind::Runtime,
            install_scope: crate::applications::InstallScope::User,
            package_size: 1_200_000_000,
            measured_size: None,
            user_data_size: None,
            runtime_size: None,
            location: None,
            desktop_file: None,
            icon_name: "steam".into(),
            is_explicit: true,
        };

        let app_store: crate::applications::ApplicationStore =
            std::sync::Arc::new(std::sync::Mutex::new(Some(vec![
                mock_app,
                mock_game,
                mock_runtime,
            ])));
        let icon_resolver = std::sync::Arc::new(std::sync::Mutex::new(
            crate::applications::IconResolver::new(),
        ));
        let filtered_store = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let current_filter = std::sync::Arc::new(std::sync::Mutex::new("apps".to_string()));
        let current_search = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let current_sort = std::sync::Arc::new(std::sync::Mutex::new("size_desc".to_string()));
        let ui_image_cache = std::rc::Rc::new(std::cell::RefCell::new(
            crate::applications::UiImageCache::new(),
        ));
        let app_model_slot: crate::AppModelSlot = Default::default();

        crate::wire_selection(
            &app,
            store.clone(),
            app_store.clone(),
            icon_resolver.clone(),
            filtered_store.clone(),
            current_filter.clone(),
            current_search.clone(),
            current_sort.clone(),
            ui_image_cache.clone(),
            app_model_slot.clone(),
        );

        // Unknown keys never open the detail view.
        app.invoke_category_selected("bogus".into());
        assert_eq!(app.get_detail_category().as_str(), "");

        // Applications category: displays installed application inventory
        app.invoke_category_selected("apps".into());
        assert_eq!(app.get_detail_category().as_str(), "apps");
        assert_eq!(app.get_detail_title().as_str(), "Applications");

        // Default filter is "apps" -> only Firefox
        assert_eq!(app.get_app_rows().row_count(), 1);
        assert_eq!(app.get_count_all(), 3);
        assert_eq!(app.get_count_apps(), 1);
        assert_eq!(app.get_count_games(), 1);
        assert_eq!(app.get_count_runtimes(), 1);
        let arow = app.get_app_rows().row_data(0).expect("app row");
        assert_eq!(arow.name.as_str(), "Firefox");
        assert_eq!(arow.source.as_str(), "Pacman");
        assert_eq!(
            arow.size.as_str(),
            &crate::storage::format_bytes(482_000_000)
        );

        // Filter switching: Games
        app.invoke_app_filter_selected("games".into());
        assert_eq!(app.get_app_rows().row_count(), 1);
        let grow = app.get_app_rows().row_data(0).expect("game row");
        assert_eq!(grow.name.as_str(), "Wallpaper Engine");

        // Filter switching: Runtimes
        app.invoke_app_filter_selected("runtimes".into());
        assert_eq!(app.get_app_rows().row_count(), 1);
        let rrow = app.get_app_rows().row_data(0).expect("runtime row");
        assert_eq!(rrow.name.as_str(), "Proton Experimental");

        // Filter switching: All
        app.invoke_app_filter_selected("all".into());
        assert_eq!(app.get_app_rows().row_count(), 3);

        // Switch back to Apps
        app.invoke_app_filter_selected("apps".into());
        assert_eq!(app.get_app_rows().row_count(), 1);

        // Global Icon Size Preference
        assert_eq!(app.get_global_icon_size().as_str(), "medium");
        app.invoke_global_icon_size_selected("small".into());
        assert_eq!(app.get_global_icon_size().as_str(), "small");
        assert_eq!(app.get_explorer_icon_zoom().as_str(), "small");
        app.invoke_global_icon_size_selected("large".into());
        assert_eq!(app.get_global_icon_size().as_str(), "large");
        assert_eq!(app.get_explorer_icon_zoom().as_str(), "large");
        app.invoke_global_icon_size_selected("medium".into());

        // Click app row -> opens drilldown card
        app.invoke_app_selected(0);
        assert!(app.get_app_detail_active());
        assert_eq!(app.get_app_detail_name().as_str(), "Firefox");
        assert_eq!(app.get_app_detail_source().as_str(), "Pacman");
        assert_eq!(app.get_app_detail_scope().as_str(), "System application");
        assert_eq!(app.get_app_detail_package_name().as_str(), "firefox");

        // Back from drilldown card
        app.invoke_app_detail_back();
        assert!(!app.get_app_detail_active());
        // Back from drilldown card
        app.invoke_app_detail_back();
        assert!(!app.get_app_detail_active());

        // In-memory Search verification
        app.invoke_app_filter_selected("all".into());
        assert_eq!(app.get_app_rows().row_count(), 3);
        app.invoke_app_search_changed("wallpaper".into());
        assert_eq!(app.get_app_rows().row_count(), 1);
        let srow = app.get_app_rows().row_data(0).expect("search result");
        assert_eq!(srow.name.as_str(), "Wallpaper Engine");

        // Clear search
        app.invoke_app_search_changed("".into());
        assert_eq!(app.get_app_rows().row_count(), 3);

        // Sorting verification (Name: A -> Z)
        app.invoke_app_sort_selected("name_asc".into());
        assert_eq!(
            app.get_app_rows().row_data(0).unwrap().name.as_str(),
            "Firefox"
        );
        assert_eq!(
            app.get_app_rows().row_data(1).unwrap().name.as_str(),
            "Proton Experimental"
        );
        assert_eq!(
            app.get_app_rows().row_data(2).unwrap().name.as_str(),
            "Wallpaper Engine"
        );

        // Sorting verification (Name: Z -> A)
        app.invoke_app_sort_selected("name_desc".into());
        assert_eq!(
            app.get_app_rows().row_data(0).unwrap().name.as_str(),
            "Wallpaper Engine"
        );
        assert_eq!(
            app.get_app_rows().row_data(2).unwrap().name.as_str(),
            "Firefox"
        );

        // Sorting verification (Size: Largest first)
        app.invoke_app_sort_selected("size_desc".into());
        assert_eq!(
            app.get_app_rows().row_data(0).unwrap().name.as_str(),
            "Proton Experimental"
        ); // 1.4 GB
        assert_eq!(
            app.get_app_rows().row_data(1).unwrap().name.as_str(),
            "Wallpaper Engine"
        ); // 800 MB
        assert_eq!(
            app.get_app_rows().row_data(2).unwrap().name.as_str(),
            "Firefox"
        ); // 482 MB

        // Populated category: rows mirror contributions exactly.
        app.invoke_category_selected("document".into());
        assert_eq!(app.get_detail_title().as_str(), "Documents");
        assert_eq!(app.get_detail_total().as_str(), "100 B");
        assert_eq!(app.get_detail_rows().row_count(), 1);
        let row = app.get_detail_rows().row_data(0).expect("one row");
        assert_eq!(row.label.as_str(), "Documents");
        assert_eq!(row.size.as_str(), "100 B");
        assert_eq!(row.meta.as_str(), "1 file");
        // Detail geometry follows the same Rust-driven contract:
        // list = window minus detail chrome, canvas fits the rows.
        assert!(
            (app.get_dbg_detail_list_h() - crate::detail::detail_list_height(760.0)).abs() < 1.0
        );
        assert!((app.get_dbg_detail_viewport_h() - 72.0).abs() < 1.0);

        // Dashboard model untouched by selection (state preserved).
        assert_eq!(app.get_categories().row_count(), 9);

        // Back navigation only clears the key; everything else persists,
        // and the stored snapshot still partitions exactly.
        app.set_detail_category("".into());
        assert_eq!(app.get_detail_category().as_str(), "");
        assert_eq!(app.get_categories().row_count(), 9);
        assert!(
            store
                .lock()
                .unwrap()
                .as_ref()
                .expect("stored")
                .1
                .partition_ok(),
            "selection must leave the stored snapshot partitioned"
        );

        // ============================================================
        // Milestone 6: File & Folder Exploration verification
        // ============================================================
        std::fs::create_dir_all(dir.path().join("Documents/Projects/App")).unwrap();
        std::fs::create_dir_all(dir.path().join("Documents/Work")).unwrap();
        std::fs::create_dir_all(dir.path().join("Documents/EmptyFolder")).unwrap();
        std::fs::write(
            dir.path().join("Documents/Projects/App/main.rs"),
            vec![0u8; 1000],
        )
        .unwrap();
        std::fs::write(dir.path().join("Documents/Work/report.pdf"), vec![0u8; 500]).unwrap();

        let scan = crate::scan::scan_blocking(&options, &AtomicBool::new(false), |_| {});
        let rules_m6 = crate::classify::ClassificationRules::with_xdg(home, &xdg);
        let classification = rules_m6.classify(&scan);
        *store.lock().unwrap() = Some((rules_m6, classification, scan.file_tree.clone()));

        // 1. Category view
        app.invoke_category_selected("document".into());
        assert_eq!(app.get_detail_category().as_str(), "document");
        assert_eq!(app.get_detail_rows().row_count(), 1);

        // 2. Click contributor -> Opens Explorer root
        app.invoke_contributor_selected(0);
        assert!(app.get_explorer_active());
        assert_eq!(app.get_explorer_title().as_str(), "Documents");
        assert_eq!(app.get_explorer_parent_title().as_str(), "Documents");
        assert_eq!(
            app.get_explorer_breadcrumb().as_str(),
            "Documents › Documents"
        );
        assert!(!app.get_explorer_empty());
        assert_eq!(app.get_explorer_error().as_str(), "");

        // 3. Verify descending sort by size:
        // Projects (1000B) > Work (500B) > a.txt (100B) > EmptyFolder (0B)
        let rows = app.get_explorer_rows();
        assert_eq!(rows.row_count(), 4);
        let r0 = rows.row_data(0).unwrap();
        assert_eq!(r0.name.as_str(), "Projects");
        assert!(r0.is_dir);
        let r1 = rows.row_data(1).unwrap();
        assert_eq!(r1.name.as_str(), "Work");
        assert!(r1.is_dir);
        let r2 = rows.row_data(2).unwrap();
        assert_eq!(r2.name.as_str(), "a.txt");
        assert!(!r2.is_dir);
        let r3 = rows.row_data(3).unwrap();
        assert_eq!(r3.name.as_str(), "EmptyFolder");
        assert!(r3.is_dir);

        // 4. Drill down: Projects -> App -> main.rs
        app.invoke_explorer_navigate(0);
        assert_eq!(app.get_explorer_title().as_str(), "Projects");
        assert_eq!(app.get_explorer_parent_title().as_str(), "Documents");
        assert_eq!(
            app.get_explorer_breadcrumb().as_str(),
            "Documents › Documents › Projects"
        );

        app.invoke_explorer_navigate(0);
        assert_eq!(app.get_explorer_title().as_str(), "App");
        assert_eq!(app.get_explorer_parent_title().as_str(), "Projects");
        assert_eq!(
            app.get_explorer_breadcrumb().as_str(),
            "Documents › Documents › Projects › App"
        );
        let app_rows = app.get_explorer_rows();
        assert_eq!(app_rows.row_count(), 1);
        let file_row = app_rows.row_data(0).unwrap();
        assert_eq!(file_row.name.as_str(), "main.rs");
        assert!(!file_row.is_dir);

        // 5. Back navigation
        app.invoke_explorer_back();
        assert_eq!(app.get_explorer_title().as_str(), "Projects");

        app.invoke_explorer_back();
        assert_eq!(app.get_explorer_title().as_str(), "Documents");

        app.invoke_explorer_back();
        assert!(
            !app.get_explorer_active(),
            "Back from explorer root must return to category detail"
        );
        assert_eq!(app.get_detail_category().as_str(), "document");

        // 6. Empty directory test
        app.invoke_contributor_selected(0);
        app.invoke_explorer_navigate(3); // EmptyFolder
        assert_eq!(app.get_explorer_title().as_str(), "EmptyFolder");
        assert!(
            app.get_explorer_empty(),
            "Empty folder must report empty state"
        );

        app.invoke_explorer_back();
        assert!(!app.get_explorer_empty());
        assert_eq!(app.get_explorer_title().as_str(), "Documents");

        // 7. View mode toggle tests (Compact default -> Details -> Icons)
        assert_eq!(app.get_explorer_view_mode().as_str(), "compact");
        app.invoke_explorer_view_mode_selected("details".into());
        assert_eq!(app.get_explorer_view_mode().as_str(), "details");
        app.invoke_explorer_view_mode_selected("icons".into());
        assert_eq!(app.get_explorer_view_mode().as_str(), "icons");

        // 8. Icon zoom tests (S / M / L)
        assert_eq!(app.get_explorer_icon_zoom().as_str(), "medium");
        app.invoke_explorer_icon_zoom_selected("small".into());
        assert_eq!(app.get_explorer_icon_zoom().as_str(), "small");
        app.invoke_explorer_icon_zoom_selected("large".into());
        assert_eq!(app.get_explorer_icon_zoom().as_str(), "large");

        // 9. Sort selection tests (direct key selection via sort popover)
        assert_eq!(app.get_explorer_sort_label().as_str(), "Size ↓");
        app.invoke_explorer_sort_selected("size_asc".into());
        assert_eq!(app.get_explorer_sort_label().as_str(), "Size ↑");
        let r0 = app.get_explorer_rows().row_data(0).unwrap();
        assert_eq!(r0.name.as_str(), "EmptyFolder");
        app.invoke_explorer_sort_selected("name_asc".into());
        assert_eq!(app.get_explorer_sort_label().as_str(), "Name A-Z");
        let r0 = app.get_explorer_rows().row_data(0).unwrap();
        assert_eq!(r0.name.as_str(), "a.txt");

        // 10. Test trigger sort menu callback
        app.invoke_trigger_sort_menu();

        // 11. Viewport-based icon windowing (M7.2.2): populate 40 rows with
        // distinct real icons, then simulate scroll positions through the
        // exact production fill path. Headless Flickable geometry reads 0,
        // so scroll offsets are passed explicitly like the live timer does.
        let icon_dir = tempfile::tempdir().unwrap();
        let svg_src = std::path::Path::new("ui/icons/apps.svg")
            .canonicalize()
            .expect("repo icon fixture must exist");
        let mut windowing_apps = Vec::new();
        for i in 0..40 {
            let icon = if i % 2 == 0 {
                let dest = icon_dir.path().join(format!("wicon-{i}.svg"));
                std::fs::copy(&svg_src, &dest).expect("copy fixture icon");
                dest.to_string_lossy().into_owned()
            } else {
                format!("missing-wicon-{i}-xyz")
            };
            windowing_apps.push(crate::applications::Application {
                id: format!("test:wapp-{i}"),
                name: format!("wapp-{i}"),
                display_name: format!("Window App {i}"),
                version: "1.0".into(),
                description: String::new(),
                source: crate::applications::ApplicationSource::Manual,
                kind: crate::applications::ApplicationKind::Application,
                install_scope: crate::applications::InstallScope::User,
                package_size: 1000 + i as u64,
                measured_size: None,
                user_data_size: None,
                runtime_size: None,
                location: None,
                desktop_file: None,
                icon_name: icon,
                is_explicit: true,
            });
        }
        let mut wresolver = crate::applications::IconResolver::new();
        let mut wcache = crate::applications::UiImageCache::new();
        let wfiltered: std::sync::Arc<std::sync::Mutex<Vec<crate::applications::Application>>> =
            Default::default();
        let wslot: crate::AppModelSlot = Default::default();
        crate::populate_app_rows(
            &app,
            &windowing_apps,
            "all",
            "",
            "size_desc",
            0,
            &wfiltered,
            &wslot,
        );
        assert_eq!(app.get_app_count(), 40);
        let wmodel = wslot.borrow().as_ref().expect("model stored").clone();
        // Populate paints fallback rows immediately; the scroll timer fills
        // the visible window on its next tick. Here we drive the same fill
        // explicitly with fallback geometry (scroll 0, 600px pre-layout).
        let sorted = wfiltered.lock().unwrap().clone();
        assert_eq!(sorted.len(), 40);
        let (d0, _) = crate::fill_app_icon_window(
            &wmodel,
            &sorted,
            &mut wresolver,
            &mut wcache,
            0.0,
            600.0,
            66.0,
        );
        let decoded0 = (0..40)
            .filter(|&i| wmodel.row_data(i).unwrap().has_icon)
            .count();
        assert!(d0 > 0 && decoded0 == d0, "fill decodes the visible window");
        assert!(decoded0 < 40, "fill must not decode everything");
        // Row 39 holds the smallest app (app 0, real icon): far outside any
        // plausible initial window, so it must be fallback + unresolved.
        assert!(!wmodel.row_data(39).unwrap().has_icon, "tail untouched");
        assert!(
            !wresolver.is_path_cached(&sorted[39].icon_name),
            "tail path never resolved"
        );
        // Simulate scroll: window (12, 35) is pure geometry (see unit
        // tests). Afterwards every real-icon row in the window is decoded,
        // and decoded+reused equals the independently counted even apps.
        let even_in_window = sorted[12..35]
            .iter()
            .filter(|a| {
                a.name
                    .rsplit('-')
                    .next()
                    .and_then(|n| n.parse::<usize>().ok())
                    .map(|n| n % 2 == 0)
                    .unwrap_or(false)
            })
            .count();
        let (d2, r2) = crate::fill_app_icon_window(
            &wmodel,
            &sorted,
            &mut wresolver,
            &mut wcache,
            20.0 * 66.0,
            400.0,
            66.0,
        );
        assert_eq!(d2 + r2, even_in_window, "window fully processed");
        for (r, app) in sorted.iter().enumerate().take(35).skip(12) {
            let idx: usize = app.name.rsplit('-').next().unwrap().parse().unwrap();
            if idx % 2 == 0 {
                assert!(wmodel.row_data(r).unwrap().has_icon, "row {r} decoded");
            } else {
                assert!(!wmodel.row_data(r).unwrap().has_icon, "row {r} fallback");
            }
        }
        // Tail still untouched; identical repeat fill decodes nothing new.
        assert!(
            !wmodel.row_data(39).unwrap().has_icon,
            "tail still untouched"
        );
        let (d3, r3) = crate::fill_app_icon_window(
            &wmodel,
            &sorted,
            &mut wresolver,
            &mut wcache,
            20.0 * 66.0,
            400.0,
            66.0,
        );
        assert_eq!(d3, 0, "repeat fill decodes nothing");
        assert_eq!(r3, d2 + r2, "repeat fill reuses everything");
    }
}
