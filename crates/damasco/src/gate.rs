//! One assistant at work at a time, across every site: the slot.
//!
//! The assistant's CLI runs for one site at a time, a turn of the chat or
//! the terminal. A chat takes the slot before starting and holds it, through
//! a `SlotGuard`, until it ends; another site that tries meanwhile learns who
//! holds it. The hooks and the egress approvals of the running CLI go to the
//! site that holds the slot.
//!
//! The same site may take the slot again while it holds it (a turn and the
//! files it then builds, or a retry): its own chat already keeps a turn and
//! the terminal apart.

use std::sync::{Arc, Mutex};

use serde::Serialize;

/// What occupies the assistant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Turn,
    Terminal,
}

/// Who holds the slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Occupant {
    pub site_id: String,
    pub kind: Kind,
}

#[derive(Default)]
struct Held {
    occupant: Option<Occupant>,
    /// Guards of the occupant's site still alive.
    count: usize,
}

#[derive(Default)]
pub struct AgentSlot {
    held: Mutex<Held>,
    /// The occupant after each change, for the panel.
    changes: Option<tokio::sync::watch::Sender<Option<Occupant>>>,
    /// True once the server is shutting down: the panel's streams end.
    closing: Option<tokio::sync::watch::Sender<bool>>,
}

impl AgentSlot {
    pub fn new() -> Arc<Self> {
        let (changes, _) = tokio::sync::watch::channel(None);
        let (closing, _) = tokio::sync::watch::channel(false);
        Arc::new(Self {
            held: Mutex::new(Held::default()),
            changes: Some(changes),
            closing: Some(closing),
        })
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tell(&self, occupant: Option<Occupant>) {
        if let Some(changes) = &self.changes {
            changes.send_replace(occupant);
        }
    }

    /// Takes the slot for `site_id`, or says who holds it for another site.
    pub fn try_take(self: &Arc<Self>, site_id: &str, kind: Kind) -> Result<SlotGuard, Occupant> {
        let mut held = self.held();
        match &held.occupant {
            Some(occupant) if occupant.site_id != site_id => return Err(occupant.clone()),
            Some(_) => held.count += 1,
            None => {
                held.occupant = Some(Occupant {
                    site_id: site_id.to_string(),
                    kind,
                });
                held.count = 1;
                self.tell(held.occupant.clone());
            }
        }
        Ok(SlotGuard { slot: self.clone() })
    }

    /// Who holds the slot, if anyone.
    pub fn current(&self) -> Option<Occupant> {
        self.held().occupant.clone()
    }

    /// The occupant after each change.
    pub fn watch(&self) -> Option<tokio::sync::watch::Receiver<Option<Occupant>>> {
        self.changes.as_ref().map(|changes| changes.subscribe())
    }

    /// Ends the panel's streams of changes, when the server shuts down.
    pub fn close(&self) {
        if let Some(closing) = &self.closing {
            closing.send_replace(true);
        }
    }

    /// Resolves once `close` was called.
    pub fn closed(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let receiver = self.closing.as_ref().map(|closing| closing.subscribe());
        async move {
            if let Some(mut receiver) = receiver {
                let _ = receiver.wait_for(|closed| *closed).await;
            }
        }
    }

    fn release(&self) {
        let mut held = self.held();
        held.count = held.count.saturating_sub(1);
        if held.count == 0 && held.occupant.take().is_some() {
            self.tell(None);
        }
    }
}

/// The slot, held until dropped.
pub struct SlotGuard {
    slot: Arc<AgentSlot>,
}

impl std::fmt::Debug for SlotGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SlotGuard")
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.slot.release();
    }
}

/// One build at a time across every site: `npm ci` and `npm run build` take
/// memory and CPU, and builds of several sites must not pile up. Tokio's
/// semaphore serves the waiting builds in the order they arrived.
pub static BUILDS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// The site whose build holds `BUILDS`, for the panel: a publication of
/// another site says it waits for it.
pub fn building() -> &'static tokio::sync::watch::Sender<Option<String>> {
    static BUILDING: std::sync::OnceLock<tokio::sync::watch::Sender<Option<String>>> =
        std::sync::OnceLock::new();
    BUILDING.get_or_init(|| tokio::sync::watch::Sender::new(None))
}

/// Shows the site `id` as building until dropped.
pub struct Building;

impl Building {
    pub fn start(id: String) -> Self {
        building().send_replace(Some(id));
        Self
    }
}

impl Drop for Building {
    fn drop(&mut self) {
        building().send_replace(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_site_learns_who_holds_the_slot() {
        let slot = AgentSlot::new();
        let turn = slot.try_take("A", Kind::Turn).unwrap();
        let refused = slot.try_take("B", Kind::Terminal).unwrap_err();
        assert_eq!(
            refused,
            Occupant {
                site_id: "A".into(),
                kind: Kind::Turn
            }
        );
        drop(turn);
        assert_eq!(slot.current(), None);
        assert!(slot.try_take("B", Kind::Terminal).is_ok());
    }

    #[test]
    fn the_same_site_takes_it_again_and_keeps_it_until_the_last_guard() {
        let slot = AgentSlot::new();
        let first = slot.try_take("A", Kind::Turn).unwrap();
        let second = slot.try_take("A", Kind::Turn).unwrap();
        drop(first);
        assert_eq!(slot.current().map(|o| o.site_id), Some("A".into()));
        drop(second);
        assert_eq!(slot.current(), None);
    }

    #[tokio::test]
    async fn the_panel_hears_each_change() {
        let slot = AgentSlot::new();
        let mut watch = slot.watch().unwrap();
        let guard = slot.try_take("A", Kind::Terminal).unwrap();
        watch.changed().await.unwrap();
        assert_eq!(watch.borrow().clone().map(|o| o.kind), Some(Kind::Terminal));
        drop(guard);
        watch.changed().await.unwrap();
        assert_eq!(*watch.borrow(), None);
    }

    #[tokio::test]
    async fn builds_wait_in_the_order_they_arrive() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let first = BUILDS.acquire().await.unwrap();
        let mut waiting = Vec::new();
        for name in ["B", "C"] {
            let order = order.clone();
            waiting.push(tokio::spawn(async move {
                let _permit = BUILDS.acquire().await.unwrap();
                order.lock().unwrap().push(name);
            }));
            // Let each one queue before the next.
            tokio::task::yield_now().await;
        }
        drop(first);
        for task in waiting {
            task.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), ["B", "C"]);
    }
}
