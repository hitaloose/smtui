//! Events produced by background work (PTY readers, file operations) for the UI loop.

use std::any::Any;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// A payload owned by a mode. Every mode sees every event and downcasts the ones it
/// knows, so adding a mode never requires touching this file.
pub struct AppEvent(pub Box<dyn Any + Send>);

impl AppEvent {
    pub fn new<T: Any + Send>(payload: T) -> Self {
        Self(Box::new(payload))
    }

    pub fn get<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }
}

/// Cloneable handle; `send` is synchronous so plain threads can use it too.
#[derive(Clone)]
pub struct EventSender(UnboundedSender<AppEvent>);

impl EventSender {
    pub fn send<T: Any + Send>(&self, payload: T) {
        // The receiver only goes away when the app is shutting down.
        let _ = self.0.send(AppEvent::new(payload));
    }
}

pub fn channel() -> (EventSender, UnboundedReceiver<AppEvent>) {
    let (tx, rx) = unbounded_channel();
    (EventSender(tx), rx)
}
