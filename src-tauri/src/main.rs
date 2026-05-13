#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "--helper-burn" => {
                std::process::exit(diskcutter_lib::run_helper(&args[2..]));
            }
            // Recognised CLI subcommands route to the headless runner.
            // Anything else (or no arg) falls through to the GUI so a
            // double-click still launches normally.
            "help" | "-h" | "--help" | "version" | "-v" | "--version" | "formats" | "inspect"
            | "backup" | "snapshot" | "restore" => {
                std::process::exit(diskcutter_lib::run_cli(&args[1..]));
            }
            _ => {}
        }
    }
    diskcutter_lib::run()
}
