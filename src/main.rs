mod platform;
mod storage;

use storage::{StorageInfo, format_bytes};

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let app = AppWindow::new()?;

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

    app.run()?;
    Ok(())
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
