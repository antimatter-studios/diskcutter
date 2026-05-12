#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--helper-burn" {
        std::process::exit(disk_cutter_lib::run_helper(&args[2..]));
    }
    disk_cutter_lib::run()
}
