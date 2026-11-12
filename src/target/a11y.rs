//! Terminal detection through the AT-SPI accessibility bus, the only way to
//! see which widget has focus in another window on GNOME Wayland.

use std::collections::VecDeque;
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, Instant};

use zbus::OwnedObjectPath;
use zbus::{connection, Connection};
use zruntime::SharedRuntime;

use super::TargetKind;

const ROLE_TERMINAL: u32 = 60;
const STATE_ACTIVE: u32 = 1;
const STATE_FOCUSED: u32 = 12;

/// Nodes visited per window before giving up.
const NODE_BUDGET: usize = 600;
/// Deepest level searched; terminals sit about 16 levels down in Ptyxis.
const MAX_DEPTH: usize = 32;
/// Longest a single bus call may take.
const CALL_TIMEOUT: Duration = Duration::from_millis(150);
/// Stop issuing calls past this point; a stuck peer is not waited for.
const CALL_BUDGET: Duration = Duration::from_millis(280);

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";

type ObjectRef = (String, OwnedObjectPath);

/// True when `bit` is set in the AT-SPI state set (an array of 32-bit words).
pub fn has_state(words: &[u32], bit: u32) -> bool {
    words
        .get((bit / 32) as usize)
        .is_some_and(|w| w & (1 << (bit % 32)) != 0)
}

/// How long the next bus call may take: `CALL_TIMEOUT`, cut short by the
/// request's deadline; `None` once that has passed.
fn call_window(now: Instant, deadline: Instant) -> Option<Duration> {
    let left = deadline.checked_duration_since(now)?;
    (!left.is_zero()).then(|| left.min(CALL_TIMEOUT))
}

/// Where detection gave up; logged with how long it had taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    OpenBus,
    NoActiveWindow,
    /// The request's time ran out; says nothing about the bus.
    OutOfTime,
}

struct Request {
    reply: mpsc::Sender<Result<TargetKind, Stage>>,
    /// Past this the caller has stopped waiting.
    expires: Instant,
}

static WORKER: OnceLock<async_channel::Sender<Request>> = OnceLock::new();

/// Starts the thread that owns the accessibility-bus connection. It connects
/// right away, so the first popup opening only pays for the tree walk, and it
/// connects again after a failure. Idle, it sleeps on its channel.
pub fn start() {
    sender();
}

fn sender() -> &'static async_channel::Sender<Request> {
    WORKER.get_or_init(|| {
        let (tx, rx) = async_channel::unbounded::<Request>();
        let spawned = std::thread::Builder::new()
            .name("tuck-a11y".into())
            .spawn(move || zruntime::block_on(worker(rx)));
        if let Err(e) = spawned {
            log::warn!("cannot start the accessibility thread: {e}");
        }
        tx
    })
}

async fn worker(rx: async_channel::Receiver<Request>) {
    let Ok(runtime) = SharedRuntime::current() else {
        return;
    };
    let mut conn = connect(&runtime).await;
    while let Ok(request) = rx.recv().await {
        let started = Instant::now();
        if conn.is_none() {
            conn = connect(&runtime).await;
        }
        // Requests queued behind a slow one, or behind reconnecting, are
        // answered by dropping them: their callers have moved on.
        if call_window(Instant::now(), request.expires).is_none() {
            log::info!("target detection request expired before it was handled");
            continue;
        }
        let result = match &conn {
            Some(c) => {
                let bus = Bus {
                    conn: c.clone(),
                    runtime: runtime.clone(),
                    deadline: request.expires,
                };
                let result = walk(&bus).await;
                if matches!(result, Err(Stage::OpenBus)) {
                    // The connection stopped answering; open a new one next time.
                    conn = None;
                }
                result
            }
            None => Err(Stage::OpenBus),
        };
        log_outcome(&result, started);
        let _ = request.reply.send(result);
    }
}

async fn connect(runtime: &SharedRuntime) -> Option<Connection> {
    let started = Instant::now();
    let conn = open_bus(runtime).await;
    log::info!(
        "accessibility bus {} in {} ms",
        if conn.is_some() {
            "connected"
        } else {
            "unavailable"
        },
        started.elapsed().as_millis()
    );
    conn
}

fn log_outcome(result: &Result<TargetKind, Stage>, started: Instant) {
    match result {
        Ok(kind) => log::info!(
            "target detection: {kind:?} in {} ms",
            started.elapsed().as_millis()
        ),
        Err(stage) => log::info!(
            "target detection failed at {stage:?} after {} ms",
            started.elapsed().as_millis()
        ),
    }
}

pub fn detect() -> TargetKind {
    let (reply, answer) = mpsc::channel();
    let expires = Instant::now() + CALL_BUDGET;
    if sender().try_send(Request { reply, expires }).is_err() {
        return TargetKind::Unknown;
    }
    // A call may overrun the budget by a moment; reconnecting to the bus
    // first may take longer still, and then the answer is not waited for.
    answer
        .recv_timeout(CALL_BUDGET + CALL_TIMEOUT)
        .ok()
        .and_then(Result::ok)
        .unwrap_or(TargetKind::Unknown)
}

struct Bus {
    conn: Connection,
    runtime: SharedRuntime,
    deadline: Instant,
}

impl Bus {
    /// One bus call, abandoned after `CALL_TIMEOUT` or once the overall
    /// budget is spent.
    async fn call<T>(&self, dest: &str, path: &str, iface: &str, method: &str) -> Option<T>
    where
        T: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        let limit = call_window(Instant::now(), self.deadline)?;
        let call = self
            .conn
            .call_method(Some(dest), path, Some(iface), method, &());
        let msg = self.runtime.timeout(limit, call).await.ok()?.ok()?;
        msg.body().deserialize::<T>().ok()
    }

    async fn accessible<T>(&self, node: &ObjectRef, method: &str) -> Option<T>
    where
        T: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        self.call(&node.0, node.1.as_str(), ACCESSIBLE, method)
            .await
    }

    async fn children(&self, node: &ObjectRef) -> Vec<ObjectRef> {
        self.accessible::<Vec<ObjectRef>>(node, "GetChildren")
            .await
            .unwrap_or_default()
    }

    async fn role(&self, node: &ObjectRef) -> Option<u32> {
        self.accessible::<u32>(node, "GetRole").await
    }

    async fn state(&self, node: &ObjectRef) -> Option<Vec<u32>> {
        self.accessible::<Vec<u32>>(node, "GetState").await
    }

    async fn process_name(&self, bus_name: &str) -> Option<String> {
        let limit = call_window(Instant::now(), self.deadline)?;
        let args = (bus_name,);
        let call = self.conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetConnectionUnixProcessID",
            &args,
        );
        let msg = self.runtime.timeout(limit, call).await.ok()?.ok()?;
        let pid: u32 = msg.body().deserialize().ok()?;
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        Some(comm.trim().to_string())
    }
}

async fn open_bus(runtime: &SharedRuntime) -> Option<Connection> {
    let session = runtime
        .timeout(CALL_TIMEOUT, Connection::session())
        .await
        .ok()?
        .ok()?;
    let call = session.call_method(
        Some("org.a11y.Bus"),
        "/org/a11y/bus",
        Some("org.a11y.Bus"),
        "GetAddress",
        &(),
    );
    let reply = runtime.timeout(CALL_TIMEOUT, call).await.ok()?.ok()?;
    let address: String = reply.body().deserialize().ok()?;
    let builder = connection::Builder::address(address.as_str());
    runtime
        .timeout(CALL_TIMEOUT, builder.build())
        .await
        .ok()?
        .ok()
}

/// `Terminal` when a focused terminal widget exists in the active window,
/// `Other` when an active window was found without one, and the failed stage
/// when the accessibility bus gave no usable answer.
async fn walk(bus: &Bus) -> Result<TargetKind, Stage> {
    let root: ObjectRef = (
        "org.a11y.atspi.Registry".into(),
        OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root").map_err(|_| Stage::OpenBus)?,
    );
    let apps = bus.children(&root).await;
    if apps.is_empty() {
        // Only a bus that failed with time left is reopened.
        if Instant::now() >= bus.deadline {
            return Err(Stage::OutOfTime);
        }
        return Err(Stage::OpenBus);
    }
    let mut saw_active_window = false;
    for app in apps {
        if Instant::now() >= bus.deadline {
            break;
        }
        // gnome-shell answers slowly and never hosts the paste target.
        if bus.process_name(&app.0).await.as_deref() == Some("gnome-shell") {
            continue;
        }
        for frame in bus.children(&app).await {
            let active = bus
                .state(&frame)
                .await
                .is_some_and(|s| has_state(&s, STATE_ACTIVE));
            if !active {
                continue;
            }
            saw_active_window = true;
            if search_terminal(bus, frame).await {
                return Ok(TargetKind::Terminal);
            }
        }
    }
    if saw_active_window {
        Ok(TargetKind::Other)
    } else if Instant::now() >= bus.deadline {
        Err(Stage::OutOfTime)
    } else {
        Err(Stage::NoActiveWindow)
    }
}

/// Breadth-first search below a window for a focused TERMINAL widget.
async fn search_terminal(bus: &Bus, frame: ObjectRef) -> bool {
    let mut queue = VecDeque::from([(frame, 0usize)]);
    let mut visited = 0;
    while let Some((node, depth)) = queue.pop_front() {
        if visited >= NODE_BUDGET || Instant::now() >= bus.deadline {
            return false;
        }
        visited += 1;
        if bus.role(&node).await == Some(ROLE_TERMINAL)
            && bus
                .state(&node)
                .await
                .is_some_and(|s| has_state(&s, STATE_FOCUSED))
        {
            return true;
        }
        if depth < MAX_DEPTH {
            for child in bus.children(&node).await {
                queue.push_back((child, depth + 1));
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_stop_at_the_request_deadline() {
        let now = Instant::now();
        assert_eq!(call_window(now, now + CALL_BUDGET), Some(CALL_TIMEOUT));
        let near = Duration::from_millis(20);
        assert_eq!(call_window(now, now + near), Some(near));
        assert_eq!(call_window(now, now), None);
        assert_eq!(call_window(now + near, now), None);
    }

    #[test]
    fn state_bitfield() {
        let words = [1 << 1 | 1 << 12, 0];
        assert!(has_state(&words, STATE_ACTIVE));
        assert!(has_state(&words, STATE_FOCUSED));
        assert!(!has_state(&words, 2));
        // State 34 lives in the second word.
        assert!(has_state(&[0, 1 << 2], 34));
        assert!(!has_state(&[1 << 2, 0], 34));
        assert!(!has_state(&[], 1));
    }
}
