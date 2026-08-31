//! Interrupts for injecting temporary messages into a running execution.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// The priority of an interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptPriority {
    /// Queued normally, injected at the next Call LLM node.
    Normal,
    /// Injected before the next LLM call (e.g. between tool calls).
    Urgent,
    /// Immediately aborts the current LLM call and forces a message to the LLM.
    Emergency,
}

/// A temporary message injected into a running execution.
#[derive(Debug, Clone)]
pub struct Interrupt {
    /// The priority of the interrupt.
    pub priority: InterruptPriority,
    /// The message content.
    pub message: String,
}

/// A shared interrupt queue with priority-based draining.
///
/// The bus is shared between the gRPC control RPCs (sender side) and the
/// running execution (receiver side). A [`Notify`] allows the execution to
/// wait for an emergency interrupt while an LLM call is in flight.
#[derive(Clone)]
pub struct InterruptBus {
    inner: Arc<Mutex<VecDeque<Interrupt>>>,
    notify: Arc<Notify>,
}

impl InterruptBus {
    /// Creates a new empty interrupt bus.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Queues an interrupt and wakes any waiting receiver.
    pub fn send(&self, interrupt: Interrupt) {
        self.inner.lock().unwrap().push_back(interrupt);
        self.notify.notify_one();
    }

    /// Removes and returns all queued interrupts of the given priority.
    pub fn drain(&self, priority: InterruptPriority) -> Vec<String> {
        let mut guard = self.inner.lock().unwrap();
        let mut out = Vec::new();
        let mut i = 0;
        while i < guard.len() {
            if guard[i].priority == priority {
                if let Some(interrupt) = guard.remove(i) {
                    out.push(interrupt.message);
                }
            } else {
                i += 1;
            }
        }
        out
    }

    /// Removes and returns all queued interrupts.
    pub fn drain_all(&self) -> Vec<String> {
        let mut guard = self.inner.lock().unwrap();
        guard.drain(..).map(|i| i.message).collect()
    }

    /// Waits for an emergency interrupt, returning its message.
    ///
    /// Non-emergency interrupts are left in the queue. This is used to race
    /// against an in-flight LLM call so that an emergency message can abort it.
    pub async fn wait_emergency(&self) -> Option<String> {
        loop {
            let mut msgs = self.drain(InterruptPriority::Emergency);
            if let Some(msg) = msgs.pop() {
                return Some(msg);
            }
            self.notify.notified().await;
        }
    }
}

impl Default for InterruptBus {
    fn default() -> Self {
        Self::new()
    }
}
