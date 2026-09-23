#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let mut args = std::env::args();
    let first = args.nth(1);
    match first {
        Some(a) if a == "tun" => std::process::exit(openflux_gui::headless_tun()),
        Some(a) if a.starts_with("openflux://") => {
            let notice = openflux_gui::headless_import(&a);
            // When launched from a browser via the openflux:// handler, stdout is lost;
            // the user sees the message in the GUI once (via take_notice).
            eprintln!("{notice}");
            openflux_gui::run(Some(notice));
        }
        _ => openflux_gui::run(None),
    }
}