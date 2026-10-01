# Design and decisions

- **Requirements (SRS)**: [`SRS.md`](SRS.md). Source of truth for scope, requirements, the project plan and open decisions
- **Product and Solution Design** (living doc): https://claude.ai/code/artifact/a3f61e00-2e2c-457a-b135-7b3387bc7179
- **Workspace UI design** (canvas, four screens): https://claude.ai/artifact/TnSaPrFXBCeqxP7ErBmrTr
- **Playbook and decisions**: NEW SOURCER project, `sourcer-playbook.md` and `sourcer-v2-decisions.md`

## Build process

1. **Plan and architect (waterfall rigour):** requirements, tech stack, and a system architecture covering scalability and security. Output: a Software Requirements Specification and a project plan with clear milestones.
2. **Build in sprints (Agile/Scrum):** two-week sprints that build, test and deliver features incrementally, with feedback each sprint and early validation of the MVP.
3. **Design principles throughout:** SOLID, DRY, KISS and modularity, so the code stays maintainable, testable and scalable.

## Milestones

See the project plan in [`SRS.md`](SRS.md#project-plan). M2 Reach (week 10) is the MVP.

## Done in this scaffold

- Rust server with `/api/health`, config from environment, Postgres pool, migrations run on start
- Schema: 17 tables with `org_id` on every row; one candidacy per person per role; contacts typed (work, personal, phone); do-not-contact list that survives deletion; kill switch on the org
- Send-safety rules (`server/src/policy.rs`): opt-out, do-not-contact, kill switch, sequence approval, reply on any channel, all tested
- Candidate state machine with tests (no contact without approval, no handover without reply, late replies and reconsidered rejects allowed)
- Shared types generated from Rust into TypeScript
- Web app shell in Austin Werner black and gold with the six screens as placeholders
- Docker Compose deploy bound to localhost

## Decisions

- Mail and sign-in: Outlook / Microsoft 365, via Microsoft Graph (29 Sep 2026)
- Email sends from each resourcer's own mailbox only; Apollo is used for contact details, not sending
- Build process: hybrid (plan up front, then two-week sprints) (29 Sep 2026)
- Requirements signed off through `SRS.md`; changes recorded in its change log first
