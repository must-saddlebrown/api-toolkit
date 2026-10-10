//! Check implementations (filled in P1-P3).

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
