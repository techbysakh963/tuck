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

/// Longest a whole INCR transfer may take, however steadily chunks arrive.
const INCR_TIMEOUT: Duration = Duration::from_secs(10);

/// Property used as the destination of conversions.
const DEST_PROPERTY: &[u8] = b"TUCK_SELECTION";

/// How many destination properties are used in turn after abandoned transfers.
const DEST_PROPERTIES: u8 = 4;

/// Watches the CLIPBOARD selection until the connection fails or `tx` closes.
pub(super) fn run(tx: &Sender<CaptureEvent>, ready: &dyn Fn()) -> Result<(), String> {
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
    ready();

    let mut watcher = Watcher {
        conn: &conn,
        win,
        atoms,
        fd,
        dirty: false,
        prop_index: 0,
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
    /// Which of the destination properties is in use; see `abandon_transfer`.
    prop_index: u8,
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
        let event = read_capture(&offered, source, |mime| self.read_target(mime));
        // Every conversion names the selection, not an owner, so after a change
        // the offer list and the content (or its password-manager hint) may come
        // from different owners. Drop the mix; the run loop reads again.
        unless_owner_changed(event, self.dirty)
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
        // A stale value left by an abandoned request must not be taken for
        // this answer.
        self.conn.delete_property(self.win, self.atoms.prop).ok()?;
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
                Event::SelectionNotify(e)
                    if is_answer_to(&e, self.win, self.atoms.clipboard, target) =>
                {
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
        let complete_by = Instant::now() + INCR_TIMEOUT;
        loop {
            let Some(event) = self.next_event(incr_wait_deadline(Instant::now(), complete_by))
            else {
                self.abandon_transfer();
                return None;
            };
            match event {
                Event::PropertyNotify(p)
                    if p.window == self.win
                        && p.atom == self.atoms.prop
                        && p.state == x11rb::protocol::xproto::Property::NEW_VALUE =>
                {
                    let Some((_, chunk)) = self.take_property() else {
                        self.abandon_transfer();
                        return None;
                    };
                    if chunk.is_empty() {
                        return Some(data);
                    }
                    if data.len() + chunk.len() > MAX_PAYLOAD {
                        log_oversized();
                        // Drain the rest so the owner is not left blocked.
                        if !self.abort_incr() {
                            self.abandon_transfer();
                        }
                        return None;
                    }
                    data.extend_from_slice(&chunk);
                }
                other => {
                    self.note(&other);
                    // The content is from an owner that no longer holds the
                    // selection; the run loop reads the new one.
                    if self.dirty {
                        self.abandon_transfer();
                        return None;
                    }
                }
            }
        }
    }

    /// Reads and discards the rest of an INCR transfer; false when the owner
    /// did not finish it in time.
    fn abort_incr(&mut self) -> bool {
        let deadline = Instant::now() + TRANSFER_TIMEOUT;
        while let Some(event) = self.next_event(deadline) {
            if let Event::PropertyNotify(p) = event {
                if p.atom == self.atoms.prop
                    && p.state == x11rb::protocol::xproto::Property::NEW_VALUE
                {
                    match self.take_property() {
                        Some((_, chunk)) if !chunk.is_empty() => {}
                        Some(_) => return true,
                        None => return false,
                    }
                }
            }
        }
        false
    }

    /// Moves later conversions to another destination property. An owner left
    /// in the middle of an INCR transfer writes its next chunk as soon as its
    /// property is deleted, which the next request would do; with a different
    /// property that chunk cannot be taken for the new answer.
    fn abandon_transfer(&mut self) {
        let index = (self.prop_index + 1) % DEST_PROPERTIES;
        let atom = self
            .conn
            .intern_atom(false, &dest_property_name(index))
            .ok()
            .and_then(|c| c.reply().ok());
        if let Some(reply) = atom {
            self.prop_index = index;
            self.atoms.prop = reply.atom;
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

/// Whether a `SelectionNotify` answers the request we are waiting on; a late
/// reply to an earlier, timed-out request must be ignored.
fn is_answer_to(
    e: &x11rb::protocol::xproto::SelectionNotifyEvent,
    win: u32,
    selection: Atom,
    target: Atom,
) -> bool {
    e.requestor == win && e.selection == selection && e.target == target
}

/// The destination property for slot `index`; slot 0 is the plain name.
fn dest_property_name(index: u8) -> Vec<u8> {
    if index == 0 {
        DEST_PROPERTY.to_vec()
    } else {
        format!("{}_{index}", String::from_utf8_lossy(DEST_PROPERTY)).into_bytes()
    }
}

/// When to stop waiting for the next INCR chunk: each chunk gets the usual
/// timeout, but never past the end of the whole transfer, so a source that
/// trickles data cannot hold the watcher.
fn incr_wait_deadline(now: Instant, complete_by: Instant) -> Instant {
    (now + TRANSFER_TIMEOUT).min(complete_by)
}

/// A capture is only kept when the selection owner stayed the same while it
/// was read.
fn unless_owner_changed<T>(event: Option<T>, owner_changed: bool) -> Option<T> {
    event.filter(|_| !owner_changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xproto::SelectionNotifyEvent;

    #[test]
    fn incr_transfer_has_an_overall_deadline() {
        let start = Instant::now();
        let complete_by = start + INCR_TIMEOUT;
        assert_eq!(
            incr_wait_deadline(start, complete_by),
            start + TRANSFER_TIMEOUT
        );
        // Late in the transfer the overall deadline wins over a fresh wait.
        let late = complete_by - Duration::from_secs(1);
        assert_eq!(incr_wait_deadline(late, complete_by), complete_by);
        assert_eq!(
            incr_wait_deadline(complete_by + INCR_TIMEOUT, complete_by),
            complete_by
        );
    }

    #[test]
    fn destination_properties_are_distinct() {
        assert_eq!(dest_property_name(0), DEST_PROPERTY);
        let names: std::collections::HashSet<_> =
            (0..DEST_PROPERTIES).map(dest_property_name).collect();
        assert_eq!(names.len(), usize::from(DEST_PROPERTIES));
        assert_eq!(dest_property_name(2), b"TUCK_SELECTION_2");
    }

    #[test]
    fn capture_read_across_an_owner_change_is_dropped() {
        assert_eq!(unless_owner_changed(Some(1), false), Some(1));
        assert_eq!(unless_owner_changed(Some(1), true), None);
        assert_eq!(unless_owner_changed::<i32>(None, true), None);
    }

    fn notify(requestor: u32, selection: Atom, target: Atom) -> SelectionNotifyEvent {
        SelectionNotifyEvent {
            response_type: 31,
            sequence: 0,
            time: 0,
            requestor,
            selection,
            target,
            property: 0,
        }
    }

    #[test]
    fn only_the_pending_request_is_accepted() {
        let e = notify(7, 10, 20);
        assert!(is_answer_to(&e, 7, 10, 20));
        assert!(!is_answer_to(&e, 8, 10, 20));
        assert!(!is_answer_to(&e, 7, 11, 20));
        assert!(!is_answer_to(&e, 7, 10, 21));
    }
}
