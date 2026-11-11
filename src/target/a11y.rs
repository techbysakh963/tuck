//! Terminal detection through the AT-SPI accessibility bus, the only way to
//! see which widget has focus in another window on GNOME Wayland.

use std::collections::VecDeque;
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

pub fn detect() -> TargetKind {
    find_focused_terminal().unwrap_or(TargetKind::Unknown)
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
        if Instant::now() >= self.deadline {
            return None;
        }
        let call = self
            .conn
            .call_method(Some(dest), path, Some(iface), method, &());
        let msg = self.runtime.timeout(CALL_TIMEOUT, call).await.ok()?.ok()?;
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
        let args = (bus_name,);
        let call = self.conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetConnectionUnixProcessID",
            &args,
        );
        let msg = self.runtime.timeout(CALL_TIMEOUT, call).await.ok()?.ok()?;
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

/// `Some(Terminal)` when a focused terminal widget exists in the active
/// window, `Some(Other)` when an active window was found without one, and
/// `None` when the accessibility bus gave no usable answer.
fn find_focused_terminal() -> Option<TargetKind> {
    zruntime::block_on(async {
        let runtime = SharedRuntime::current().ok()?;
        let bus = Bus {
            conn: open_bus(&runtime).await?,
            runtime,
            deadline: Instant::now() + CALL_BUDGET,
        };
        let root: ObjectRef = (
            "org.a11y.atspi.Registry".into(),
            OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root").ok()?,
        );

        let mut saw_active_window = false;
        for app in bus.children(&root).await {
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
                if search_terminal(&bus, frame).await {
                    return Some(TargetKind::Terminal);
                }
            }
        }
        saw_active_window.then_some(TargetKind::Other)
    })
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
