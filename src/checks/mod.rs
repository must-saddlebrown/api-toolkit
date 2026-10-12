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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A mutant that hides the stop makes other tests spin forever, so end the test process.
    fn expect_stop(got: Result<Stop, tokio::time::error::Elapsed>, want: Stop) {
        if got != Ok(want) {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("expect_stop: expected {want:?}, got {got:?}\n"),
            );
            std::process::exit(101);
        }
    }

    #[test]
    fn current_stop_sees_the_signal() {
        let (tx, rx) = watch::channel(None);
        assert_eq!(current_stop(&rx), None);
        tx.send(Some(Stop::Cancelled)).unwrap();
        assert_eq!(current_stop(&rx), Some(Stop::Cancelled));
        tx.send(Some(Stop::DeadlineExceeded)).unwrap();
        assert_eq!(current_stop(&rx), Some(Stop::DeadlineExceeded));
    }

    #[tokio::test(start_paused = true)]
    async fn wait_stop_returns_each_stop_without_hanging() {
        for stop in [Stop::Cancelled, Stop::DeadlineExceeded] {
            // Already signalled before waiting.
            let (tx, mut rx) = watch::channel(Some(stop));
            let got = tokio::time::timeout(Duration::from_secs(1), wait_stop(&mut rx)).await;
            expect_stop(got, stop);
            drop(tx);
            // Signalled while waiting.
            let (tx, mut rx) = watch::channel(None);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                tx.send(Some(stop)).unwrap();
            });
            let got = tokio::time::timeout(Duration::from_secs(1), wait_stop(&mut rx)).await;
            expect_stop(got, stop);
        }
    }
}
