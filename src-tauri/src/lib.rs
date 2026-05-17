pub mod backup;
pub mod catalog;
pub mod cli;
pub mod commands;
mod db;
pub mod decoder_chain;
#[cfg(target_os = "macos")]
mod disk_arb;
mod disks;
pub mod doctor;
pub mod forensic;
pub mod hash;
mod helper;
pub mod image;
pub mod inspect;
pub mod pipeline;
pub mod qemu;
pub mod readers;
pub mod snapshot;
pub mod source;
pub mod sparse;
pub mod url_fetch;
pub mod validate;
pub mod writers;
pub mod xz_footer;

pub use cli::run_cli;
pub use helper::run_helper;

use db::{
    burn_jobs_active, burn_jobs_clear, burn_jobs_list, burn_logs_for_job, burn_logs_list,
    config_all, config_get, config_set, enqueue_burn, remove_burn_job, set_burn_target, Db,
};
use disks::{
    abort_and_quit, app_info, cancel_write, check_fda, find_orphan_helpers, has_active_burns,
    inspect_image, kill_orphan_helpers, list_disks, open_fda_settings, reattach_running_helpers,
    start_write, verify_image, ActiveBurns, CancelRegistry, ElevatedJobs,
};
use std::sync::Mutex;
use tauri::menu::{AboutMetadataBuilder, MenuBuilder, SubmenuBuilder};
use tauri::{Emitter, Manager, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(CancelRegistry::default())
        .manage(ActiveBurns::default())
        .manage(ElevatedJobs::default())
        .manage(url_fetch::DownloadRegistry::default())
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let active = app.state::<ActiveBurns>();
                if !active.is_empty() {
                    api.prevent_close();
                    let snapshot = active.snapshot();
                    let payload: Vec<_> = snapshot
                        .iter()
                        .map(|b| {
                            serde_json::json!({
                                "job_id": b.job_id,
                                "target": b.target,
                            })
                        })
                        .collect();
                    let _ = window.emit(
                        "disk-cutter://close-blocked",
                        serde_json::json!({ "active": payload }),
                    );
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            open_fda_settings,
            check_fda,
            find_orphan_helpers,
            kill_orphan_helpers,
            list_disks,
            inspect_image,
            start_write,
            cancel_write,
            has_active_burns,
            abort_and_quit,
            verify_image,
            config_get,
            config_set,
            config_all,
            burn_jobs_list,
            burn_jobs_active,
            burn_jobs_clear,
            enqueue_burn,
            remove_burn_job,
            set_burn_target,
            burn_logs_list,
            burn_logs_for_job,
            commands::inspect_partitions,
            commands::inspect_image_partitions,
            commands::inspect_image_bootable,
            commands::capture_snapshot,
            commands::restore_snapshot,
            commands::export_burn_report,
            commands::run_backup,
            doctor::doctor,
            qemu::qemu_check,
            qemu::qemu_test_image,
            url_fetch::start_download,
            url_fetch::cancel_download,
            catalog::catalog_list,
            catalog::catalog_refresh,
            validate::validate_image_contents,
        ])
        .setup(|app| {
            // Persistence is load-bearing: burn_jobs is the source of truth
            // for both live queue and history, and every Tauri command on the
            // burn hot-path expects Db state to be managed. A failed open
            // must abort startup — a silent fallthrough leaves the app
            // looking healthy until the operator clicks Burn and `state::<Db>()`
            // panics mid-write.
            let conn = db::open(app.handle()).map_err(|e| format!("db::open failed: {e}"))?;
            app.manage(Db(Mutex::new(conn)));
            // After the DB is managed: walk the non-terminal
            // burn_jobs rows and rehook the UI to any helper
            // processes that survived a prior parent-process
            // restart. Rows whose helper is no longer alive
            // get marked EORPHAN here so the frontend
            // hydrate won't show them as eternally running.
            reattach_running_helpers(app.handle());

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
