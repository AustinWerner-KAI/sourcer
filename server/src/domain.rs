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

/// What a user may do. Admins also manage the team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, sqlx::Type)]
#[sqlx(type_name = "user_role", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum Role {
    Admin,
    Resourcer,
}

/// Where a team member is: invited but never signed in, active, or switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum MemberStatus {
    Invited,
    Active,
    Disabled,
}

/// One person on the team, as the Team screen shows them.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct TeamMember {
    pub id: uuid::Uuid,
    pub name: String,
    pub email: String,
    pub role: Role,
    pub status: MemberStatus,
}

/// An admin invites someone by their Microsoft 365 email address.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct NewMember {
    pub name: String,
    pub email: String,
    pub role: Role,
}

/// Confirm exactly these lines. `based_on` is the version the editor loaded
/// (`None` if there was no brief), so a stale window cannot overwrite newer work.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct ConfirmBrief {
    pub lines: BriefLines,
    pub based_on: Option<i32>,
}

/// Switch a team member off or back on.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct MemberUpdate {
    pub disabled: bool,
}

/// How the client treats a tool named in the spec. Never assumed: the
/// resourcer answers for each tool before a search runs (SRS F4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum ToolStatus {
    Required,
    Nice,
    Replacing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct BriefTool {
    pub name: String,
    /// `None` until the resourcer answers.
    pub status: Option<ToolStatus>,
}

/// The five-line check (SRS F3), as the resourcer edits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct BriefLines {
    /// 1. Seniority titles to search, e.g. "Senior", "Lead", "Principal".
    pub levels: Vec<String>,
    /// Titles never searched, e.g. "Director", "VP".
    pub excluded_titles: Vec<String>,
    /// 2. Up to three, most important first.
    pub must_haves: Vec<String>,
    /// 3. Every tool the spec names.
    pub tools: Vec<BriefTool>,
    /// 4. Cities, and whether remote counts.
    pub locations: Vec<String>,
    pub remote: bool,
    /// 5. Kinds of employer to search, and companies to leave out.
    pub employer_types: Vec<String>,
    pub leave_out: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct Brief {
    pub id: uuid::Uuid,
    pub version: i32,
    pub lines: BriefLines,
    pub drafted_by_ai: bool,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct Client {
    pub id: uuid::Uuid,
    pub name: String,
    pub domain: Option<String>,
    /// Never approach their staff for any role.
    pub off_limits: bool,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct NewClient {
    pub name: String,
    /// The client's web domain, e.g. "example.com". Used to keep their staff out.
    pub domain: String,
    pub off_limits: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct RoleSummary {
    pub id: uuid::Uuid,
    pub title: String,
    pub client_name: Option<String>,
    /// "none", "draft" or "confirmed".
    pub brief_state: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct RoleDetail {
    pub id: uuid::Uuid,
    pub title: String,
    pub client: Option<Client>,
    pub spec_text: String,
    /// The latest version: the open draft if there is one, else the last confirmed.
    pub brief: Option<Brief>,
    /// Companies whose staff are always left out of this role: the hiring
    /// client and every off-limits client. Cannot be removed.
    pub locked_out: Vec<LockedOut>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct LockedOut {
    pub id: uuid::Uuid,
    pub name: String,
    /// The client this role is for, rather than an off-limits client.
    pub hiring: bool,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct NewRole {
    pub client_id: uuid::Uuid,
    pub title: String,
    pub spec_text: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct RoleUpdate {
    pub title: String,
    pub spec_text: String,
    /// Set the client, only while the role has none. `None` leaves it as it is.
    pub client_id: Option<uuid::Uuid>,
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
