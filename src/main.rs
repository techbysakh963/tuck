// SPDX-License-Identifier: GPL-3.0-or-later
fn main() -> libadwaita::glib::ExitCode {
    // The popup is a small static list, so the GPU renderer buys nothing, and
    // loading the GL/Vulkan driver stack keeps over 100 MB resident for as long
    // as the app lives. An explicit GSK_RENDERER from the user still wins.
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "cairo");
    }
    tuck::app::run()
}
