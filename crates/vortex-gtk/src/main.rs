//! Vortex GTK entry point: root check, then the libadwaita application window.

use libadwaita::prelude::*;

const APP_ID: &str = "io.github.nexusdeveloper902.Vortex";

fn main() {
    if rustix::process::geteuid().is_root() {
        eprintln!("Vortex refuses to run as root. Start it as your normal user.");
        std::process::exit(1);
    }

    let app = libadwaita::Application::builder()
        .application_id(APP_ID)
        .build();
    app.connect_activate(|app| {
        let page = libadwaita::StatusPage::builder()
            .title("No games yet")
            .build();
        let window = libadwaita::ApplicationWindow::builder()
            .application(app)
            .title("Vortex")
            .default_width(900)
            .default_height(600)
            .content(&page)
            .build();
        window.present();
    });
    app.run();
}
