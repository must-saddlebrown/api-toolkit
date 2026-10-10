//! Check implementations (filled in P1-P3).

use tokio::sync::watch;

use crate::report::ReasonCode;

pub mod brute_force;
pub mod load;
pub mod race;

/// Why a check was told to stop early. P4 triggers it (Ctrl-C or global deadline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Cancelled,
    DeadlineExceeded,
}

impl Stop {
    /// The reason code a stopped check reports.
    pub fn reason(self) -> ReasonCode {
        match self {
            Stop::Cancelled => ReasonCode::Cancelled,
            Stop::DeadlineExceeded => ReasonCode::DeadlineExceeded,
        }
    }
}

/// The stop signal now, if any. `Stop` is `Copy`, so the borrow ends right away.
pub(crate) fn current_stop(rx: &watch::Receiver<Option<Stop>>) -> Option<Stop> {
    *rx.borrow()
}

/// Wait until a stop is signalled. If the sender is dropped, no stop can ever come,
/// so wait forever (the other `select!` branch then decides).
pub(crate) async fn wait_stop(rx: &mut watch::Receiver<Option<Stop>>) -> Stop {
    loop {
        if let Some(s) = current_stop(rx) {
            return s;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}
