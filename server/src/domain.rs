//! Shared types. Structs marked `#[ts(export)]` generate TypeScript types into
//! `web/src/api/types/` when `cargo test` runs, so the frontend cannot drift.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Where a candidate is for one role. Mirrors the state flow in the design doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, sqlx::Type)]
#[sqlx(type_name = "candidacy_state", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum CandidacyState {
    Found,
    KnownChecked,
    Ranked,
    Shortlisted,
    Rejected,
    Drafted,
    Approved,
    Contacted,
    Replied,
    NoReply,
    HandedToAts,
}

impl CandidacyState {
    /// Allowed transitions. Anything else is refused by the API.
    pub fn can_move_to(self, next: CandidacyState) -> bool {
        use CandidacyState::*;
        matches!(
            (self, next),
            (Found, KnownChecked)
                | (KnownChecked, Ranked)
                | (Ranked, Shortlisted)
                | (Ranked, Rejected)
                | (Shortlisted, Rejected)
                | (Shortlisted, Drafted)
                | (Drafted, Approved)
                | (Approved, Contacted)
                | (Rejected, Ranked) // resourcer changes their mind
                | (Contacted, Replied)
                | (Contacted, NoReply)
                | (NoReply, Replied) // a late reply still counts
                | (Replied, HandedToAts)
        )
    }
}

/// Why a resourcer rejected a candidate. Feeds the playbook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, sqlx::Type)]
#[sqlx(type_name = "reason_code", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum ReasonCode {
    Fit,
    Senior,
    Junior,
    Function,
    Skill,
    Location,
    Employer,
    Known,
}

/// Outreach channel. Email is automated; the others are assisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, sqlx::Type)]
#[sqlx(type_name = "channel", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum Channel {
    Email,
    Linkedin,
    Whatsapp,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct Health {
    pub status: String,
    pub version: String,
    pub database: bool,
}

/// The signed-in user, as the web app sees them.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct Me {
    pub id: uuid::Uuid,
    pub name: String,
    pub email: String,
    /// "admin" or "resourcer".
    pub role: String,
}

#[cfg(test)]
mod tests {
    use super::CandidacyState::*;

    #[test]
    fn happy_path_is_allowed() {
        let path = [
            Found,
            KnownChecked,
            Ranked,
            Shortlisted,
            Drafted,
            Approved,
            Contacted,
            Replied,
            HandedToAts,
        ];
        for w in path.windows(2) {
            assert!(
                w[0].can_move_to(w[1]),
                "{:?} -> {:?} should be allowed",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn cannot_contact_without_approval() {
        assert!(!Drafted.can_move_to(Contacted));
        assert!(!Shortlisted.can_move_to(Contacted));
        assert!(!Ranked.can_move_to(Drafted));
    }

    #[test]
    fn cannot_hand_over_without_reply() {
        assert!(!Contacted.can_move_to(HandedToAts));
    }
}
