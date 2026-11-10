//! Vortex GTK entry point: root check, then the libadwaita application.

mod app;
mod game_object;
mod ui;

fn main() -> libadwaita::glib::ExitCode {
    if rustix::process::geteuid().is_root() {
        eprintln!("Vortex refuses to run as root. Start it as your normal user.");
        std::process::exit(1);
    }
    app::run()
}
