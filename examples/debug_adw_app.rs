use {
    gio::prelude::{ApplicationExt, ApplicationExtManual},
    gtk::prelude::GtkWindowExt,
};

fn main() {
    let app = adw::Application::builder().build();
    app.connect_activate(|app| {
        adw::ApplicationWindow::new(app).present();
    });
    app.run();
}
