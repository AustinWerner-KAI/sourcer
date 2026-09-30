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
                | (Found, Ranked) // the known check runs live on every list
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

/// How much a domain counts. `Must` narrows the search; `Plus` only lifts
/// the ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum DomainWeight {
    Must,
    Plus,
}

/// An area of the business the person should know, e.g. "Digital asset custody".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct BriefDomain {
    pub name: String,
    pub weight: DomainWeight,
}

/// The brief check (SRS F3), as the resourcer edits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct BriefLines {
    /// 1. Seniority titles to search, e.g. "Senior", "Lead", "Principal".
    pub levels: Vec<String>,
    /// Titles never searched, e.g. "Director", "VP".
    pub excluded_titles: Vec<String>,
    /// 2. Up to three, most important first.
    pub must_haves: Vec<String>,
    /// 3. Functional and soft skills. Used to rank and explain, never to filter.
    pub capabilities: Vec<String>,
    /// 4. Areas of the business the person should know.
    pub domains: Vec<BriefDomain>,
    /// 5. Every tool the spec names.
    pub tools: Vec<BriefTool>,
    /// 6. Cities, and whether remote counts.
    pub locations: Vec<String>,
    pub remote: bool,
    /// 7. Kinds of employer to search, and companies to leave out.
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

/// The search step for one role, as the Search screen shows it.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct SearchState {
    /// The confirmed brief searches use, if there is one.
    pub brief_version: Option<i32>,
    /// The confirmed lines that searches use.
    pub lines: Option<BriefLines>,
    /// Edits after confirming are not searched until confirmed.
    pub unconfirmed_edits: bool,
    /// The locations that will each be counted and pulled on their own.
    pub locations: Vec<String>,
    /// Why searching is not possible right now, in words for people.
    pub blocked: Option<String>,
    pub count: Option<CountView>,
    pub pull: Option<PullView>,
    /// Credits used by this organisation since the start of the month.
    #[ts(type = "number")]
    pub credits_this_month: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct CountView {
    pub id: uuid::Uuid,
    pub brief_version: i32,
    /// Seconds since 1970, for "counted 2 minutes ago".
    #[ts(type = "number")]
    pub counted_at: i64,
    /// The brief has been confirmed again since, so this count is out of date.
    pub stale: bool,
    pub locations: Vec<CountLocation>,
    pub credits_used: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct CountLocation {
    pub label: String,
    #[ts(type = "number")]
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct PullView {
    pub id: uuid::Uuid,
    /// The count this pull came from. Each count is pulled from once.
    pub count_id: uuid::Uuid,
    pub requested: i32,
    pub pulled: i32,
    pub new_candidates: i32,
    /// Found again: already a candidate for this role.
    pub already: i32,
    /// Removed after pulling: current job matched a locked-out company.
    pub left_out: i32,
    pub unknown_employer: i32,
    pub credits_used: i32,
    pub locations: i32,
    pub locations_done: i32,
    pub done: bool,
    /// A location could not be pulled after several tries.
    pub failed: bool,
    /// Which locations failed, by name.
    pub failed_locations: Vec<String>,
    /// Credits paid for people who were never saved (a location failed after
    /// the provider charged). Already counted in credits_used.
    pub credits_unsaved: i32,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct CountRequest {
    /// Made fresh by the browser for each press, so a repeat never pays twice.
    pub key: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct PullPick {
    pub location: String,
    pub size: u32,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct PullRequest {
    pub count_id: uuid::Uuid,
    pub picks: Vec<PullPick>,
    /// Required when pulling more than 50 people at once.
    pub confirmed: bool,
    pub key: String,
}

/// The three lists on the Candidates screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum CandidateTab {
    #[default]
    Review,
    Shortlisted,
    Rejected,
}

/// The Candidates screen for one role (SRS F6, F7, F9).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct CandidatesView {
    pub tab: CandidateTab,
    /// The confirmed brief people are ranked against, if there is one.
    pub brief_version: Option<i32>,
    #[ts(type = "number")]
    pub to_review: i64,
    #[ts(type = "number")]
    pub shortlisted: i64,
    #[ts(type = "number")]
    pub rejected: i64,
    /// Found but not ranked yet.
    #[ts(type = "number")]
    pub unranked: i64,
    /// A ranking is queued or running.
    pub ranking: bool,
    /// Why ranking cannot run now, in words for people.
    pub rank_blocked: Option<String>,
    /// Best first. At most `MAX_LISTED` people.
    pub people: Vec<CandidateRow>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct CandidateRow {
    /// The candidacy (this person for this role).
    pub id: uuid::Uuid,
    /// Sent back with a decision, so two people cannot overwrite each other.
    pub version: i32,
    pub state: CandidacyState,
    pub name: String,
    pub title: Option<String>,
    pub employer: Option<String>,
    pub location: Option<String>,
    /// Without the scheme, e.g. "linkedin.com/in/someone".
    pub linkedin_url: Option<String>,
    /// "A", "B" or "C" once ranked.
    pub tier: Option<String>,
    pub score: Option<i32>,
    /// Why, in one or two sentences. Strong evidence is wrapped in **double asterisks**.
    pub reason: Option<String>,
    /// Things to check that the profile does not show.
    pub unknowns: Vec<String>,
    /// Known to the team through another role or a past message.
    pub known: Option<String>,
    /// Opted out or asked to be erased: never contacted.
    pub do_not_contact: bool,
    /// No current employer on record: check before any contact.
    pub employer_unknown: bool,
    pub has_work_email: bool,
    pub has_phone: bool,
    pub reject_reason: Option<ReasonCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum DecisionAction {
    Shortlist,
    Reject,
    /// Move a rejected person back to the list to review.
    Reconsider,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct Decision {
    pub action: DecisionAction,
    /// Required to reject.
    pub reason: Option<ReasonCode>,
    /// The version the screen showed.
    pub version: i32,
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
