//! Hosting the output on its own thread behind a bounded queue, so a slow
//! Home Assistant never blocks the engine.

use crate::output::{Alert, HaOutput, OutputResult};
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// The queue is full; the alert was dropped. The host records a failed
    /// delivery.
    Full,
    /// The worker has stopped.
    Closed,
}

pub struct Worker {
    tx: Option<SyncSender<Alert>>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    /// Starts the thread. `sink` receives every alert with its result, in
    /// submission order (the host turns it into a `delivery` trace step).
    pub fn spawn(
        output: Arc<HaOutput>,
        capacity: usize,
        sink: impl Fn(Alert, OutputResult) + Send + 'static,
    ) -> Worker {
        let (tx, rx) = sync_channel::<Alert>(capacity.max(1));
        let handle = std::thread::Builder::new()
            .name("ha-output".to_string())
            .spawn(move || {
                while let Ok(alert) = rx.recv() {
                    let result = output.deliver(&alert);
                    sink(alert, result);
                }
            })
            .ok();
        Worker {
            tx: Some(tx),
            handle,
        }
    }

    /// Queues an alert without blocking.
    pub fn submit(&self, alert: Alert) -> Result<(), SubmitError> {
        match self.tx.as_ref().map(|t| t.try_send(alert)) {
            Some(Ok(())) => Ok(()),
            Some(Err(TrySendError::Full(_))) => Err(SubmitError::Full),
            _ => Err(SubmitError::Closed),
        }
    }

    /// Finishes the queued alerts, then stops the thread.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.tx = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}
