//! The resident application: actions, command line, capture wiring.

pub mod clipboard_page;
pub mod first_run;
pub mod grid_page;
pub mod paste_flow;
pub mod preferences;
pub mod row;
pub mod symbols;
pub mod window;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::capture::{self, BackendKind, CaptureEvent};
use crate::echo::EchoGuard;
use crate::history::HistoryStore;
use crate::ingest::{ingest, IngestResult};
use crate::paste::{Combo, KeySender, UinputKeyboard, XTestSender};
use crate::privacy::PrivacyFilter;
use crate::settings::Settings;
use crate::target::{detect_target, TargetKind};
use crate::tray::{Tray, TrayCommand};
use crate::APP_ID;

pub use window::PopupWindow;

/// Delay after the popup unmaps before keys are sent, so the compositor has
/// handed focus back to the target.
const FOCUS_RETURN_DELAY: Duration = Duration::from_millis(120);
const UNMAP_TIMEOUT: Duration = Duration::from_millis(300);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The one handle the pages and flows receive.
pub struct AppCtx {
    pub store: Rc<RefCell<HistoryStore>>,
    pub settings: Rc<RefCell<Settings>>,
    pub echo: Rc<RefCell<EchoGuard>>,
    pub keys: Rc<RefCell<Box<dyn KeySender>>>,
    pub target: Cell<TargetKind>,
    pub window: PopupWindow,
    pub app: adw::Application,
    pub filter: RefCell<PrivacyFilter>,
    pub config_path: PathBuf,
    pub is_x11: bool,
    tray: RefCell<Option<Tray>>,
    tray_tx: async_channel::Sender<TrayCommand>,
    toasts: RefCell<Vec<String>>,
    save_pending: Cell<bool>,
    opening: Cell<bool>,
    shown_since: Cell<Option<Instant>>,
}

impl AppCtx {
    /// Makes the top-bar icon match the "show icon" setting.
    pub fn sync_tray(&self) {
        let (want, paused) = {
            let s = self.settings.borrow();
            (s.show_tray_icon, s.paused)
        };
        let mut tray = self.tray.borrow_mut();
        match (want, tray.is_some()) {
            (true, false) => *tray = Tray::show(self.tray_tx.clone(), paused),
            (false, true) => {
                if let Some(t) = tray.take() {
                    t.remove();
                }
            }
            _ => {}
        }
    }

    /// Shown the next time the popup opens (and logged by the caller).
    pub fn queue_toast(&self, message: &str) {
        self.toasts.borrow_mut().push(message.to_string());
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Writes the history shortly after it last changed. The timer exists only
/// while there is something to write.
pub fn schedule_save(ctx: &Rc<AppCtx>) {
    if ctx.save_pending.get() || !ctx.store.borrow().is_dirty() {
        return;
    }
    ctx.save_pending.set(true);
    let ctx = ctx.clone();
    glib::timeout_add_local_once(SAVE_DEBOUNCE, move || {
        ctx.save_pending.set(false);
        if let Err(e) = ctx.store.borrow_mut().save() {
            log::error!("cannot save history: {e}");
        }
    });
}

/// Fails every key press: used when no paste backend could be set up, so the
/// user sees why pasting does nothing.
struct UnavailableKeys(String);

impl KeySender for UnavailableKeys {
    fn send(&mut self, _: Combo) -> Result<(), String> {
        Err(self.0.clone())
    }
}

fn data_dir() -> PathBuf {
    glib::user_data_dir().join("tuck")
}

fn prune_by_age(ctx: &AppCtx) {
    let days = ctx.settings.borrow().auto_clear_days;
    if let Some(days) = days {
        let removed = ctx
            .store
            .borrow_mut()
            .prune_older_than(i64::from(days) * DAY_MS, now_ms());
        if removed > 0 {
            log::info!("removed {removed} items past the auto-clear age");
        }
    }
}

fn build_ctx(app: &adw::Application) -> Rc<AppCtx> {
    let data = data_dir();
    let config_path = glib::user_config_dir().join("tuck").join("settings.json");

    if let Err(e) = crate::logging::init_logging(&data.join("logs"), log::LevelFilter::Info) {
        eprintln!("tuck: logging disabled: {e}");
    }
    let (settings, settings_backup) = Settings::load_checked(&config_path);
    let (store, report) = HistoryStore::open(&data, settings.max_items);

    let mut toasts = Vec::new();
    if let Some(path) = settings_backup {
        log::warn!("settings were unreadable, moved to {}", path.display());
        toasts.push(format!(
            "Settings could not be read and were reset. The old file was kept as {}.",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    if let Some(path) = report.recovered_from_corrupt {
        log::warn!("history was unreadable, moved to {}", path.display());
        toasts.push(format!(
            "History could not be read and was reset. The old file was kept as {}.",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    if report.newer_version.is_some() {
        toasts.push(
            "History was saved by a newer Tuck. It is left unchanged and new copies are not saved."
                .to_string(),
        );
    }
    if report.dir_unavailable {
        toasts.push("Cannot create the history folder. History will not be saved.".to_string());
    }

    let is_x11 = std::env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty());
    let keys: Box<dyn KeySender> = if is_x11 {
        match XTestSender::connect() {
            Ok(k) => Box::new(k),
            Err(e) => {
                log::error!("no paste backend: {e}");
                toasts.push("Pasting is unavailable: cannot reach the X server".into());
                Box::new(UnavailableKeys(e))
            }
        }
    } else {
        match UinputKeyboard::create() {
            Ok(k) => Box::new(k),
            Err(e) => {
                log::error!("no paste backend: {e}");
                toasts.push("Paste needs access to /dev/uinput (see the README)".into());
                Box::new(UnavailableKeys(e))
            }
        }
    };

    let filter = PrivacyFilter::new(&settings.exclusions);
    let window = PopupWindow::new(app);
    let (tray_tx, tray_rx) = async_channel::unbounded::<TrayCommand>();
    let ctx = Rc::new(AppCtx {
        store: Rc::new(RefCell::new(store)),
        settings: Rc::new(RefCell::new(settings)),
        echo: Rc::new(RefCell::new(EchoGuard::new())),
        keys: Rc::new(RefCell::new(keys)),
        target: Cell::new(TargetKind::Unknown),
        window,
        app: app.clone(),
        filter: RefCell::new(filter),
        config_path,
        is_x11,
        tray: RefCell::new(None),
        tray_tx,
        toasts: RefCell::new(toasts),
        save_pending: Cell::new(false),
        opening: Cell::new(false),
        shown_since: Cell::new(None),
    });
    ctx.window.attach(&ctx);

    let weak = Rc::downgrade(&ctx);
    ctx.window.window.connect_map(move |_| {
        if let Some(ctx) = weak.upgrade() {
            if let Some(start) = ctx.shown_since.take() {
                log::info!("popup shown in {} ms", start.elapsed().as_millis());
            }
        }
    });

    install_actions(&ctx);
    start_capture(&ctx);
    handle_tray_commands(app, tray_rx);
    ctx.sync_tray();
    ctx
}

/// Runs what the top-bar icon asked for, on the main loop.
fn handle_tray_commands(app: &adw::Application, rx: async_channel::Receiver<TrayCommand>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        while let Ok(command) = rx.recv().await {
            match command {
                TrayCommand::Toggle => app.activate_action("toggle", None),
                TrayCommand::Preferences => app.activate_action("preferences", None),
                TrayCommand::TogglePause => app.activate_action("pause", None),
                TrayCommand::Quit => app.activate_action("quit", None),
            }
        }
    });
}

fn start_capture(ctx: &Rc<AppCtx>) {
    let env = capture::probe_session();
    let Some(kind) = capture::select_backend(&env) else {
        log::error!("no clipboard backend available: {env:?}");
        ctx.queue_toast("Cannot watch the clipboard in this session");
        return;
    };
    log::info!("clipboard backend: {kind:?}");
    let _: BackendKind = kind;
    let (tx, rx) = async_channel::unbounded::<CaptureEvent>();
    // The watcher thread ends with the process; nothing joins it.
    drop(capture::spawn(kind, tx));

    let ctx = ctx.clone();
    glib::spawn_future_local(async move {
        while let Ok(event) = rx.recv().await {
            on_capture(&ctx, event);
        }
        log::error!("clipboard watcher channel closed");
    });
}

fn on_capture(ctx: &Rc<AppCtx>, event: CaptureEvent) {
    prune_by_age(ctx);
    let paused = ctx.settings.borrow().paused;
    let result = {
        let mut store = ctx.store.borrow_mut();
        let filter = ctx.filter.borrow();
        let mut echo = ctx.echo.borrow_mut();
        ingest(event, &mut store, &filter, &mut echo, paused, now_ms())
    };
    match result {
        Ok(IngestResult::Stored(_)) => {
            schedule_save(ctx);
            if ctx.window.is_visible() {
                ctx.window.clipboard.refresh(false);
            }
        }
        Ok(IngestResult::Skipped(reason)) => log::debug!("capture skipped: {reason:?}"),
        Err(e) => log::warn!("capture failed: {e}"),
    }
}

fn install_actions(ctx: &Rc<AppCtx>) {
    let app = &ctx.app;

    let toggle = gio::SimpleAction::new("toggle", None);
    let c = ctx.clone();
    toggle.connect_activate(move |_, _| {
        let c = c.clone();
        glib::spawn_future_local(async move { toggle_popup(&c).await });
    });
    app.add_action(&toggle);

    let preferences = gio::SimpleAction::new("preferences", None);
    let c = ctx.clone();
    preferences.connect_activate(move |_, _| {
        c.window.present();
        preferences::present(&c);
    });
    app.add_action(&preferences);

    let paused = ctx.settings.borrow().paused;
    let pause = gio::SimpleAction::new_stateful("pause", None, &paused.to_variant());
    let c = ctx.clone();
    pause.connect_activate(move |action, _| {
        let now_paused = !action
            .state()
            .and_then(|s| s.get::<bool>())
            .unwrap_or(false);
        action.set_state(&now_paused.to_variant());
        {
            let mut settings = c.settings.borrow_mut();
            settings.paused = now_paused;
            if let Err(e) = settings.save(&c.config_path) {
                log::error!("cannot save settings: {e}");
            }
        }
        if let Some(tray) = c.tray.borrow().as_ref() {
            tray.set_paused(now_paused);
        }
    });
    app.add_action(&pause);

    let clear = gio::SimpleAction::new("clear", None);
    let c = ctx.clone();
    clear.connect_activate(move |_, _| {
        {
            let mut store = c.store.borrow_mut();
            store.clear_unpinned();
            store.discard_corrupt_backups();
        }
        schedule_save(&c);
        c.window.clipboard.refresh(false);
    });
    app.add_action(&clear);

    let about = gio::SimpleAction::new("about", None);
    let c = ctx.clone();
    about.connect_activate(move |_, _| {
        let dialog = adw::AboutDialog::builder()
            .application_name("Tuck")
            .application_icon(APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("Tuck contributors")
            .website("https://github.com/techbysakh963/tuck")
            .license_type(gtk::License::Gpl30)
            .build();
        dialog.present(Some(&c.window.window));
    });
    app.add_action(&about);

    let quit = gio::SimpleAction::new("quit", None);
    let a = app.clone();
    quit.connect_activate(move |_, _| a.quit());
    app.add_action(&quit);
}

async fn toggle_popup(ctx: &Rc<AppCtx>) {
    if ctx.window.is_visible() {
        ctx.window.hide();
    } else {
        open_popup(ctx).await;
    }
}

/// Opens the popup: detect where a paste would go first (while the target
/// still has focus), then show the window.
pub async fn open_popup(ctx: &Rc<AppCtx>) {
    if ctx.opening.replace(true) {
        return;
    }
    let started = Instant::now();
    let is_x11 = ctx.is_x11;
    let kind = gio::spawn_blocking(move || detect_target(is_x11))
        .await
        .unwrap_or(TargetKind::Unknown);
    ctx.target.set(kind);
    log::info!("paste target: {kind:?}");

    prune_by_age(ctx);
    ctx.window.prepare_to_show();
    ctx.shown_since.set(Some(started));
    ctx.window.present();
    if ctx.is_x11 {
        center_on_pointer_monitor(&ctx.window.window);
    }
    ctx.opening.set(false);

    if !ctx.settings.borrow().first_run_done {
        first_run::present(ctx);
    }

    let pending: Vec<String> = ctx.toasts.borrow_mut().drain(..).collect();
    for message in pending {
        ctx.window.add_toast(&message);
    }
}

/// On X11 the toolkit cannot place windows, so move ours to the middle of the
/// monitor holding the pointer. Best effort: a window manager may still
/// override it. On Wayland the compositor decides and nothing is attempted.
fn center_on_pointer_monitor(window: &adw::ApplicationWindow) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt};

    let Some(surface) = window.surface() else {
        return;
    };
    let Ok(surface) = surface.downcast::<gdk4_x11::X11Surface>() else {
        return;
    };
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let root = conn.setup().roots[screen_num].root;
    let Some(pointer) = conn
        .query_pointer(root)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return;
    };
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let monitors = display.monitors();
    let target = (0..monitors.n_items())
        .filter_map(|i| monitors.item(i)?.downcast::<gtk::gdk::Monitor>().ok())
        .find(|m| {
            let g = m.geometry();
            let scale = m.scale_factor();
            let (px, py) = (i32::from(pointer.root_x), i32::from(pointer.root_y));
            px >= g.x() * scale
                && px < (g.x() + g.width()) * scale
                && py >= g.y() * scale
                && py < (g.y() + g.height()) * scale
        });
    let Some(monitor) = target else { return };
    let g = monitor.geometry();
    let scale = monitor.scale_factor();
    let x = (g.x() + (g.width() - window.default_width()) / 2) * scale;
    let y = (g.y() + (g.height() - window.default_height()) / 2) * scale;
    let _ = conn.configure_window(surface.xid() as u32, &ConfigureWindowAux::new().x(x).y(y));
    let _ = conn.flush();
}

/// Hides the popup and waits until the target window can receive keys again.
pub async fn hide_and_wait_for_target(ctx: &AppCtx) {
    let window = &ctx.window.window;
    if window.is_mapped() {
        let (tx, rx) = async_channel::bounded::<()>(1);
        let id = window.connect_unmap(move |_| {
            let _ = tx.try_send(());
        });
        ctx.window.hide();
        let _ = glib::future_with_timeout(UNMAP_TIMEOUT, rx.recv()).await;
        window.disconnect(id);
    }
    glib::timeout_future(FOCUS_RETURN_DELAY).await;
}

enum Request {
    Toggle,
    Background,
    Preferences,
    Help,
    Unknown(String),
}

fn parse_args(args: &[std::ffi::OsString]) -> Request {
    match args.get(1).map(|a| a.to_string_lossy().into_owned()) {
        None => Request::Toggle,
        Some(a) => match a.as_str() {
            "--toggle" => Request::Toggle,
            "--background" => Request::Background,
            "--preferences" => Request::Preferences,
            "--help" | "-h" => Request::Help,
            other => Request::Unknown(other.to_string()),
        },
    }
}

const USAGE: &str = "Usage: tuck [--toggle | --background | --preferences]\n";

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    let ctx: Rc<RefCell<Option<Rc<AppCtx>>>> = Rc::default();

    let slot = ctx.clone();
    app.connect_startup(move |app| {
        // Stays resident: the window is built once and only hidden.
        std::mem::forget(app.hold());
        *slot.borrow_mut() = Some(build_ctx(app));
    });

    let slot = ctx.clone();
    app.connect_shutdown(move |_| {
        if let Some(ctx) = slot.borrow().as_ref() {
            if let Err(e) = ctx.store.borrow_mut().save() {
                log::error!("cannot save history on exit: {e}");
            }
        }
    });

    // Launching from the app grid (D-Bus activation) opens the popup.
    app.connect_activate(|app| app.activate_action("toggle", None));

    app.connect_command_line(move |app, cmdline| {
        match parse_args(&cmdline.arguments()) {
            Request::Toggle => app.activate_action("toggle", None),
            Request::Preferences => app.activate_action("preferences", None),
            Request::Background => {}
            Request::Help => print!("{USAGE}"),
            Request::Unknown(arg) => {
                eprint!("tuck: unknown option {arg}\n{USAGE}");
                return glib::ExitCode::from(2);
            }
        }
        glib::ExitCode::SUCCESS
    });

    app.run()
}
