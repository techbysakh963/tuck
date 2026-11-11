use libadwaita as adw;
use libadwaita::prelude::*;

fn main() -> adw::glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(tuck::APP_ID)
        .build();
    app.connect_activate(|_| {});
    app.run()
}
