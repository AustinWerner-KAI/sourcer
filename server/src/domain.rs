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
                | (Drafted, Rejected)
                | (Approved, Rejected)
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
    /// Their own user in Recruitly, once known, to tell their records from a colleague's.
    pub recruitly_user_id: Option<String>,
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

/// The brief check (SRS F3), as the resourcer edits it. Lines added later
/// default to empty, so a page loaded before an update can still save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct BriefLines {
    /// Claude's short read of the spec: what the search has to find and why.
    /// Shown above the brief; never searched.
    #[serde(default)]
    pub analysis: String,
    /// Line 1: job titles to search, the spec's own and close variants, e.g.
    /// "Cloud Security Engineer". A person's title must contain one.
    #[serde(default)]
    pub titles: Vec<String>,
    /// Seniority words, e.g. "Senior", "Lead", "Principal". The title must
    /// also contain one.
    pub levels: Vec<String>,
    /// Titles never searched, e.g. "Director", "VP".
    pub excluded_titles: Vec<String>,
    /// Fewest years of work experience, if the spec says. Narrows the search.
    #[serde(default)]
    pub min_years: Option<i32>,
    /// 2. Up to three, most important first.
    pub must_haves: Vec<String>,
    /// 3. Functional and soft skills. Used to rank and explain, never to filter.
    pub capabilities: Vec<String>,
    /// 4. Areas of the business the person should know.
    pub domains: Vec<BriefDomain>,
    /// 5. Every tool the spec names.
    pub tools: Vec<BriefTool>,
    /// Line 6: standards and regulations, e.g. "NIST CSF", "DORA". Rank
    /// only, as few profiles list them.
    #[serde(default)]
    pub frameworks: Vec<String>,
    /// Certifications the spec asks for, e.g. "CISSP". Rank only.
    #[serde(default)]
    pub certifications: Vec<String>,
    /// 7. Cities, and whether remote counts.
    pub locations: Vec<String>,
    pub remote: bool,
    /// 8. Kinds of employer to search, and companies to leave out.
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
    /// Closed: no searching, no more emails.
    pub closed: bool,
    /// Email sequences approved and still going, which closing would stop.
    #[ts(type = "number")]
    pub active_sequences: i64,
}

/// POST /api/roles/:id/close
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct CloseRole {
    /// True to close, false to reopen.
    pub closed: bool,
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
    /// The Recruitly job this role came from or is linked to.
    pub recruitly_job: Option<RecruitlyLink>,
    /// Closed: no searching, no more emails.
    pub closed: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct RecruitlyLink {
    pub id: String,
    /// e.g. "Senior IAM Engineer (J-1042)".
    pub label: String,
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
    /// A pull for this role, from any search, is still running.
    pub pulling: bool,
    /// Up to three wider searches of the confirmed brief, by slot.
    pub more: Vec<MoreSearch>,
    /// How many tightenings made the confirmed brief: 0, 1 or 2.
    pub tighten_round: i32,
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
    /// Already pulled from. Each count is pulled from once.
    pub pulled: bool,
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
    /// The wider search this pull came from, or `None` for the brief.
    pub search_name: Option<String>,
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

/// What a wider search adds to the brief. It can only widen: the search is
/// the brief with these applied, so the people found still fit the job.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct Widen {
    #[serde(default)]
    pub add_titles: Vec<String>,
    #[serde(default)]
    pub add_levels: Vec<String>,
    #[serde(default)]
    pub add_locations: Vec<String>,
    /// Only from the employer types Sourcer knows.
    #[serde(default)]
    pub add_employer_types: Vec<String>,
    /// Required tools, by name, that become nice to have.
    #[serde(default)]
    pub tools_to_nice: Vec<String>,
    /// Must domains, by name, that become Plus.
    #[serde(default)]
    pub domains_to_plus: Vec<String>,
    /// Fewer years than the brief asks for; 0 for any. `None` keeps the brief's.
    #[serde(default)]
    pub min_years: Option<i32>,
}

/// One of a role's wider searches, as the Search screen shows it.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct MoreSearch {
    /// 1 and 2 are Claude's picks; 3 is the resourcer's own.
    pub slot: i32,
    pub by_claude: bool,
    pub name: String,
    /// What it adds and why, in one sentence.
    pub note: String,
    pub widen: Widen,
    pub version: i32,
    /// The places it counts and pulls: only those where it differs from the brief.
    pub locations: Vec<String>,
    /// The latest count of this search, if any.
    pub count: Option<MoreCount>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct MoreCount {
    pub id: uuid::Uuid,
    /// Seconds since 1970.
    #[ts(type = "number")]
    pub counted_at: i64,
    /// The search changed since this count.
    pub stale: bool,
    /// People not already found for this role, per place.
    pub locations: Vec<CountLocation>,
    pub credits_used: i32,
    pub pulled: bool,
}

/// PUT /api/roles/:id/searches/:slot
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct SaveSearch {
    pub widen: Widen,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct MorePick {
    pub slot: i32,
    pub count_id: uuid::Uuid,
    /// People to pull from this search, spread over its places.
    pub size: u32,
}

/// POST /api/roles/:id/searches/pull
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct MorePullRequest {
    pub picks: Vec<MorePick>,
    /// Required when pulling more than 50 people at once.
    pub confirmed: bool,
    pub key: String,
}

/// One way a tightening narrows the brief, towards the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum TightenKind {
    /// A tool the spec names as essential becomes required (added if new).
    RequireTool,
    /// An area the spec insists on becomes a Must domain (added if new).
    MustDomain,
    DropTitle,
    DropLevel,
    /// More years, as the spec asks. `value` is the number.
    MinYears,
    DropEmployerType,
    /// An anywhere search becomes these cities, comma separated, all named in the spec.
    SetLocations,
    DropLocation,
}

/// One tightening change, with the words of the spec that justify it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct TightenMove {
    pub kind: TightenKind,
    pub value: String,
    /// Copied from the spec. A change whose quote is not in the spec is dropped.
    pub quote: String,
    /// The change in words, set by the server.
    #[serde(default)]
    pub label: String,
}

/// Claude's tightening of a brief that found far too many people. Nothing
/// is saved until the resourcer agrees.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct TightenView {
    /// The confirmed brief version this tightens.
    pub based_on: i32,
    /// This will be round 1 or 2.
    pub round: i32,
    /// Why so many were found, in plain words.
    pub why: String,
    /// People the count found before tightening.
    #[ts(type = "number")]
    pub before: i64,
    pub moves: Vec<TightenMove>,
}

/// POST /api/roles/:id/search/tighten/apply: the changes the resourcer kept.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct TightenApply {
    pub based_on: i32,
    pub moves: Vec<TightenMove>,
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
    /// Ranked or shortlisted against an older brief; re-ranked automatically.
    #[ts(type = "number")]
    pub stale: i64,
    /// A ranking is queued or running.
    pub ranking: bool,
    /// Why ranking cannot run now, in words for people.
    pub rank_blocked: Option<String>,
    /// Best first. At most `MAX_LISTED` people.
    pub people: Vec<CandidateRow>,
    /// Recruitly is set up: shortlisted people are checked there and can be sent over.
    pub recruitly: bool,
    /// Where people sent to Recruitly land, if the role is linked to a job.
    pub recruitly_job: Option<RecruitlyLink>,
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
    /// Claude's verdict on each must-have, title, level, years, required tool
    /// and Must domain, in the brief's order. Empty when not ranked.
    pub checks: Vec<RankCheck>,
    /// Known to the team through another role or a past message.
    pub known: Option<String>,
    /// Opted out or asked to be erased: never contacted.
    pub do_not_contact: bool,
    /// No current employer on record: check before any contact.
    pub employer_unknown: bool,
    /// The wider search that found them, or `None` for the brief's own.
    pub found_by: Option<String>,
    pub has_work_email: bool,
    pub has_phone: bool,
    /// Work email, personal emails and phones, work first. Empty for anyone
    /// who must not be contacted.
    pub contacts: Vec<ContactLine>,
    /// The latest CV assessment for this role, out of 10.
    pub cv_score: Option<i32>,
    /// The score is against an older brief; a re-rank is due.
    pub stale_rank: bool,
    pub reject_reason: Option<ReasonCode>,
    /// What Recruitly knows about them, in words. `None` when not checked or not there.
    pub recruitly_note: Option<String>,
    /// The Recruitly user who owns their record, to compare with `Me`.
    pub recruitly_owner_id: Option<String>,
    pub recruitly_checked: bool,
    /// The last check could not reach Recruitly.
    pub recruitly_check_failed: bool,
    /// Seconds since 1970 of the last check, for "checked 2 hours ago".
    #[ts(type = "number | null")]
    pub recruitly_checked_at: Option<i64>,
    /// When they were sent to Recruitly, e.g. "30 Sep 2026".
    pub sent_to_recruitly: Option<String>,
    /// Sent into the linked job's pipeline, not only as a candidate.
    pub in_recruitly_pipeline: bool,
    /// Where their emails are: "draft", "approved", "active", "stopped" or
    /// "done", or "reply", "auto" or "bounce" once one arrived. None if never drafted.
    pub email_status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum ContactKind {
    WorkEmail,
    PersonalEmail,
    Phone,
}

impl ContactKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "work_email" => Some(Self::WorkEmail),
            "personal_email" => Some(Self::PersonalEmail),
            "phone" => Some(Self::Phone),
            _ => None,
        }
    }
}

/// One way to reach a person, as the card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct ContactLine {
    pub kind: ContactKind,
    pub value: String,
}

/// How well the work evidence shows one line of the brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum CheckVerdict {
    Met,
    Partly,
    /// The profile does not show it. Not the same as "does not have it".
    NotShown,
}

/// One line of the brief and Claude's verdict on it for one person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct RankCheck {
    pub item: String,
    pub verdict: CheckVerdict,
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

/// A LinkedIn profile the resourcer is viewing, saved to a role (SRS F10).
/// Filled in from the page when they click the Save button, then checked by them.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct SaveProfile {
    pub role_id: uuid::Uuid,
    /// The profile address, e.g. "https://www.linkedin.com/in/someone/".
    pub linkedin_url: String,
    pub name: String,
    pub title: Option<String>,
    pub employer: Option<String>,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct SavedProfile {
    /// False when the person was already on this role.
    pub added: bool,
    pub candidate: CandidateRow,
}

/// Recruitly for the screens. The key itself never leaves the server.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct RecruitlyStatus {
    pub configured: bool,
    #[ts(type = "number")]
    pub calls_today: i64,
    #[ts(type = "number")]
    pub daily_cap: i64,
    /// Whether Recruitly answered the key recently. `None` when not set up or not yet known.
    pub connected: Option<bool>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct RecruitlyTest {
    /// The Recruitly user the key belongs to.
    pub connected_as: String,
}

/// A job in Recruitly, to start a role from.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct RecruitlyJob {
    pub id: String,
    pub title: String,
    pub reference: Option<String>,
    pub company: Option<String>,
    pub status: Option<String>,
    pub location: Option<String>,
    /// The Sourcer role already made from this job.
    pub role_id: Option<uuid::Uuid>,
}

/// A Recruitly job read into the new-role form. Nothing is saved yet.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct JobPreview {
    pub id: String,
    pub label: String,
    pub title: String,
    /// The description as plain text, with location and pay added.
    pub spec_text: String,
    pub company_name: Option<String>,
    pub company_domain: Option<String>,
    /// The Sourcer client that matches the Recruitly company, if any.
    pub client_id: Option<uuid::Uuid>,
    /// The Sourcer role already made from this job.
    pub role_id: Option<uuid::Uuid>,
}

/// Save a role started from a Recruitly job, after the resourcer checked the form.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct ImportRole {
    pub job_id: String,
    pub client_id: uuid::Uuid,
    pub title: String,
    pub spec_text: String,
}

/// Add a shortlisted person to Recruitly. `confirmed` holds the keys of the
/// questions the resourcer has said yes to (see `HandoverResult`).
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct Handover {
    #[serde(default)]
    pub confirmed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct HandoverResult {
    /// Set when done.
    pub candidate: Option<CandidateRow>,
    /// Set instead when the resourcer must say yes first, e.g. a colleague
    /// owns them in Recruitly. Send `confirm_key` back in `confirmed`.
    pub confirm: Option<String>,
    pub confirm_key: Option<String>,
}

/// Link a role to a Recruitly job, or unlink it with `None`.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct LinkJob {
    pub job_id: Option<String>,
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
        // Only stopping the emails moves someone back to the shortlist, so a
        // decision can never leave emails approved for a shortlisted person.
        assert!(!Drafted.can_move_to(Shortlisted));
        assert!(!Approved.can_move_to(Shortlisted));
    }

    #[test]
    fn cannot_hand_over_without_reply() {
        assert!(!Contacted.can_move_to(HandedToAts));
    }
}
