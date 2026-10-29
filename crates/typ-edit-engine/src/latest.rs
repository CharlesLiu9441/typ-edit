//! Latest-wins mailbox split into a sender and a receiver.
//!
//! Workers publish intermediate results faster than the UI consumes them; only
//! the newest value matters. The pair shares one `Option<T>` slot (bounded memory
//! of a single value) plus one [`bounded(1)`](crossbeam_channel::bounded) tickle
//! signal. A sender replaces the pending value and rings the bell; the receiver
//! drains the slot on each tickle. A tickle with an empty slot is spurious and is
//! skipped by looping.

use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, RecvError, Sender, bounded};

use crate::sync::lock;

/// Producing end of a latest-wins mailbox.
pub(crate) struct LatestSender<T> {
    slot: Arc<Mutex<Option<T>>>,
    tickle_tx: Sender<()>,
}

/// Consuming end of a latest-wins mailbox.
pub(crate) struct LatestReceiver<T> {
    slot: Arc<Mutex<Option<T>>>,
    tickle_rx: Receiver<()>,
}

/// Creates a connected latest-wins mailbox.
pub(crate) fn latest<T>() -> (LatestSender<T>, LatestReceiver<T>) {
    let slot = Arc::new(Mutex::new(None));
    let (tickle_tx, tickle_rx) = bounded(1);
    (
        LatestSender {
            slot: Arc::clone(&slot),
            tickle_tx,
        },
        LatestReceiver { slot, tickle_rx },
    )
}

impl<T> LatestSender<T> {
    /// Replaces any pending value, then rings the tickle bell. `try_send` is
    /// deliberate: a full channel already holds a tickle and the newest value is
    /// in the slot.
    pub(crate) fn send(&self, value: T) {
        *lock(&self.slot) = Some(value);
        let _ = self.tickle_tx.try_send(());
    }

    /// Merges into the pending value, inserting `T::default()` when the slot is
    /// empty (a merge must never be silently dropped), then rings the bell.
    pub(crate) fn update(&self, merge: impl FnOnce(&mut T))
    where
        T: Default,
    {
        {
            let mut guard = lock(&self.slot);
            merge(guard.get_or_insert_with(T::default));
        }
        let _ = self.tickle_tx.try_send(());
    }
}

impl<T> LatestReceiver<T> {
    /// Blocks on the tickle; returns the newest pending value, skipping spurious
    /// tickles (empty slot) by looping. Returns `Err` when the sender is dropped.
    pub(crate) fn recv(&self) -> Result<T, RecvError> {
        loop {
            self.tickle_rx.recv()?;
            if let Some(value) = self.take() {
                return Ok(value);
            }
        }
    }

    /// Non-blocking take.
    pub(crate) fn take(&self) -> Option<T> {
        lock(&self.slot).take()
    }

    /// For `select!`: wait for a tickle or disconnect. After this returns `Ok`,
    /// call [`take`](Self::take); a `None` take means a spurious tickle and the
    /// caller must loop back to the select.
    pub(crate) fn tickle_rx(&self) -> &Receiver<()> {
        &self.tickle_rx
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn send_replaces_pending_value() {
        let (tx, rx) = latest::<u32>();
        tx.send(1);
        tx.send(2);
        assert_eq!(rx.recv().unwrap(), 2);
    }

    #[test]
    fn update_merges_into_default_when_empty() {
        let (tx, rx) = latest::<Vec<u32>>();
        tx.update(|m| m.push(1));
        assert_eq!(rx.recv().unwrap(), vec![1]);
    }

    #[test]
    fn update_merges_into_pending_value() {
        let (tx, rx) = latest::<Vec<u32>>();
        tx.send(vec![1]);
        tx.update(|m| m.push(2));
        assert_eq!(rx.recv().unwrap(), vec![1, 2]);
    }

    #[test]
    fn spurious_tickle_loops_until_a_value_arrives() {
        let (tx, rx) = latest::<u32>();
        tx.send(1);
        // Consume the value directly, leaving the tickle queued but the slot empty.
        assert_eq!(rx.take(), Some(1));

        let handle = thread::spawn(move || rx.recv());
        tx.send(2);
        assert_eq!(handle.join().unwrap().unwrap(), 2);
    }

    #[test]
    fn dropping_sender_wakes_blocked_recv_with_err() {
        let (tx, rx) = latest::<u32>();
        let handle = thread::spawn(move || rx.recv());
        drop(tx);
        assert!(matches!(handle.join().unwrap(), Err(RecvError)));
    }

    #[test]
    fn newest_of_many_sends_wins_and_is_not_lost() {
        let (tx, rx) = latest::<u32>();
        let producer = thread::spawn(move || {
            for i in 0..1000 {
                tx.send(i);
            }
        });

        let mut last = None;
        while let Ok(value) = rx.recv() {
            last = Some(value);
        }
        producer.join().unwrap();

        assert_eq!(last, Some(999));
        assert_eq!(rx.take(), None, "slot retains at most one value");
    }
}
