# Information Architecture: Sourcer

Audit of the app as built (main at 2d1ca50, 2 Oct 2026), with the proposed structure.
Status updated 2 Oct 2026: findings 1 to 8 and 11 are built (branch ia-roles-admin). 9 and 10 wait for M2b.
Users: Kai (admin) and a small resourcer team. Desktop first, phone for checking in.

## Audit findings

| # | Finding | Why it matters | Fix | Status |
|---|---|---|---|---|
| 1 | The home page (`/` → `/today`) is an empty placeholder. | Every sign-in lands on a dead end. | Home goes to Roles until Today is built in M2b. | Done |
| 2 | "Brief" and "Candidates" are two menu items listing the same roles. | Two doors to one thing. The real object is the role, and the brief is one step of it. | One "Roles" section. Each role has its steps as tabs. | Done |
| 3 | Four of the eight menu items are placeholders: Today, Outreach, Client map and Playbook. | It looks unfinished, and people click on nothing. | Hide each one until it works. | Done |
| 4 | There is no screen for the kill switches. `sending_paused` and `paid_calls_paused` can only be changed in the database. | Before emails go out (M2b), an admin must be able to stop sending in one click. | An Admin page with a controls section. | Done |
| 5 | Clients and the off-limits flag are set only when a client is created inside "New role". They can't be listed or edited afterwards. | The off-limits list is a safety rule, yet nobody can see or fix it. | Admin › Clients. | Done |
| 6 | The do-not-contact list (opt-outs, erasure, F20) has no screen. | When someone replies "no", you can't add them, check them or prove it. | Admin › Do not contact. Replies add to it automatically in M2b. | Done (adding by hand; replies in M2b) |
| 7 | The Chrome button setup instructions sit on the Candidates page. | They're a one-time setup, shown every visit. | Move them to Settings. | Done |
| 8 | URLs read `/brief/:id/candidates`. | A candidate list is not part of a brief. | `/roles/:id/candidates`. Old links redirect. | Done |
| 9 | There's no view of all emails across roles. | In M2b you need one queue: to approve, sending, replied. | Outreach page in M2b. It also feeds Today. | M2b |
| 10 | Roles have no status (open or closed) and no filter. | The list only grows. Old roles bury live ones. | An Open/Closed filter, taken from Recruitly where the role is linked. | Later |
| 11 | On phones, the 8 nav items scroll sideways. | Hard to use on a phone. | With 3 to 4 main items, they fit on one row. | Done |

## Site Map

Built now unless marked.

- Roles `/roles`: the home page until Today exists
  - New role `/roles/new`
  - Role `/roles/:id`: opens the step the role is at
    - Spec and brief `/roles/:id/brief`
    - Search `/roles/:id/search`
    - Candidates `/roles/:id/candidates?tab=review|shortlisted|rejected`
- Today `/today` (M2b): becomes the home page once it's built
- Outreach `/outreach?tab=approve|sending|replied|stopped` (M2b)
- Playbook `/playbook` (M3, admin)
- Settings `/settings`: your emails, Chrome button, Outlook connection (M2b)
- Admin `/admin` (admin only)
  - Team `/admin/team`
  - Clients and off-limits `/admin/clients`
  - Do not contact `/admin/do-not-contact`
  - Controls `/admin/controls`: pause sending, pause paid calls, Recruitly and PDL usage
- Client map: dropped from the menu. It's a "nice" item (F21) for later, and would live under Admin › Clients.

Redirects: `/brief` → `/roles`, `/brief/:id/*` → `/roles/:id/*`, `/candidates` → `/roles`, `/team` → `/admin/team`.

## Navigation Model

- **Primary (left sidebar, at most 4):** Today (M2b), Roles, Outreach (M2b), Playbook (M3). Until M2b this is only Roles.
- **Secondary:** inside a role, the existing step bar (Spec · Brief · Search · Candidates) becomes clickable tabs. Candidates keeps its To review / Shortlisted / Rejected tabs.
- **Utility (sidebar foot):** your name, Settings, Admin (admins only), Sign out, and the Server and Recruitly lights.
- **Mobile:** a top bar with the primary items, and Settings/Admin under your name.

## Content Hierarchy

### Roles
1. Open roles, newest activity first: title, client, step reached, count to review. This tells you where work is waiting.
2. "New role". This is the second most common action.
3. Closed roles, behind a filter.

### Role › Candidates (where most time is spent)
1. Tabs with counts. They show what needs deciding.
2. Each person: tier, score, why, and the brief checks. This is what you decide on.
3. Contact details, then CV, Recruitly and Email lines on shortlisted people. These are acted on after the decision.
4. Ranking status and the Recruitly job link. These are background state.

### Today (M2b)
1. Replies waiting. A real person is waiting on you.
2. Emails to approve. Nothing sends until they're approved.
3. Follow-ups going out today, and new matches. Information only.

### Admin › Controls
1. Pause sending and Pause paid calls, each with its current state. These are the safety switches.
2. Usage this month: PDL credits, Recruitly calls.

## User Flows

### Fill a role (exists today)
1. Roles → New role: paste a spec, or pick a Recruitly job (the Client Brief comes in with it once wired).
2. Brief tab: check Claude's draft, answer the tool questions, confirm.
3. Search tab: count per location, then pull a number.
   - If the count is thin → Round 2 suggestions.
4. Candidates tab: shortlist or reject with a reason.
   - If someone is known or do-not-contact → they're flagged, and do-not-contact people can't be shortlisted.
5. For a shortlisted person: add the CV, add to Recruitly, draft 3 emails → approve.

### Reach out (M2b)
1. Today shows "3 emails to approve" → opens Outreach › To approve.
2. Review → Approve all 3 → status becomes Sending.
3. Reply arrives → the rest stop → Today › Replies.
   - If they say no → they go on the do-not-contact list.
   - If they're interested → add them to the Recruitly job.

### Stop everything (admin)
1. Admin → Controls → Pause sending. Nothing goes out until it's switched back on.

## Naming Conventions

| Concept | Label in UI | Notes |
|---|---|---|
| A job you're filling | Role | Not "brief" or "job". "Job" only means the Recruitly record. |
| The checked search criteria | Brief | One step of a role. |
| The pasted job description | Spec | |
| People found for a role | Candidates | |
| The three emails | Emails | Not "sequence" or "outreach" on cards. "Outreach" is only the cross-role page. |
| Hand to the ATS | Add to Recruitly | Matches the button already built. |
| Opt-outs and erasure | Do not contact | Same words everywhere. |
| Kill switch | Pause sending / Pause paid calls | |
| The hiring company | Client | Never named in emails. |

## Component Reuse Map

| Component | Used on | Differences |
|---|---|---|
| Sidebar shell | Every signed-in page | Admin link only for admins |
| Role header + step tabs | All four role tabs | The current step is highlighted |
| Candidate card | Candidates tabs, later Outreach | Actions change by state |
| Panel + table list | Roles, Admin lists | |
| Settings-style form panel | Settings, Admin › Controls | |

## Content Growth Plan

- **Roles:** grow without limit. Open/Closed filter, plus search by title or client once there are more than about 30.
- **Candidates per role:** capped at 200 shown, best first. Add a name/employer filter if roles regularly pass 200.
- **Outreach and do-not-contact lists:** paged, 50 per page, newest first, with search by email.
- **Audit log:** stays in the database, with no screen until asked for.

## URL Strategy

- Pattern: `/section/:id/step`, for example `/roles/:id/candidates`.
- Dynamic segments: role id (UUID) only.
- Query parameters: `tab` for tabs inside a page. Filters such as `?status=open` are kept in the URL so they survive a reload and can be shared.
- Old `/brief/...` links redirect permanently.

## Built (2 Oct 2026)

- `/` opens Roles. The menu is Roles, then Settings and Admin (admins only). Placeholders are hidden.
- A role's steps (Spec and brief, Search, Candidates) are clickable tabs. `/roles/:id` opens the step the role is at.
- Admin has four tabs: Team, Clients (edit name, domain, off-limits), Do not contact (add, search, 50 a page; no removal), Controls (Pause sending, Pause paid calls, Recruitly).
- Adding someone to Do not contact stops their waiting emails and puts them back on the shortlist, flagged. The audit log keeps the reason, never the address.
- The Chrome button steps moved to Settings. The Chrome button now links to `/roles`.
- Old `/brief/...`, `/team` and unknown addresses redirect.
- Phone: one row of menu items, no sideways scroll. Team hides the email column and Clients hides the domain column on phones.

## Audit: More searches mockup (2 Oct 2026)

Mockup only, nothing built. Measured with Playwright at 1440 and 390.

**P1. Pull button does not say which search it pulls from.** "Pull 25 people · 25 credits" sits under three cards. Fix: one pull line per card ("Pull 25 from Wider titles"), and the bar shows only the total.

**P1. Two gold buttons compete in the bar.** "Count ticked" (gold outline) and "Pull" (gold fill). Fix: counting happens inside each card (the tick runs the count); the bar keeps one primary action.

**P1. Mobile touch targets under 44px.** Chips 29px, Skip/25/38 segment 34px, Count checkbox 16px (label 22px), Set up 38px, nav 32px, Sign out 15px. Fix on this screen: segment and buttons 44px tall, the whole tick row tappable.

**P1. Mobile layout breaks.** At 390: bar buttons split mid-label ("Count ticked / · 3 credits"); candidate rows squeeze the "Found by" tag into a three-line pill; "38 found · 1 credit" wraps. The bar is below all three cards, so the pull is far from the choice. Fix: stack bar buttons full width with labels on one line; candidate rows wrap the title under the name and the tag below.

**P2. "Your own" label is drawn as a full-width bordered box.** The other cards use a plain gold label. Cause: `.by.you` picks up a border and block width. Fix: same label style, grey.

**P2. Claude's chips cannot be removed.** To drop one suggested title the user must rebuild it as "Your own". Fix: each "+" chip has a remove button; removing one marks the count stale.

**P2. Overlap is hidden.** "38 found" includes people the brief already found, so the credit cost reads higher than real. Fix: "38 found, 3 already on your list".

**P3. Smallest text 11.5px** ("Claude's pick", "Found by"). Raise to 12.5px, in line with `.meta`. Body text matches the rest of Sourcer (14px); no inputs on screen, so no iOS zoom issue.

**Passes.** Gold on near-black contrast about 12:1 (the detector's 1.0 readings were translucent backgrounds, false positives). No dashes in copy. Credit sum correct (42 + 3 + 25 = 70). Desktop cards equal height (292px). Reuses `.panel`, `.chip`, `.seg.pick`, `.bar`, `.tier`.

**Open question.** Kai said "3 maximun" and "Claude chooses 2 and leave one as an option". Read here as the brief plus up to 3 more. Not yet confirmed.

**Fixed in revised mockup (2 Oct 2026).** Each card holds its own choice: Wider titles picks None/25/All 35, Wider places has its own "Count · 3 credits" button (no tick, no second bar button), Your own has "Set up". The bar has one primary, "Pull 25 people · 25 credits", and says where they come from ("25 from Wider titles"). Each Claude chip has a remove ×. Overlap shown ("35 new · 38 found, 3 already on your list"). "Your own" label now matches the others (class renamed `.lbl`, the clash was the global `.you`). Phone: every control on the screen 44px or more, Pull button full width, candidate rows wrap under the name. Smallest text 12.5px. Left over, app-wide not this screen: nav links 32px and Sign out 15px tall on phones.

## Built: More searches (2 Oct 2026, PR after #32)

- Search screen: the Round 2 panel is gone. A "More searches" panel shows once the brief has a current count: Claude's two picks (asked automatically once per brief version, free of search credits) and "Your own".
- Each card: what it adds as chips (× removes one; the last cannot be removed), its own Count button (one credit per place that differs from the brief), then "N new, not already on your list" and None / 25 / All N. One Pull button for all cards, naming where people come from; over 50 asks once more.
- Your own: add titles, levels, places, employers, make required tools optional, fewer years. Widen only: it can never narrow the brief.
- Candidates: "Found by: <search>" on anyone a wider search found.
- Brief editor: 15 more employer types (Banks to Healthcare), each mapped to People Data Labs industries.
- Review fixes before shipping: adding employer types to a brief with none would have narrowed it (now ignored); a press on several searches now shows as one result; picks clear after a pull; a count still running keeps its key, so a retry never pays twice; chips cannot be changed mid-count; Claude is not asked while searching is blocked.
- Measured at 390: every control in the panel 44px or taller; no sideways scroll.

## Built: Tightening (2 Oct 2026, PR after #33)

- Search screen: when the brief's current count finds more than 300 and nothing has been pulled from it, a "Too many to pull well" panel replaces More searches. "Ask Claude to tighten" uses no search credits.
- Claude's answer: "Why so many" (two sentences), then one or two changes, each a ticked box with the label ("Require vLLM", "Search in London, San Francisco (was anywhere)") and the spec's own words as a quote under it. "Agree and count" confirms the next brief version and counts it with the usual Count step; "Not now" closes it. Header shows "round 1 of 2"; after two rounds the panel says to edit the brief yourself.
- The count panel then shows "Before tightening: 26,783."
- Server checks, so a reply can only narrow and only from the spec: the quote must be in the spec (12 characters or more) and name the tool, domain or number it backs; cities must be named in the spec; a second Must domain is refused (Must domains are any one of, so it would widen); every list keeps one entry; leave-out list, excluded titles and must-haves never change; each change must stand on its own so any can be unticked.
- Measured at 390: checkboxes rows, Not now and the main button 44px or taller; no sideways scroll.

## Built: Open and closed roles (2 Oct 2026, PR after #34)

- Roles list: "Open (n) | Closed (n)" tabs, open by default. Each open role has a quiet "Close"; it asks once in the row ("Stops 3 email sequences still going." or "No more searching or emails.", Keep open / Close role). Closed roles show "Reopen".
- A closed role: Search shows "This role is closed. Reopen it on the Roles page to search again." and every count and pull is refused; approval is refused and every email still due stops at its check before sending ("This role is closed."); Today hides its drafts to approve and emails going out. Replies are still read and shown.
- Reopening allows searching and new approvals again; sequences already stopped stay stopped.
- Phones: role rows become cards (title, client and brief, then actions); every control 44px or taller; no sideways scroll. This also fixes the Roles table running off the side on phones.
- Known edge: a pull already queued when the role is closed still runs.

## Built: Send a test to myself (3 Oct 2026)

- Settings, Your Outlook: once connected and working, a line explains the test and a "Send a test to myself" button sends one sample first email from your Outlook to your own address, with your signature and the data-source footer. Then: "Sent to <address>. Check your inbox."
- Refused with a plain reason when Outlook is not connected, needs connecting again, or there is no signature; at most one a minute. No candidate is involved and nothing counts towards the daily limit.
- Measured: button 44px on phones, no sideways scroll.
