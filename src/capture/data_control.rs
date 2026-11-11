//! Wayland data-control watchers (ext-data-control-v1 and the wlroots variant).

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd};
use std::time::{Duration, Instant};

use async_channel::Sender;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};

use wayland_protocols::ext::data_control::v1::client as ext_proto;
use wayland_protocols_wlr::data_control::v1::client as wlr_proto;

use super::{read_capture, CaptureEvent, MAX_PAYLOAD};

/// Time one source may stay silent while we read its data.
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(3);

/// Total time allowed for one flavour.
const READ_TOTAL_TIMEOUT: Duration = Duration::from_secs(20);

/// Lists the interface names advertised by the compositor.
pub(super) fn list_globals() -> Result<Vec<String>, String> {
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let (globals, _queue) = registry_queue_init::<Probe>(&conn).map_err(|e| e.to_string())?;
    let names = globals
        .contents()
        .clone_list()
        .into_iter()
        .map(|g| g.interface)
        .collect();
    Ok(names)
}

struct Probe;

impl Dispatch<WlRegistry, GlobalListContents> for Probe {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Reads one flavour from an offer through a pipe, with idle and total
/// timeouts so a stuck source cannot hang the watcher.
fn read_pipe(
    conn: &Connection,
    receive: impl FnOnce(std::os::fd::BorrowedFd<'_>),
) -> Option<Vec<u8>> {
    let (mut reader, writer) = os_pipe::pipe().ok()?;
    receive(writer.as_fd());
    // Close our copy so EOF arrives when the source finishes.
    drop(writer);
    conn.flush().ok()?;

    let start = Instant::now();
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let left = READ_TOTAL_TIMEOUT.checked_sub(start.elapsed())?;
        let wait = left.min(READ_IDLE_TIMEOUT);
        let mut pfd = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd that lives for the call.
        let n = unsafe { libc::poll(&mut pfd, 1, wait.as_millis() as i32) };
        if n <= 0 {
            return None;
        }
        match reader.read(&mut buf) {
            Ok(0) => return Some(out),
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > MAX_PAYLOAD {
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

/// Generates the watcher for one data-control protocol flavour. The two
/// protocols are identical apart from their type names.
macro_rules! data_control_backend {
    (
        $run:ident, $state:ident,
        manager: $mgr_mod:ident :: $Mgr:ident,
        device: $dev_mod:ident :: $Dev:ident,
        offer: $off_mod:ident :: $Off:ident,
        path: $path:ident
    ) => {
        pub(super) fn $run(tx: &Sender<CaptureEvent>) -> Result<(), String> {
            use $path::$mgr_mod::$Mgr;

            let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
            let (globals, mut queue) =
                registry_queue_init::<$state>(&conn).map_err(|e| e.to_string())?;
            let qh = queue.handle();
            let manager: $Mgr = globals
                .bind(&qh, 1..=1, ())
                .map_err(|e| format!("data-control manager: {e}"))?;
            let seat: WlSeat = globals
                .bind(&qh, 1..=1, ())
                .map_err(|e| format!("seat: {e}"))?;
            let _device = manager.get_data_device(&seat, &qh, ());
            let mut state = $state {
                conn: conn.clone(),
                offers: HashMap::new(),
                tx: tx.clone(),
                closed: false,
            };
            while !state.closed {
                queue
                    .blocking_dispatch(&mut state)
                    .map_err(|e| format!("Wayland connection lost: {e}"))?;
            }
            Ok(())
        }

        struct $state {
            conn: Connection,
            offers: HashMap<wayland_client::backend::ObjectId, Vec<String>>,
            tx: Sender<CaptureEvent>,
            closed: bool,
        }

        impl Dispatch<WlRegistry, GlobalListContents> for $state {
            fn event(
                _: &mut Self,
                _: &WlRegistry,
                _: <WlRegistry as Proxy>::Event,
                _: &GlobalListContents,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<WlSeat, ()> for $state {
            fn event(
                _: &mut Self,
                _: &WlSeat,
                _: <WlSeat as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<$path::$mgr_mod::$Mgr, ()> for $state {
            fn event(
                _: &mut Self,
                _: &$path::$mgr_mod::$Mgr,
                _: <$path::$mgr_mod::$Mgr as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<$path::$off_mod::$Off, ()> for $state {
            fn event(
                state: &mut Self,
                offer: &$path::$off_mod::$Off,
                event: <$path::$off_mod::$Off as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let $path::$off_mod::Event::Offer { mime_type } = event {
                    state.offers.entry(offer.id()).or_default().push(mime_type);
                }
            }
        }

        impl Dispatch<$path::$dev_mod::$Dev, ()> for $state {
            fn event(
                state: &mut Self,
                _: &$path::$dev_mod::$Dev,
                event: <$path::$dev_mod::$Dev as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                use $path::$dev_mod::Event;
                match event {
                    Event::DataOffer { id } => {
                        state.offers.entry(id.id()).or_default();
                    }
                    Event::Selection { id } => {
                        let Some(offer) = id else { return };
                        let mimes = state.offers.remove(&offer.id()).unwrap_or_default();
                        let conn = state.conn.clone();
                        let event = read_capture(&mimes, None, |mime| {
                            read_pipe(&conn, |fd| offer.receive(mime.to_string(), fd))
                        });
                        offer.destroy();
                        if let Some(event) = event {
                            if state.tx.send_blocking(event).is_err() {
                                state.closed = true;
                            }
                        }
                    }
                    Event::Finished => state.closed = true,
                    // The primary selection is not the clipboard.
                    Event::PrimarySelection { id: Some(offer) } => {
                        state.offers.remove(&offer.id());
                        offer.destroy();
                    }
                    _ => {}
                }
            }

            event_created_child!($state, $path::$dev_mod::$Dev, [
                0 => ($path::$off_mod::$Off, ()),
            ]);
        }
    };
}

data_control_backend!(
    run_ext, ExtState,
    manager: ext_data_control_manager_v1 :: ExtDataControlManagerV1,
    device: ext_data_control_device_v1 :: ExtDataControlDeviceV1,
    offer: ext_data_control_offer_v1 :: ExtDataControlOfferV1,
    path: ext_proto
);

data_control_backend!(
    run_wlr, WlrState,
    manager: zwlr_data_control_manager_v1 :: ZwlrDataControlManagerV1,
    device: zwlr_data_control_device_v1 :: ZwlrDataControlDeviceV1,
    offer: zwlr_data_control_offer_v1 :: ZwlrDataControlOfferV1,
    path: wlr_proto
);
