// SPDX-License-Identifier: GPL-3.0-or-later
//! Prints every clipboard change the capture backend reports.

fn main() {
    let env = tuck::capture::probe_session();
    let Some(kind) = tuck::capture::select_backend(&env) else {
        eprintln!("no clipboard backend available in this session: {env:?}");
        std::process::exit(1);
    };
    eprintln!("backend: {kind:?}");
    let (tx, rx) = async_channel::unbounded();
    let (status_tx, status_rx) = async_channel::unbounded();
    let _watcher = tuck::capture::spawn(tx, status_tx);
    std::thread::spawn(move || {
        while let Ok(status) = status_rx.recv_blocking() {
            eprintln!("watcher status: {status:?}");
        }
    });
    while let Ok(event) = rx.recv_blocking() {
        let summary = match &event.captured {
            tuck::capture::Captured::Text { text, html } => {
                format!(
                    "text {} chars, html: {}",
                    text.chars().count(),
                    html.is_some()
                )
            }
            tuck::capture::Captured::Image { png } => format!("image {} bytes", png.len()),
            tuck::capture::Captured::Files(files) => format!("files {files:?}"),
        };
        println!(
            "{summary} | source {:?} | hint {:?} | {} mime types",
            event.source,
            event.secret_hint,
            event.mime_types.len()
        );
    }
}
