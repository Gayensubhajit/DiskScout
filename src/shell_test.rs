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
    use slint::ComponentHandle;

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
                (viewport_h - 598.0).abs() < 1.0,
                "{width}x{height}: viewport must cover the 8 cards, got {viewport_h}"
            );
            assert!(
                (list_h - 598.0).abs() < 1.0,
                "{width}x{height}: category list must measure its 8 cards, got {list_h}"
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
    }
}
