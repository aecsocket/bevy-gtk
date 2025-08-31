use {
    gio::prelude::{ApplicationExt, ApplicationExtManual},
    gtk4::prelude::GtkWindowExt,
};

extern crate libadwaita as adw;

fn main() {
    let app = adw::Application::builder().build();
    app.connect_activate(|app| {
        adw::ApplicationWindow::new(app).present();
    });
    app.run();
}
