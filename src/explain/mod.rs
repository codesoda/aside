//! On-demand, read-only explanations. No analysis scheduler, durable drafts or review mutations.
pub mod context;
pub mod providers;
#[cfg(test)]
mod tests;

use crate::config::AiConfig;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::watch;

pub type Result<T> = std::result::Result<T, ExplainError>;
#[derive(Debug, PartialEq, Eq)]
pub enum ExplainError {
    Invalid(&'static str),
    Stale,
    Busy,
    Cancelled,
    Timeout,
    Provider(&'static str),
}

struct Request {
    cancel: watch::Sender<bool>,
    created: Instant,
    running: bool,
}

#[derive(Default)]
pub struct Explanations {
    pub config: AiConfig,
    requests: Mutex<HashMap<String, Request>>,
}

pub struct Running<'a> {
    owner: &'a Explanations,
    id: String,
    cancel: watch::Receiver<bool>,
}
impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.owner
            .requests
            .lock()
            .expect("explanation lock")
            .remove(&self.id);
    }
}

impl Explanations {
    pub fn new(config: AiConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    fn valid_id(id: &str) -> Result<()> {
        if !(8..=80).contains(&id.len())
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(ExplainError::Invalid("Invalid explanation request ID."));
        }
        Ok(())
    }

    pub fn start(&self, id: &str) -> Result<Running<'_>> {
        Self::valid_id(id)?;
        let mut requests = self.requests.lock().expect("explanation lock");
        requests.retain(|_, r| r.running || r.created.elapsed() < Duration::from_secs(120));
        if requests.get(id).is_some_and(|r| *r.cancel.borrow()) {
            return Err(ExplainError::Cancelled);
        }
        // One active generation per review. Never hold a review/store lock across awaits.
        if requests.values().any(|r| r.running) {
            return Err(ExplainError::Busy);
        }
        let (cancel, receiver) = watch::channel(false);
        requests.insert(
            id.to_owned(),
            Request {
                cancel,
                created: Instant::now(),
                running: true,
            },
        );
        Ok(Running {
            owner: self,
            id: id.to_owned(),
            cancel: receiver,
        })
    }

    pub fn cancel(&self, id: &str) -> Result<()> {
        Self::valid_id(id)?;
        let mut requests = self.requests.lock().expect("explanation lock");
        requests.retain(|_, r| r.running || r.created.elapsed() < Duration::from_secs(120));
        if let Some(request) = requests.get(id) {
            request.cancel.send_replace(true);
        } else {
            // Remember cancellation before POST arrival; do not leak an orphaned request.
            if requests.len() >= 128 {
                return Err(ExplainError::Busy);
            }
            let (cancel, _) = watch::channel(true);
            requests.insert(
                id.to_owned(),
                Request {
                    cancel,
                    created: Instant::now(),
                    running: false,
                },
            );
        }
        Ok(())
    }
}

impl Running<'_> {
    pub async fn run<T>(
        &mut self,
        operation: impl std::future::Future<Output = Result<T>>,
        limit: Duration,
    ) -> Result<T> {
        if *self.cancel.borrow() {
            return Err(ExplainError::Cancelled);
        }
        tokio::select! {
            biased;
            _ = self.cancel.changed() => Err(ExplainError::Cancelled),
            result = tokio::time::timeout(limit, operation) => result.unwrap_or(Err(ExplainError::Timeout)),
        }
        // Dropping the operation aborts HTTP IO or kills the tool-free CLI child.
    }
}
