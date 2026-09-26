#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The headless subcommands below print their result into a pipe; a closed one must end the
    // process with the signal rather than panic on "failed printing to stdout".
    openflux::restore_default_sigpipe();
    let mut args = std::env::args();
    let first = args.nth(1);
    match first {
        // Both privileged subcommands must be handled *before* the GTK/Tao event loop is
        // built: the GUI reaches them through `pkexec <self> ...`, where there is no display,
        // and tao panics ("Failed to initialize GTK backend!") if it is started anyway.
        Some(a) if a == "tun" || a == "disconnect" => {
            std::process::exit(openflux_gui::headless_command())
        }
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
