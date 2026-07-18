use crate::protocol::NuntiusEvent;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct PublishedEvent {
    pub cursor: i64,
    pub user_id: String,
    pub event: NuntiusEvent,
}

#[derive(Clone)]
pub struct EventHub {
    sender: broadcast::Sender<PublishedEvent>,
}

impl EventHub {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<PublishedEvent> {
        self.sender.subscribe()
    }
    pub fn publish(&self, event: PublishedEvent) {
        let _ = self.sender.send(event);
    }
}
