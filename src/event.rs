use std::{
    cell::RefCell,
    sync::mpsc::{Receiver, RecvTimeoutError},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    MountsChanged(String),
    SettingsChanged,
    ManualRun(String),
}

pub trait ReceiveEvent {
    fn recv_timeout(&self, timeout: Option<Duration>) -> Result<Event, RecvTimeoutError>;
}

pub struct EventReceiver(Receiver<Event>);

impl EventReceiver {
    pub fn new(rx: Receiver<Event>) -> Self {
        EventReceiver(rx)
    }
}

impl ReceiveEvent for EventReceiver {
    fn recv_timeout(&self, timeout: Option<Duration>) -> Result<Event, RecvTimeoutError> {
        match timeout {
            Some(t) => self.0.recv_timeout(t),
            None => self.0.recv().map_err(|_| RecvTimeoutError::Disconnected),
        }
    }
}

#[derive(Debug, Default)]
pub struct MockEventReceiver {
    pub results: RefCell<Vec<Result<Event, RecvTimeoutError>>>,
    pub recv_timeout: RefCell<Vec<Option<Duration>>>,
}

impl ReceiveEvent for MockEventReceiver {
    fn recv_timeout(&self, timeout: Option<Duration>) -> Result<Event, RecvTimeoutError> {
        self.recv_timeout.borrow_mut().push(timeout);
        self.results.borrow_mut().remove(0)
    }
}
