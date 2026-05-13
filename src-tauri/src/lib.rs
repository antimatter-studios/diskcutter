pub mod backup;
pub mod cli;
pub mod commands;
mod db;
#[cfg(target_os = "macos")]
mod disk_arb;
mod disks;
pub mod forensic;
pub mod hash;
mod helper;
pub mod inspect;
pub mod pipeline;
pub mod readers;
pub mod snapshot;
pub mod sparse;
pub mod writers;

pub use cli::run_cli;
pub use helper::run_helper;

use db::{
    burn_history_clear, burn_history_list, burn_logs_list, config_all, config_get, config_set, Db,
};
use disks::{
    app_info, cancel_write, find_orphan_helpers, inspect_image, kill_orphan_helpers, list_disks,
    open_fda_settings, start_write, verify_image, CancelRegistry,
};
use std::sync::Mutex;
use tauri::menu::{AboutMetadataBuilder, MenuBuilder, SubmenuBuilder};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(CancelRegistry::default())
        .invoke_handler(tauri::generate_handler![
            app_info,
            open_fda_settings,
            find_orphan_helpers,
            kill_orphan_helpers,
            list_disks,
            inspect_image,
            start_write,
            cancel_write,
            verify_image,
            config_get,
            config_set,
            config_all,
            burn_history_list,
            burn_history_clear,
            burn_logs_list,
            commands::inspect_partitions,
            commands::capture_snapshot,
            commands::restore_snapshot,
            commands::export_burn_report,
            commands::run_backup,
        ])
        .setup(|app| {
            match db::open(app.handle()) {
                Ok(conn) => {
                    app.manage(Db(Mutex::new(conn)));
                }
                Err(e) => {
                    eprintln!("db::open failed, continuing without persistence: {e}");
                }
            }

            let authors: Vec<String> = env!("CARGO_PKG_AUTHORS")
                .split(':')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();

            let about = AboutMetadataBuilder::new()
                .name(Some("Disk Cutter"))
                .version(Some(env!("CARGO_PKG_VERSION")))
                .authors(Some(authors))
                .comments(Some(env!("CARGO_PKG_DESCRIPTION")))
                .copyright(Some(format!("© {} Chris Thomas", current_year())))
                .build();

            let app_submenu = SubmenuBuilder::new(app, "Disk Cutter")
                .about(Some(about))
                .separator()
                .services()
                .separator()
                .hide()
                .hide_others()
                .show_all()
                .separator()
                .quit()
                .build()?;

            let edit_submenu = SubmenuBuilder::new(app, "Edit")
                .undo()
                .redo()
                .separator()
                .cut()
                .copy()
                .paste()
                .select_all()
                .build()?;

            let window_submenu = SubmenuBuilder::new(app, "Window")
                .minimize()
                .maximize()
                .separator()
                .close_window()
                .build()?;

            let menu = MenuBuilder::new(app)
                .items(&[&app_submenu, &edit_submenu, &window_submenu])
                .build()?;

            app.set_menu(menu)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn current_year() -> i32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    1970 + (secs / 31_557_600) as i32
}
