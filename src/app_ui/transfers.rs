use slint::ComponentHandle;

use crate::AppWindow;

pub(crate) fn install(app: &AppWindow) {
    app.on_transfers_open_add({
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_transfers_add_open(true);
            }
        }
    });

    app.on_transfers_close_add({
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                close_add_overlay(&app);
            }
        }
    });

    app.on_transfers_submit_magnet({
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                close_add_overlay(&app);
            }
        }
    });

    app.on_transfers_pick_torrent_file({
        let weak = app.as_weak();
        move || {
            let _picked = rfd::FileDialog::new()
                .add_filter("Torrent files", &["torrent"])
                .pick_file();

            if let Some(app) = weak.upgrade() {
                close_add_overlay(&app);
            }
        }
    });
}

fn close_add_overlay(app: &AppWindow) {
    app.set_transfers_add_open(false);
    app.set_transfers_magnet("".into());
}
