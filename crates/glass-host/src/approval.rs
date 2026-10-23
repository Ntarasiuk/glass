//! "Ask before connecting": a connection waits until the person at the computer allows
//! it, from a prompt on screen, `glass allow`/`glass deny` (say, over SSH), or test hooks.
//! Nobody answering in time means no.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::oneshot;

pub const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub id: u64,
    /// "Safari on iPad · remote guest from 203.0.113.9"
    pub who: String,
    /// Unix seconds.
    pub since: u64,
}

struct Pending {
    request: Request,
    answer: oneshot::Sender<bool>,
}

static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

/// Waits for an answer about `who`; false when denied or unanswered.
pub async fn ask(who: String) -> bool {
    let id = crate::viewers::next_id();
    let (tx, rx) = oneshot::channel();
    PENDING.lock().unwrap().push(Pending {
        request: Request {
            id,
            who: who.clone(),
            since: crate::access::now(),
        },
        answer: tx,
    });
    tracing::info!("asking whether to let in: {who}");
    // The on-screen prompt answers through the same queue as the CLI.
    let body = format!("{who}\nwants to see and control this computer.");
    tokio::spawn(async move {
        if let Some(allow) =
            crate::notify::confirm("Glass", &body, "Allow", "Deny", TIMEOUT).await
        {
            decide(id, allow);
        }
    });
    let allowed = matches!(tokio::time::timeout(TIMEOUT, rx).await, Ok(Ok(true)));
    PENDING.lock().unwrap().retain(|p| p.request.id != id);
    tracing::info!("{who}: {}", if allowed { "allowed" } else { "denied" });
    allowed
}

/// Answers a waiting request; false if there's no such request (any more).
pub fn decide(id: u64, allow: bool) -> bool {
    let mut pending = PENDING.lock().unwrap();
    match pending.iter().position(|p| p.request.id == id) {
        Some(i) => {
            let p = pending.remove(i);
            p.answer.send(allow).is_ok()
        }
        None => false,
    }
}

pub fn pending() -> Vec<Request> {
    PENDING
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.request.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_answer_lets_the_connection_through() {
        crate::testmode::enable();
        let waiting = tokio::spawn(ask("Safari on iPad".into()));
        let id = loop {
            if let Some(r) = pending().into_iter().find(|r| r.who == "Safari on iPad") {
                break r.id;
            }
            tokio::task::yield_now().await;
        };
        assert!(decide(id, true));
        assert!(waiting.await.unwrap());
        assert!(!decide(id, true), "answered once");

        let waiting = tokio::spawn(ask("Chrome on Windows".into()));
        let id = loop {
            if let Some(r) = pending().into_iter().find(|r| r.who == "Chrome on Windows") {
                break r.id;
            }
            tokio::task::yield_now().await;
        };
        assert!(decide(id, false));
        assert!(!waiting.await.unwrap());
    }
}
