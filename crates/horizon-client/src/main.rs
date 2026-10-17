//! Horizon: Rust client for Minecraft Java Edition 26.3 gameplay.
#![forbid(unsafe_code)]
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
mod game;

#[cfg(not(target_arch = "wasm32"))]
use horizon_client::{crash, logging};

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    horizon_client::telemetry::mark_start();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let game_args = args.iter().take_while(|a| *a != "--dedicated");
    let help = game_args.clone().any(|a| a == "--help" || a == "-h");
    // The dedicated server logs in its own folder, as vanilla's does; the game in its home.
    let (home, side, thread) = if args.iter().any(|a| a == "--dedicated") {
        (std::path::PathBuf::from("."), "server", "Server thread")
    } else {
        (logging::game_home(), "client", "Render thread")
    };
    if !help {
        logging::init(&home, thread);
        crash::install(home.clone(), side);
    }
    if let Err(error) = game::run() {
        log::error!("Horizon: {error}");
        if !help {
            crash::report_error(&home, side, &*error);
        }
        // Started from Explorer, the console window closes with the game:
        // keep the message on screen until it is read.
        #[cfg(windows)]
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            eprintln!("Press Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
    logging::flush();
    if game::restart_requested() {
        std::process::exit(horizon_server_files::ENGINE_RESTART);
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // Workers instantiate the same module to run their assigned Rust closure.
    // Only the window owns resources, audio, rendering and the event loop.
    if std::thread::is_web_worker_thread() {
        return;
    }
    std::thread::Builder::new()
        .worker_script_url("/rust-worker.js".into())
        .set_default();
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    horizon_client::telemetry::mark_start();
    wasm_bindgen_futures::spawn_local(async {
        if let Err(error) = horizon_client::browser::ready().await {
            log::error!("Horizon: browser preparation failed: {error:?}");
            horizon_client::browser::startup_error(
                &error.as_string().unwrap_or_else(|| format!("{error:?}")),
            );
            return;
        }
        horizon_client::browser::start_persistence();
        if let Err(error) = game::run() {
            log::error!("Horizon: {error}");
            horizon_client::browser::startup_error(&error.to_string());
        }
    });
}
