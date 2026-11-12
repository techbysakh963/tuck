//! X11 clipboard watcher driven by XFixes selection events.

use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

use async_channel::Sender;
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME};

use super::{log_oversized, read_capture, CaptureEvent, MAX_PAYLOAD};

/// How long one owner may take to answer a conversion before we give up.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(3);

/// Property used as the destination of conversions.
const DEST_PROPERTY: &[u8] = b"TUCK_SELECTION";

/// Watches the CLIPBOARD selection until the connection fails or `tx` closes.
pub(super) fn run(tx: &Sender<CaptureEvent>) -> Result<(), String> {
    let (conn, screen_num, fd) = connect_with_fd()?;
    let root = conn.setup().roots[screen_num].root;
    let win = conn.generate_id().map_err(|e| e.to_string())?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| e.to_string())?;

    conn.xfixes_query_version(5, 0)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("XFixes unavailable: {e}"))?;

    let atoms = Atoms::new(&conn)?;
    conn.xfixes_select_selection_input(
        win,
        atoms.clipboard,
        SelectionEventMask::SET_SELECTION_OWNER
            | SelectionEventMask::SELECTION_WINDOW_DESTROY
            | SelectionEventMask::SELECTION_CLIENT_CLOSE,
    )
    .map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())?;

    let mut watcher = Watcher {
        conn: &conn,
        win,
        atoms,
        fd,
        dirty: false,
    };
    loop {
        // Blocks in the kernel until the server sends something: no polling.
        let event = conn
            .wait_for_event()
            .map_err(|e| format!("X11 connection lost: {e}"))?;
        if let Event::XfixesSelectionNotify(e) = event {
            if e.selection != watcher.atoms.clipboard {
                continue;
            }
            watcher.dirty = true;
            // The owner may change again while we read; loop until settled.
            while std::mem::take(&mut watcher.dirty) {
                if let Some(ev) = watcher.capture() {
                    if tx.send_blocking(ev).is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    incr: Atom,
    prop: Atom,
    wm_class: Atom,
}

impl Atoms {
    fn new(conn: &RustConnection) -> Result<Self, String> {
        let intern = |name: &[u8]| -> Result<Atom, String> {
            Ok(conn
                .intern_atom(false, name)
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?
                .atom)
        };
        Ok(Self {
            clipboard: intern(b"CLIPBOARD")?,
            targets: intern(b"TARGETS")?,
            incr: intern(b"INCR")?,
            prop: intern(DEST_PROPERTY)?,
            wm_class: intern(b"WM_CLASS")?,
        })
    }
}

struct Watcher<'a> {
    conn: &'a RustConnection,
    win: Window,
    atoms: Atoms,
    fd: RawFd,
    dirty: bool,
}

impl Watcher<'_> {
    fn capture(&mut self) -> Option<CaptureEvent> {
        let owner = self
            .conn
            .get_selection_owner(self.atoms.clipboard)
            .ok()?
            .reply()
            .ok()?
            .owner;
        // Cleared selection, or one we own ourselves: nothing to record.
        if owner == x11rb::NONE || owner == self.win {
            return None;
        }
        let offered = self.offered_targets()?;
        let source = self.owner_class(owner);
        read_capture(&offered, source, |mime| self.read_target(mime))
    }

    fn offered_targets(&mut self) -> Option<Vec<String>> {
        let data = self.convert(self.atoms.targets)?;
        let atoms: Vec<Atom> = data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_ne_bytes(*c))
            .collect();
        let cookies: Vec<_> = atoms
            .iter()
            .filter_map(|&a| self.conn.get_atom_name(a).ok())
            .collect();
        Some(
            cookies
                .into_iter()
                .filter_map(|c| c.reply().ok())
                .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                .collect(),
        )
    }

    fn read_target(&mut self, name: &str) -> Option<Vec<u8>> {
        let atom = self
            .conn
            .intern_atom(false, name.as_bytes())
            .ok()?
            .reply()
            .ok()?
            .atom;
        self.convert(atom)
    }

    /// Asks the owner to convert the selection to `target` and returns the
    /// bytes, following an INCR transfer when the owner uses one.
    fn convert(&mut self, target: Atom) -> Option<Vec<u8>> {
        let deadline = Instant::now() + TRANSFER_TIMEOUT;
        self.conn
            .convert_selection(
                self.win,
                self.atoms.clipboard,
                target,
                self.atoms.prop,
                CURRENT_TIME,
            )
            .ok()?;
        self.conn.flush().ok()?;

        // Wait for the owner's answer.
        loop {
            match self.next_event(deadline)? {
                Event::SelectionNotify(e) if e.requestor == self.win => {
                    if e.property == x11rb::NONE {
                        return None;
                    }
                    break;
                }
                other => self.note(&other),
            }
        }

        let (type_, mut data) = self.take_property()?;
        if type_ != self.atoms.incr {
            return Some(data);
        }

        // INCR: the owner sends chunks; an empty chunk ends the transfer.
        data.clear();
        loop {
            let event = self.next_event(Instant::now() + TRANSFER_TIMEOUT)?;
            match event {
                Event::PropertyNotify(p)
                    if p.window == self.win
                        && p.atom == self.atoms.prop
                        && p.state == x11rb::protocol::xproto::Property::NEW_VALUE =>
                {
                    let (_, chunk) = self.take_property()?;
                    if chunk.is_empty() {
                        return Some(data);
                    }
                    if data.len() + chunk.len() > MAX_PAYLOAD {
                        log_oversized();
                        // Drain the rest so the owner is not left blocked.
                        self.abort_incr();
                        return None;
                    }
                    data.extend_from_slice(&chunk);
                }
                other => self.note(&other),
            }
        }
    }

    fn abort_incr(&mut self) {
        let deadline = Instant::now() + TRANSFER_TIMEOUT;
        while let Some(event) = self.next_event(deadline) {
            if let Event::PropertyNotify(p) = event {
                if p.atom == self.atoms.prop
                    && p.state == x11rb::protocol::xproto::Property::NEW_VALUE
                {
                    match self.take_property() {
                        Some((_, chunk)) if !chunk.is_empty() => {}
                        _ => return,
                    }
                }
            }
        }
    }

    /// Reads and deletes our destination property.
    fn take_property(&self) -> Option<(Atom, Vec<u8>)> {
        let mut out = Vec::new();
        let mut offset = 0u32;
        loop {
            let reply = self
                .conn
                .get_property(
                    true,
                    self.win,
                    self.atoms.prop,
                    AtomEnum::ANY,
                    offset,
                    1 << 20,
                )
                .ok()?
                .reply()
                .ok()?;
            offset += (reply.value.len() / 4) as u32;
            out.extend_from_slice(&reply.value);
            if out.len() > MAX_PAYLOAD {
                log_oversized();
                return None;
            }
            if reply.bytes_after == 0 {
                return Some((reply.type_, out));
            }
            if reply.value.is_empty() {
                // Defensive: avoid spinning if the server stops delivering.
                return None;
            }
        }
    }

    /// Records events that arrive mid-transfer that matter later.
    fn note(&mut self, event: &Event) {
        if let Event::XfixesSelectionNotify(e) = event {
            if e.selection == self.atoms.clipboard {
                self.dirty = true;
            }
        }
    }

    /// Waits for the next event, blocking on the socket until `deadline`.
    fn next_event(&self, deadline: Instant) -> Option<Event> {
        loop {
            if let Some(e) = self.conn.poll_for_event().ok()? {
                return Some(e);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            let mut pfd = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = left.as_millis().min(i32::MAX as u128) as i32;
            // SAFETY: one valid pollfd that lives for the call.
            let n = unsafe { libc::poll(&mut pfd, 1, ms) };
            if n == 0 {
                return None;
            }
        }
    }

    /// WM_CLASS of the selection owner when it is a real client window.
    /// XWayland's bridge window has none, which yields `None`.
    fn owner_class(&self, owner: Window) -> Option<String> {
        let reply = self
            .conn
            .get_property(false, owner, self.atoms.wm_class, AtomEnum::STRING, 0, 64)
            .ok()?
            .reply()
            .ok()?;
        let mut parts = reply
            .value
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned());
        let instance = parts.next()?;
        Some(parts.next().unwrap_or(instance))
    }
}

/// Connects like `RustConnection::connect` but also returns the socket's file
/// descriptor, which lets us wait with a deadline instead of polling.
fn connect_with_fd() -> Result<(RustConnection, usize, RawFd), String> {
    use x11rb::reexports::x11rb_protocol::{parse_display::parse_display, xauth::get_auth};
    use x11rb::rust_connection::DefaultStream;

    let parsed = parse_display(None).map_err(|e| format!("X11 display: {e}"))?;
    let screen = usize::from(parsed.screen);
    let mut last_error = String::from("no usable display address");
    for addr in parsed.connect_instruction() {
        match DefaultStream::connect(&addr) {
            Ok((stream, (family, address))) => {
                let fd = stream.as_raw_fd();
                let (name, data) = get_auth(family, &address, parsed.display)
                    .unwrap_or(None)
                    .unwrap_or_default();
                let conn =
                    RustConnection::connect_to_stream_with_auth_info(stream, screen, name, data)
                        .map_err(|e| format!("X11 connect: {e}"))?;
                return Ok((conn, screen, fd));
            }
            Err(e) => last_error = e.to_string(),
        }
    }
    Err(format!("X11 connect: {last_error}"))
}
