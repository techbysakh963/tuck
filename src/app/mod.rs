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
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::blobs::blob_name;
use crate::capture::Captured;
use crate::capture::{self, CaptureEvent, CaptureStatus};
use crate::echo::EchoGuard;
use crate::history::HistoryStore;
use crate::history::{prepare_image, PreparedImage};
use crate::ingest::{precheck, store_event, IngestResult};
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
    /// The history item the clipboard holds, when known.
    pub clipboard_holder: Cell<paste_flow::ClipboardHolder>,
    /// Counts clipboard events, so a slow image capture can tell it is stale.
    capture_seq: Cell<u64>,
    /// Bumped whenever the user wipes history or pauses recording, so an image
    /// that was being prepared meanwhile is not added afterwards.
    wipe_generation: Cell<u64>,
    image_jobs: RefCell<ImageQueue<ImageJob>>,
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

    /// Shown right away: brings the popup back with the message on it.
    pub fn show_toast_now(&self, message: &str) {
        self.window.prepare_to_show();
        self.window.present();
        self.window.add_toast(message);
    }

    /// Shows the message on the open popup, or at the next open.
    pub fn notify_now_or_later(&self, message: &str) {
        if self.window.is_visible() {
            self.window.add_toast(message);
        } else {
            self.queue_toast(message);
        }
    }

    /// Drops a queued message that no longer applies and says so when the popup
    /// is open.
    pub fn retract_toast(&self, message: &str) {
        let was_queued = {
            let mut toasts = self.toasts.borrow_mut();
            let before = toasts.len();
            toasts.retain(|t| t != message);
            toasts.len() != before
        };
        if !was_queued {
            self.notify_now_or_later("Clipboard recording resumed");
        }
    }

    /// Shown the next time the popup opens (and logged by the caller).
    pub fn queue_toast(&self, message: &str) {
        self.toasts.borrow_mut().push(message.to_string());
    }
}

/// Whether an image prepared in the background may still be added: not after
/// the history was wiped or recording paused since the copy happened.
fn keep_prepared_image(started_generation: u64, current_generation: u64, paused: bool) -> bool {
    started_generation == current_generation && !paused
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

/// Creates the key sender for this session type.
pub fn make_key_sender(is_x11: bool) -> Result<Box<dyn KeySender>, String> {
    if is_x11 {
        Ok(Box::new(XTestSender::connect()?))
    } else {
        Ok(Box::new(UinputKeyboard::create()?))
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
    crate::logging::install_panic_hook();
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
    let keys: Box<dyn KeySender> = match make_key_sender(is_x11) {
        Ok(k) => k,
        Err(e) => {
            log::error!("no paste backend: {e}");
            toasts.push(if is_x11 {
                "Pasting is unavailable: cannot reach the X server".into()
            } else {
                "Paste needs access to /dev/uinput (see the README)".into()
            });
            Box::new(UnavailableKeys(e))
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
        clipboard_holder: Cell::new(None),
        capture_seq: Cell::new(0),
        wipe_generation: Cell::new(0),
        image_jobs: RefCell::default(),
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

const CAPTURE_STOPPED: &str = "Clipboard recording stopped; retrying";

fn start_capture(ctx: &Rc<AppCtx>) {
    let (tx, rx) = async_channel::unbounded::<CaptureEvent>();
    let (status_tx, status_rx) = async_channel::unbounded::<CaptureStatus>();
    // The watcher thread ends with the process; nothing joins it.
    drop(capture::spawn(tx, status_tx));

    let c = ctx.clone();
    glib::spawn_future_local(async move {
        while let Ok(status) = status_rx.recv().await {
            match status {
                CaptureStatus::Stopped => c.notify_now_or_later(CAPTURE_STOPPED),
                CaptureStatus::Recovered => c.retract_toast(CAPTURE_STOPPED),
            }
        }
    });

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
    let now = now_ms();
    let seq = ctx.capture_seq.get() + 1;
    ctx.capture_seq.set(seq);
    // The echo guard and privacy rules run here, in arrival order, whatever happens next.
    let skipped = precheck(
        &event,
        &ctx.filter.borrow(),
        &mut ctx.echo.borrow_mut(),
        paused,
        now,
    );
    if let Some(reason) = skipped {
        log::debug!("capture skipped: {reason:?}");
        ctx.clipboard_holder.set(paste_flow::holder_after_skip(
            ctx.clipboard_holder.get(),
            reason,
        ));
        return;
    }
    // Until the new content is stored (or fails to be) the old holder is stale.
    ctx.clipboard_holder.set(None);
    match event.captured {
        // Decoding, scaling and writing an image can take long enough to stall the UI.
        Captured::Image { png } => {
            let job = ImageJob {
                png,
                source: event.source,
                now,
                seq,
                generation: ctx.wipe_generation.get(),
            };
            let first = ctx.image_jobs.borrow_mut().submit(job);
            if let Some(job) = first {
                let ctx = ctx.clone();
                glib::spawn_future_local(async move { run_image_jobs(&ctx, job).await });
            }
        }
        captured => {
            let event = CaptureEvent { captured, ..event };
            let result = store_event(event, &mut ctx.store.borrow_mut(), now);
            match result {
                Ok(IngestResult::Stored(outcome)) => {
                    ctx.clipboard_holder.set(Some(outcome.id()));
                    stored(ctx);
                }
                Ok(IngestResult::Skipped(reason)) => {
                    log::debug!("capture skipped: {reason:?}")
                }
                Err(e) => log::warn!("capture failed: {e}"),
            }
        }
    }
}

/// A copied image waiting for, or going through, preparation.
struct ImageJob {
    png: Vec<u8>,
    source: Option<String>,
    now: i64,
    seq: u64,
    generation: u64,
}

/// Prepares one image at a time so a burst of copies cannot start many
/// decoders at once. While one runs only the newest waiting image is kept:
/// an older one would be replaced on the clipboard before anyone could use it.
struct ImageQueue<T> {
    busy: bool,
    waiting: Option<T>,
}

impl<T> Default for ImageQueue<T> {
    fn default() -> Self {
        Self {
            busy: false,
            waiting: None,
        }
    }
}

impl<T> ImageQueue<T> {
    /// Returns the job when it may start now; otherwise it waits, replacing
    /// any older waiting job.
    fn submit(&mut self, job: T) -> Option<T> {
        if self.busy {
            self.waiting = Some(job);
            None
        } else {
            self.busy = true;
            Some(job)
        }
    }

    /// Called when the running job ended; returns the job to start next.
    fn finish(&mut self) -> Option<T> {
        let next = self.waiting.take();
        self.busy = next.is_some();
        next
    }
}

async fn run_image_jobs(ctx: &Rc<AppCtx>, first: ImageJob) {
    let mut job = first;
    loop {
        process_image_job(ctx, job).await;
        match ctx.image_jobs.borrow_mut().finish() {
            Some(next) => job = next,
            None => break,
        }
    }
}

async fn process_image_job(ctx: &Rc<AppCtx>, job: ImageJob) {
    let paused = ctx.settings.borrow().paused;
    if !keep_prepared_image(job.generation, ctx.wipe_generation.get(), paused) {
        return;
    }
    let ImageJob {
        png,
        source,
        now,
        seq,
        generation,
    } = job;
    // Already in the history: nothing to decode or write.
    let known = ctx
        .store
        .borrow_mut()
        .touch_image(&blob_name(&png, "png"), now);
    if let Some(id) = known {
        if ctx.capture_seq.get() == seq {
            ctx.clipboard_holder.set(Some(id));
        }
        stored(ctx);
        return;
    }
    let blobs = ctx.store.borrow().blob_store();
    let started = Instant::now();
    let prepared = gio::spawn_blocking(move || prepare_image(&png, &blobs)).await;
    match prepared {
        Ok(Ok(prepared)) => {
            log::debug!("image prepared in {} ms", started.elapsed().as_millis());
            let paused = ctx.settings.borrow().paused;
            if keep_prepared_image(generation, ctx.wipe_generation.get(), paused) {
                add_prepared(ctx, prepared, source, now, seq);
            } else {
                discard_prepared(ctx, &prepared);
            }
        }
        Ok(Err(e)) => log::warn!("capture failed: {e}"),
        Err(_) => log::error!("image worker panicked"),
    }
}

fn add_prepared(
    ctx: &Rc<AppCtx>,
    prepared: PreparedImage,
    source: Option<String>,
    now: i64,
    seq: u64,
) {
    let outcome = ctx
        .store
        .borrow_mut()
        .add_prepared_image(prepared, source, now);
    // Only the newest capture can be what the clipboard holds now.
    if ctx.capture_seq.get() == seq {
        ctx.clipboard_holder.set(Some(outcome.id()));
    }
    stored(ctx);
}

/// Removes the blob of an image that is not going to be added, unless an
/// existing item shares it.
fn discard_prepared(ctx: &AppCtx, prepared: &PreparedImage) {
    log::debug!("dropped an image copied before the history was wiped or paused");
    let store = ctx.store.borrow();
    if !store.has_image_blob(&prepared.blob) {
        store.blob_store().remove(&prepared.blob);
    }
}

fn stored(ctx: &Rc<AppCtx>) {
    schedule_save(ctx);
    if ctx.window.is_visible() {
        ctx.window.clipboard.refresh(false);
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
        if now_paused {
            c.wipe_generation.set(c.wipe_generation.get() + 1);
        }
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
    let c = ctx.clone();
    pause.connect_state_notify(move |action| {
        let paused = action
            .state()
            .and_then(|s| s.get::<bool>())
            .unwrap_or(false);
        c.window.clipboard.set_paused(paused);
    });
    ctx.window.clipboard.set_paused(paused);
    app.add_action(&pause);

    let clear = gio::SimpleAction::new("clear", None);
    let c = ctx.clone();
    clear.connect_activate(move |_, _| confirm_clear(&c));
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

/// Clearing cannot be undone, so it asks first; pinned items stay.
fn confirm_clear(ctx: &Rc<AppCtx>) {
    let dialog = adw::AlertDialog::builder()
        .heading("Clear history?")
        .body("All items are removed except the ones you pinned. This cannot be undone.")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("clear", "Clear history")]);
    dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let c = ctx.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "clear" {
            return;
        }
        // An image still being prepared must not reappear after this.
        c.wipe_generation.set(c.wipe_generation.get() + 1);
        {
            let mut store = c.store.borrow_mut();
            store.clear_unpinned();
            store.discard_corrupt_backups();
        }
        schedule_save(&c);
        c.window.clipboard.refresh(false);
    });
    ctx.window.present();
    dialog.present(Some(&ctx.window.window));
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

#[derive(Debug, PartialEq, Eq)]
enum Request {
    Toggle,
    Background,
    Preferences,
    Help,
    Version,
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
            "--version" | "-V" => Request::Version,
            other => Request::Unknown(other.to_string()),
        },
    }
}

const USAGE: &str = "Usage: tuck [--toggle | --background | --preferences | --version | --help]\n";

static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_quit_signal(_: libc::c_int) {
    let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = 1u8;
        // Only async-signal-safe work here: one write, result ignored.
        unsafe { libc::write(fd, std::ptr::addr_of!(byte).cast(), 1) };
    }
}

/// SIGTERM (logout, `pkill`) and SIGINT quit normally, so the shutdown handler
/// writes the history instead of losing the last half second of changes. The
/// handler only writes to a pipe; a thread blocked on it wakes the main loop.
fn quit_on_signals(app: &adw::Application) {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        log::warn!("cannot watch for termination signals");
        return;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    SIGNAL_PIPE.store(write_fd, Ordering::Relaxed);
    let (tx, rx) = async_channel::bounded::<()>(1);
    std::thread::spawn(move || {
        let mut byte = 0u8;
        loop {
            let n = unsafe { libc::read(read_fd, std::ptr::addr_of_mut!(byte).cast(), 1) };
            if n > 0 {
                let _ = tx.send_blocking(());
                break;
            }
            if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
    });
    let handler = on_quit_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        if rx.recv().await.is_ok() {
            log::info!("quitting on signal");
            app.quit();
        }
    });
}

pub fn run() -> glib::ExitCode {
    // Answered here, in the calling terminal, even when Tuck is already running
    // (the running instance would otherwise print into its own output).
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    match parse_args(&args) {
        Request::Help => {
            print!("{USAGE}");
            return glib::ExitCode::SUCCESS;
        }
        Request::Version => {
            println!("tuck {}", env!("CARGO_PKG_VERSION"));
            return glib::ExitCode::SUCCESS;
        }
        Request::Unknown(arg) => {
            eprint!("tuck: unknown option {arg}\n{USAGE}");
            return glib::ExitCode::from(2);
        }
        Request::Toggle | Request::Background | Request::Preferences => {}
    }

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
        quit_on_signals(app);
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
            // Rejected in `run` before the application starts.
            Request::Help | Request::Version | Request::Unknown(_) => {}
        }
        glib::ExitCode::SUCCESS
    });

    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_queue_runs_one_and_keeps_only_the_newest_waiting() {
        let mut q = ImageQueue::default();
        assert_eq!(q.submit(1), Some(1));
        assert_eq!(q.submit(2), None);
        assert_eq!(q.submit(3), None);
        assert_eq!(q.finish(), Some(3));
        assert_eq!(q.finish(), None);
        // Idle again: the next image starts at once.
        assert_eq!(q.submit(4), Some(4));
    }

    fn args(list: &[&str]) -> Vec<std::ffi::OsString> {
        list.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn command_line_parsing() {
        assert_eq!(parse_args(&args(&["tuck"])), Request::Toggle);
        assert_eq!(parse_args(&args(&["tuck", "--toggle"])), Request::Toggle);
        assert_eq!(
            parse_args(&args(&["tuck", "--background"])),
            Request::Background
        );
        assert_eq!(
            parse_args(&args(&["tuck", "--preferences"])),
            Request::Preferences
        );
        assert_eq!(parse_args(&args(&["tuck", "-h"])), Request::Help);
        assert_eq!(parse_args(&args(&["tuck", "--help"])), Request::Help);
        assert_eq!(parse_args(&args(&["tuck", "--version"])), Request::Version);
        assert_eq!(
            parse_args(&args(&["tuck", "--bogus"])),
            Request::Unknown("--bogus".into())
        );
    }

    #[test]
    fn image_dropped_after_wipe_or_pause() {
        assert!(keep_prepared_image(3, 3, false));
        assert!(!keep_prepared_image(3, 4, false));
        assert!(!keep_prepared_image(3, 3, true));
    }
}
