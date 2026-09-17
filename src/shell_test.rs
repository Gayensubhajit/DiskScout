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
        crate::wire_selection(&app, store.clone());

        // Unknown keys never open the detail view.
        app.invoke_category_selected("bogus".into());
        assert_eq!(app.get_detail_category().as_str(), "");

        // Empty category: title set, zero rows (empty state shows).
        app.invoke_category_selected("apps".into());
        assert_eq!(app.get_detail_category().as_str(), "apps");
        assert_eq!(app.get_detail_title().as_str(), "Applications");
        assert_eq!(app.get_detail_rows().row_count(), 0);

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
    }
}
