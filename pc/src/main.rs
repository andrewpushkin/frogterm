//! PC tools for the reTerminal photo + weather frame.
//!
//!   frogterm-pc check [<album-link>]   verify the Google Photos album works without login
//!   frogterm-pc emulator               the device in a window: the firmware's logic, three clickable buttons

mod check;
mod config;
mod emulator;
mod logger;
mod net;
#[allow(dead_code)] // used by the emulator from the next step
mod portal_server;
mod window;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("check") => check::main(&args[1..]),
        Some("emulator") => emulator::main(&args[1..]),
        _ => {
            eprintln!(
                "usage: frogterm-pc check [<album-link>] [options]   (see: frogterm-pc check --help)\n       \
                 frogterm-pc emulator [options]           (see: frogterm-pc emulator --help)"
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("\nFAILED: {e:#}");
        std::process::exit(1);
    }
}
