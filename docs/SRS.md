# Sourcer SRS

Software Requirements Specification · 29 Sep 2026 · Kai

Living copy (edit here first, then sync this file): https://claude.ai/code/artifact/409c4cc8-0726-43dd-9060-afc51a259432

## Purpose and scope

Sourcer turns a job spec into a ranked, checked shortlist and personal outreach, so Austin Werner resourcers stop relying on slow LinkedIn searching.

- **Problem:** finding and approaching candidates for digital-asset and fintech roles is manual, slow and scattered across LinkedIn, spreadsheets and inboxes.
- **For:** Kai and a small resourcing team first. It may be sold to other agencies later.
- **Scope:** resourcing and outreach only. The ATS stays the system of record; Sourcer hands interested candidates over.
- **Done looks like:** a resourcer confirms a brief, gets a ranked list with reasons, approves outreach, and hands replies to the ATS, all in one place, with zero double contacts.

## Users and roles

Two roles at launch, both signing in with their Microsoft 365 account.

| Role | Who | Can |
| --- | --- | --- |
| Admin | Kai | Everything a resourcer can, plus manage users, clients, the off-limits list, the kill switch and playbook rules |
| Resourcer | Team members | Create roles and briefs, run searches, shortlist or reject, draft and approve outreach, mark replies, hand over to the ATS |

Team size is still to confirm (see open decisions).

## Functional requirements

26 requirements: 20 must-haves for the first release, 6 nice-to-haves. Each maps to a milestone in the project plan.

| ID | Requirement | Priority | Milestone |
| --- | --- | --- | --- |
| F1 | Sign in with Microsoft 365; admin and resourcer roles | Must | M0 |
| F2 | Create a role for a client; upload or paste a job spec | Must | M1 |
| F3 | Turn the spec into a brief (level, must-haves, capabilities, domain focus, tools, locations, employer types); resourcer confirms before any paid search. Capabilities only rank; a Must domain narrows the search, a Plus domain only ranks | Must | M1 |
| F4 | Ask how to treat each named tool (required, nice, being replaced) | Must | M1 |
| F5 | Search People Data Labs from the confirmed brief: count per location first (1 credit each), then pull a chosen number (1 credit each; over 50 needs a second confirmation). A count is pulled once; people already found are never paid for again | Must | M1 |
| F6 | Rank candidates with a reason and a list of unknowns for each. Claude ranks right after each pull, sent work evidence only (titles, employers, dates, location, skills), never names, contact details or profile links | Must | M1 |
| F7 | Known check: flag anyone the team has contacted, placed or blocked. Runs live on every list; known people are flagged, not hidden, and the resourcer decides. Do-not-contact people cannot be shortlisted | Must | M1 |
| F8 | Honour the client off-limits list: never surface their staff | Must | M1 |
| F9 | Shortlist or reject with a reason code (FIT, SENIOR, JUNIOR, FUNCTION, SKILL, LOCATION, EMPLOYER, KNOWN) | Must | M1 |
| F10 | Save a LinkedIn profile the resourcer is viewing, via a Chrome button (extension) that reads only the open page, only on click (no automation). The resourcer checks the details and picks the role; locked-out staff are refused | Must | M1 |
| F11 | Work email and phone: taken from People Data Labs when pulled (paid plan; only an address at the person's own employer), Apollo for shortlisted people otherwise. Personal emails are never taken from a provider | Must | M2 |
| F12 | Draft outreach in the resourcer's voice for email, InMail and WhatsApp | Must | M2 |
| F13 | Approve one person's whole sequence (first message plus follow-ups) once | Must | M2 |
| F14 | Send approved email and follow-ups from the resourcer's own Outlook mailbox | Must | M2 |
| F15 | InMail and WhatsApp assisted: copy and open, a person sends, then marks it sent | Must | M2 |
| F16 | Detect email replies; a Replied button for other channels; stop all follow-ups on any reply | Must | M2 |
| F17 | Today screen: replies waiting, follow-ups due, new matches | Must | M2 |
| F18 | Hand a replied, interested candidate to the ATS | Must | M3 |
| F19 | Learn from reject reasons and draft edits; propose playbook rules for admin approval | Must | M3 |
| F20 | Opt-out and erasure go on a do-not-contact list that is checked before every send | Must | M0 |
| F21 | Client workforce map: where a client's hires come from | Nice | M3 |
| F22 | Re-run a saved search and show only new people | Nice | M3 |
| F23 | Export a shortlist to Google Sheets | Nice | M1 |
| F24 | Owner release: hand a person to a colleague, or ownership lapses after 30 days with no reply | Nice | M2 |
| F25 | Weekly summary of activity and results per resourcer | Nice | M3 |
| F26 | Chat or MCP access to Sourcer from Claude | Nice | Later |

## Non-functional requirements

Safety comes first: no message leaves without approval, and no person is contacted twice for the same role.

| ID | Area | Requirement |
| --- | --- | --- |
| N1 | Performance | Screens load in under 2 seconds on the office connection |
| N2 | Performance | A search of up to 100 records returns a ranked list in under 60 seconds |
| N3 | Security | Sign-in through Microsoft 365 only; no local passwords |
| N4 | Security | API keys and mailbox tokens live in environment variables or encrypted at rest; never in code, logs or the browser |
| N5 | Security | Private hosting; the server listens on localhost and is reached through the VPN or a reverse proxy |
| N6 | Security | Every action that changes a candidate or sends a message is written to the audit log |
| N7 | Safety | Every send path passes the send-safety check: opt-out, do-not-contact, kill switch, approval, reply on any channel, personal-email rule |
| N8 | Safety | Database constraint: one candidacy per person per role |
| N9 | Privacy | Personal emails only for people the team has spoken to before |
| N10 | Privacy | Opt-outs and erasures honoured forever via the do-not-contact list; data kept no longer than needed (period to confirm with an adviser) |
| N11 | Cost | Paid calls (PDL, Apollo, AI) show their cost first; an admin can pause all paid calls |
| N12 | Scalability | Every row carries an organisation id, so other agencies can be added without a rebuild |
| N13 | Scalability | Designed for up to 10 users and 100,000 people records on one server |
| N14 | Reliability | Background jobs retry safely; a repeated search never charges twice (idempotency key) |
| N15 | Reliability | Daily database backup, tested restore |
| N16 | Quality | Every change passes CI: format, lint, tests, migrations, type drift, web build |

## Constraints and out of scope

**Constraints**

- Stack: Rust (Axum, Tokio, SQLx), PostgreSQL 16, TypeScript (React, Vite), Docker Compose. Already built in the M0 scaffold.
- Mail and sign-in: Microsoft 365 through Microsoft Graph. Needs admin consent.
- Data: People Data Labs for discovery (the free tier hides city, email and phone; each search costs at least 1 credit). Apollo for contact details only.
- UK and EU privacy rules apply to candidate data and outreach.
- Small team: build in two-week sprints, MVP first.

**Out of scope**

- Replacing the ATS: no pipeline stages after handover, interviews, offers or placements.
- LinkedIn automation of any kind: no scraping, auto-connect or auto-InMail.
- Cold WhatsApp through the business API.
- Sending from shared or bought mailboxes; bulk campaigns.
- Client-facing portal, billing, or selling to other agencies (the data model allows it later).

## Open decisions

D1 to D4 and D8 are decided. Five decisions remain; they block later milestones.

| # | Decision | Blocks | Status |
| --- | --- | --- | --- |
| D1 | Microsoft 365 admin consent for sign-in and sending mail. **Yes:** Kai registers the app | Sprint 1 (F1) | Decided |
| D2 | Hosting. **Office machine**, reached over the office network or VPN | Sprint 1 (deploy) | Decided |
| D3 | PDL plan. **Paid plan bought once the build is complete;** build and test on the free tier with saved sample data | Sprint 1 (F5) | Decided |
| D4 | Proposed targets N1, N2, N13 and N15. **Confirmed** | Sprint 1 | Decided |
| D5 | Team: who uses it at launch, and who is admin | M1 | Open |
| D6 | Client off-limits list | M1 (F8) | Open |
| D7 | Apollo API key | M2 (F11) | Open |
| D8 | AI provider and key for briefs, ranking and drafts. **Claude** (Anthropic); Kai adds the key | M1 (F3, F6) | Decided |
| D9 | Privacy adviser sign-off: do-not-contact list, retention period, personal-email rule | M2 (sending) | Open |
| D10 | Which ATS, and how handover works (API or export) | M3 (F18) | Open |

## Project plan

Four milestones over 14 weeks, built as two-week sprints. The MVP is the end of M2: a resourcer can find, approve and contact candidates safely.

| Milestone | Weeks | Delivers | Gate |
| --- | --- | --- | --- |
| M0 Foundations | 1 to 2 | Microsoft 365 sign-in, PDL and Apollo links, job queue, audit log, deploy and backups | A search runs from the app |
| M1 Find | 3 to 6 | Brief check, search, rank, known check, Candidates screen, LinkedIn save button | 12 of 30 accepted on a real role |
| **M2 Reach (MVP)** | 7 to 10 | Drafts in your voice, email and follow-ups, InMail and WhatsApp assist, replies, Today screen | 50 sent, zero double contacts |
| M3 Learn + handover | 11 to 14 | Playbook rules, learning from edits, ATS handover, client maps | Team uses it daily for two weeks |

Each milestone ends at its gate. The next one starts only once that test passes on real work.

**Sprint 1 (weeks 1 to 2): finish M0**

Goal: a signed-in resourcer runs a PDL search from the app and sees the results, with every step in the audit log.

- [ ] Microsoft 365 sign-in with admin and resourcer roles (F1, needs D1)
- [ ] PDL connection: count, search, cost shown first, no double charge; tested on saved sample data, one live check on the free tier (F5, N11, N14)
- [ ] Apollo connection behind the same interface, switched on when the key arrives (F11, D7)
- [ ] Background job queue with safe retries (N14)
- [ ] Audit log on every change and send (N6)
- [ ] Deploy to the office machine with daily backups (N5, N15)
- [ ] CI green; self-review before merge (N16)

## Sign-off and change log

This SRS is the source of truth. Any change to scope or requirements is recorded here before it is built.

- [ ] Kai approves the requirements, scope and project plan
- [x] D1 to D4 answered, so Sprint 1 can start

| Date | Change |
| --- | --- |
| 30 Sep 2026 | F10: LinkedIn save button built as a Chrome extension (Kai: Chrome, save straight to a role). Recruitly is the CRM/ATS for the known check and handover (D10, to plan) |
| 30 Sep 2026 | F6, F7, F9: Candidates screen built. Ranking runs after each pull; known people are flagged, not hidden (Kai). LinkedIn save button (F10) moves to the next sprint |
| 30 Sep 2026 | F5: search step built (count, then pull). F11: PDL work email and phone are saved when pulled; personal emails never (Kai) |
| 30 Sep 2026 | F3: brief gains Capabilities (functional and soft skills, ranking only) and Domain focus (Must narrows the search, Plus only ranks). The hiring client and off-limits clients are always left out. D8 decided: Claude |
| 29 Sep 2026 | D1 to D4 decided: Microsoft 365 consent yes, office machine hosting, PDL paid plan after the build, targets confirmed |
| 29 Sep 2026 | First version, drawn from the Product and Solution Design doc and the decisions log |

Sources: [Product and Solution Design](https://claude.ai/code/artifact/a3f61e00-2e2c-457a-b135-7b3387bc7179) · [Workspace UI design](https://claude.ai/artifact/TnSaPrFXBCeqxP7ErBmrTr)
