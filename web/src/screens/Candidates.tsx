import { useEffect, useRef, useState, type ReactNode } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, SignedOut } from "../api/client";
import type { CheckVerdict } from "../api/types/CheckVerdict";
import type { ContactLine } from "../api/types/ContactLine";
import type { RankCheck } from "../api/types/RankCheck";
import type { CandidateRow } from "../api/types/CandidateRow";
import type { CandidateTab } from "../api/types/CandidateTab";
import type { DecisionAction } from "../api/types/DecisionAction";
import type { ReasonCode } from "../api/types/ReasonCode";
import type { RecruitlyLink } from "../api/types/RecruitlyLink";
import { Steps } from "./Briefs";
import { CvLine } from "./Cv";
import { EmailLine } from "./Outreach";
import { ago, jobRef, RecruitlyJobBar } from "./Recruitly";

const TABS: { tab: CandidateTab; label: string }[] = [
  { tab: "review", label: "To review" },
  { tab: "shortlisted", label: "Shortlisted" },
  { tab: "rejected", label: "Rejected" },
];

/** Reject reasons in words; the code is what the playbook learns from. */
export const REASONS: { code: ReasonCode; label: string }[] = [
  { code: "FIT", label: "Not a fit" },
  { code: "SENIOR", label: "Too senior" },
  { code: "JUNIOR", label: "Too junior" },
  { code: "FUNCTION", label: "Wrong function" },
  { code: "SKILL", label: "Missing skill" },
  { code: "LOCATION", label: "Wrong location" },
  { code: "EMPLOYER", label: "Employer" },
  { code: "KNOWN", label: "Already known" },
];

/** States that have, or can have, an email line. */
const EMAIL_STATES = ["shortlisted", "drafted", "approved", "contacted", "replied", "no_reply"];

const reasonLabel = (code: ReasonCode | null) => REASONS.find((r) => r.code === code)?.label ?? code;

const isTab = (t: string | null): t is CandidateTab => t === "review" || t === "shortlisted" || t === "rejected";

/** Claude marks its strongest evidence with **double asterisks**; show those in bold, as text. */
export function Evidence({ text }: { text: string }) {
  const parts: ReactNode[] = text.split("**").map((part, i) => (i % 2 === 1 ? <b key={i}>{part}</b> : part));
  return <>{parts}</>;
}

/** Step 4: ranked people for one role. Shortlist or reject, then add shortlisted people to Recruitly. */
export function Candidates() {
  const { id = "" } = useParams();
  const [params, setParams] = useSearchParams();
  const tab: CandidateTab = isTab(params.get("tab")) ? (params.get("tab") as CandidateTab) : "review";
  const queryClient = useQueryClient();
  const role = useQuery({ queryKey: ["role", id], queryFn: () => api.role(id) });
  const list = useQuery({
    queryKey: ["candidates", id, tab],
    queryFn: () => api.candidates(id, tab),
    retry: (n, e) => !(e instanceof SignedOut) && n < 2,
    // Poll while Claude is ranking in the background.
    refetchInterval: (q) => (q.state.data?.ranking ? 3000 : false),
  });
  useEffect(() => {
    if (list.error instanceof SignedOut || role.error instanceof SignedOut) queryClient.setQueryData(["me"], null);
  }, [list.error, role.error, queryClient]);

  const rank = useMutation({
    mutationFn: () => api.rankNow(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["candidates", id] }),
  });

  if (list.isLoading || role.isLoading) return <main className="panel-note">Loading</main>;
  if (!list.data || !role.data) return <main className="form-error">Could not load the candidates for this role.</main>;
  const v = list.data;
  const r = role.data;
  const counts: Record<CandidateTab, number> = {
    review: v.to_review,
    shortlisted: v.shortlisted,
    rejected: v.rejected,
  };

  return (
    <main>
      <div className="head-row">
        <header>
          <div className="eyebrow">{r.client?.name ?? "Role"}</div>
          <h1>{r.title}</h1>
        </header>
        <Steps at={3} roleId={id} />
      </div>

      <section className="panel">
        <div className="toolbar">
          <div className="tabs" role="tablist" aria-label="Candidates">
            {TABS.map((t) => (
              <button
                key={t.tab}
                role="tab"
                aria-selected={tab === t.tab}
                className={tab === t.tab ? "on" : undefined}
                onClick={() => setParams(t.tab === "review" ? {} : { tab: t.tab }, { replace: true })}
              >
                {t.label}
                <span className="c">{counts[t.tab]}</span>
              </button>
            ))}
          </div>
          <span className="rankline">
            {v.brief_version
              ? `Ranked by Claude against brief version ${v.brief_version}. Best first. A has every must-have, B most, C few.`
              : "Confirm the brief to rank people."}
          </span>
        </div>
        {(v.unranked > 0 || v.stale > 0) && (
          <div className="rankstate" role="status">
            {v.ranking ? (
              <span>
                {v.unranked > 0
                  ? `Ranking ${v.unranked} ${v.unranked === 1 ? "person" : "people"}.`
                  : `Re-ranking ${v.stale} ${v.stale === 1 ? "person" : "people"} against brief version ${v.brief_version}.`}{" "}
                This takes about a minute.
              </span>
            ) : v.rank_blocked ? (
              <span className="warnline">
                {v.unranked > 0 ? `${v.unranked} not ranked.` : `${v.stale} ranked against an older brief.`}{" "}
                {v.rank_blocked}
              </span>
            ) : (
              <>
                <span>
                  {v.unranked > 0 ? `${v.unranked} not ranked yet.` : `${v.stale} ranked against an older brief.`}
                </span>
                <button className="btn-ghost" onClick={() => rank.mutate()} disabled={rank.isPending}>
                  {rank.isPending ? "Starting" : v.unranked > 0 ? "Rank now" : "Re-rank now"}
                </button>
              </>
            )}
            {rank.error && (
              <p className="form-error" role="alert">
                {rank.error.message}
              </p>
            )}
          </div>
        )}
        {v.recruitly && <RecruitlyJobBar roleId={id} link={v.recruitly_job} />}
      </section>

      <section className="panel people" aria-label={TABS.find((t) => t.tab === tab)?.label}>
        {v.people.length === 0 ? (
          <Empty tab={tab} roleId={id} />
        ) : (
          v.people.map((p) => (
            <Person key={p.id} p={p} tab={tab} roleId={id} recruitly={v.recruitly} job={v.recruitly_job} />
          ))
        )}
        {counts[tab] > v.people.length && (
          <p className="note">
            Showing the best {v.people.length} of {counts[tab]}.
          </p>
        )}
      </section>
      <p className="later">
        Emails go out only after you approve them, from your own Outlook.
        {v.recruitly ? " Adding someone to Recruitly copies their record there; it sends them nothing." : ""}
      </p>
    </main>
  );
}

function Empty({ tab, roleId }: { tab: CandidateTab; roleId: string }) {
  if (tab === "shortlisted") return <p className="panel-note">No one shortlisted yet.</p>;
  if (tab === "rejected") return <p className="panel-note">No one rejected yet.</p>;
  return (
    <p className="panel-note">
      No one to review. <Link to={`/roles/${roleId}/search`}>Go to search</Link> to find people.
    </p>
  );
}

function Person({
  p,
  tab,
  roleId,
  recruitly,
  job,
}: {
  p: CandidateRow;
  tab: CandidateTab;
  roleId: string;
  recruitly: boolean;
  job: RecruitlyLink | null;
}) {
  const queryClient = useQueryClient();
  const [rejecting, setRejecting] = useState(false);
  const decide = useMutation({
    mutationFn: (d: { action: DecisionAction; reason: ReasonCode | null }) =>
      api.decide(p.id, { ...d, version: p.version }),
    onSuccess: () => setRejecting(false),
    // Refresh either way: a refusal usually means someone else changed this person.
    onSettled: () => queryClient.invalidateQueries({ queryKey: ["candidates", roleId] }),
  });
  const ranked = p.state !== "found" && p.state !== "known_checked";
  const where = [p.title, p.employer].filter(Boolean).join(" · ");

  return (
    <article className={`cand${p.do_not_contact ? " blocked" : ""}`} aria-label={p.name}>
      <div className="mark">
        <div
          className={`tier ${(p.tier ?? "none").toLowerCase()}`}
          role="img"
          aria-label={p.tier ? `Tier ${p.tier}` : "Not ranked"}
        >
          {p.tier ?? "·"}
        </div>
        {p.score !== null && (
          <div className="score" title="Claude's fit score out of 100">
            <span className="sr">Score </span>
            {p.score}
            <span className="sr"> out of 100</span>
          </div>
        )}
      </div>
      <div className="body">
        <div className="who">
          <span className="nm">{p.name}</span>
          {where && <span className="ti">{where}</span>}
          {p.do_not_contact && <span className="flag stop">Do not contact</span>}
          {p.known && <span className="flag known">{p.known}</span>}
          {p.employer_unknown && <span className="flag emp">Check employer</span>}
          {p.stale_rank && (
            <span className="flag emp" title="Scored against an older brief. A re-rank is due.">
              Older brief
            </span>
          )}
          {tab === "rejected" && p.reject_reason && (
            <span className="flag emp">Rejected: {reasonLabel(p.reject_reason)}</span>
          )}
        </div>
        {p.location && <div className="sub">{p.location}</div>}
        {p.reason ? (
          <p className="why">
            <Evidence text={p.reason} />
          </p>
        ) : (
          <p className="why dim">Not ranked yet.</p>
        )}
        {p.checks.length > 0 && <Checks checks={p.checks} />}
        {p.unknowns.length > 0 && (
          <div className="unk">
            <span className="lbl2">To check:</span>
            {p.unknowns.map((u) => (
              <span key={u} className="q2">
                {u}
              </span>
            ))}
          </div>
        )}
        <Reach contacts={p.contacts} blocked={p.do_not_contact} />
        {tab !== "rejected" && ranked && <CvLine p={p} roleId={roleId} />}
        {recruitly && tab === "shortlisted" && <RecruitlyLine p={p} roleId={roleId} job={job} />}
        {EMAIL_STATES.includes(p.state) && <EmailLine p={p} roleId={roleId} />}
      </div>
      <div className="acts">
        {tab === "review" && ranked && !rejecting && (
          <button
            className="btn-ghost"
            onClick={() => decide.mutate({ action: "shortlist", reason: null })}
            disabled={decide.isPending || p.do_not_contact}
          >
            Shortlist
          </button>
        )}
        {tab !== "rejected" && ranked && !rejecting && (
          <button className="rej" onClick={() => setRejecting(true)} disabled={decide.isPending}>
            Reject
          </button>
        )}
        {tab === "rejected" && (
          <button
            className="rej"
            onClick={() => decide.mutate({ action: "reconsider", reason: null })}
            disabled={decide.isPending}
          >
            Reconsider
          </button>
        )}
        {p.linkedin_url && (
          <a className="link-button li" href={`https://${p.linkedin_url}`} target="_blank" rel="noopener noreferrer">
            LinkedIn ↗
          </a>
        )}
      </div>
      {rejecting && (
        <div className="reasons" role="group" aria-label={`Why reject ${p.name}?`}>
          <span className="t">Why reject?</span>
          {REASONS.map((r) => (
            <button
              key={r.code}
              onClick={() => decide.mutate({ action: "reject", reason: r.code })}
              disabled={decide.isPending}
            >
              {r.label}
            </button>
          ))}
          <button className="link-button cancel" onClick={() => setRejecting(false)}>
            Cancel
          </button>
        </div>
      )}
      {decide.error && (
        <p className="form-error rowerr" role="alert">
          {decide.error.message}
        </p>
      )}
    </article>
  );
}

/** What Recruitly knows about a shortlisted person, and adding them there. */
function RecruitlyLine({ p, roleId, job }: { p: CandidateRow; roleId: string; job: RecruitlyLink | null }) {
  const queryClient = useQueryClient();
  // The question waiting for a yes, and the questions already answered yes.
  const [confirm, setConfirm] = useState<{ text: string; key: string } | null>(null);
  const [yes, setYes] = useState<string[]>([]);
  // Every Recruitly call spends from the daily allowance, so the count is read again too.
  const refresh = () => {
    queryClient.invalidateQueries({ queryKey: ["candidates", roleId] });
    queryClient.invalidateQueries({ queryKey: ["recruitly-status"] });
  };
  const check = useMutation({ mutationFn: () => api.recruitlyCheck(p.id), onSettled: refresh });
  const add = useMutation({
    mutationFn: (confirmed: string[]) => {
      check.reset();
      return api.handover(p.id, confirmed);
    },
    onSuccess: (r) => {
      setConfirm(r.confirm && r.confirm_key ? { text: r.confirm, key: r.confirm_key } : null);
      // A question can follow a fresh check, so the line is read again either way.
      refresh();
    },
    onError: () => {
      // A failed try starts over: every question is asked again.
      setConfirm(null);
      setYes([]);
      refresh();
    },
  });
  const agree = (key: string) => {
    const next = [...yes, key];
    setYes(next);
    add.mutate(next);
  };
  const busy = check.isPending || add.isPending;
  const me = useQuery({ queryKey: ["me"], queryFn: api.me, retry: false });
  const myId = me.data?.recruitly_user_id;
  const mine = !!myId && p.recruitly_owner_id === myId;
  const target = job ? `${jobRef(job)} in Recruitly` : "Recruitly";

  // Three levels: stop (never add), check (look first) and clear.
  let flag: ReactNode;
  if (p.sent_to_recruitly) {
    flag = (
      <span className="flag sent">
        Added to Recruitly {p.sent_to_recruitly}
        {p.in_recruitly_pipeline && job ? ` · in ${jobRef(job)}` : ""}
      </span>
    );
  } else if (p.do_not_contact) {
    flag = <span className="flag stop">Can't be added to Recruitly</span>;
  } else if (p.recruitly_check_failed) {
    flag = <span className="flag emp">Recruitly check failed</span>;
  } else if (!p.recruitly_checked) {
    flag = <span className="flag emp">Not checked in Recruitly</span>;
  } else if (p.recruitly_note) {
    // Already in Recruitly with an owner, stage or history: look before adding.
    // Your own record needs no second look.
    const clear = p.recruitly_note === "In Recruitly" || mine;
    flag = (
      <span className={`flag ${clear ? "rc" : "emp"}`}>{mine ? ownedByYou(p.recruitly_note) : p.recruitly_note}</span>
    );
  } else {
    flag = <span className="flag rc">Not in Recruitly</span>;
  }
  // Can be added: not yet sent and not on the do-not-contact list.
  const open = !p.sent_to_recruitly && !p.do_not_contact;
  // A pending question goes away if the row changes under it.
  useEffect(() => {
    if (!open) {
      setConfirm(null);
      setYes([]);
    }
  }, [open]);
  // The question takes focus so keyboard and screen reader users meet it.
  const yesButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (confirm) yesButton.current?.focus();
  }, [confirm]);

  return (
    <div className="rcline">
      <div className="rcflags">
        {flag}
        {p.recruitly_checked_at && !p.recruitly_check_failed && !p.sent_to_recruitly && (
          <span className="rcwhen">Checked {ago(p.recruitly_checked_at)}</span>
        )}
        {/* Still offered when blocked: Recruitly may since have cleared its flag. */}
        {!p.sent_to_recruitly && (
          <button type="button" className="link-button" onClick={() => check.mutate()} disabled={busy}>
            {check.isPending ? "Checking" : p.recruitly_checked ? "Check again" : "Check now"}
          </button>
        )}
      </div>
      {open && !confirm && (
        <button type="button" className="btn-ghost rcadd" onClick={() => add.mutate(yes)} disabled={busy}>
          {add.isPending ? "Adding" : `Add to ${target}`}
        </button>
      )}
      {open && confirm && (
        <div className="confirm" role="group" aria-label="Check before adding">
          <p>{confirm.text}</p>
          <button
            ref={yesButton}
            type="button"
            className="btn-ghost"
            onClick={() => agree(confirm.key)}
            disabled={busy}
          >
            {add.isPending ? "Adding" : "Add anyway"}
          </button>
          <button
            type="button"
            className="link-button"
            onClick={() => {
              setConfirm(null);
              setYes([]);
            }}
          >
            Cancel
          </button>
        </div>
      )}
      {(check.error || add.error) && (
        <p className="form-error" role="alert">
          {(add.error ?? check.error)?.message}
        </p>
      )}
    </div>
  );
}

/** "In Recruitly: owned by Sam Lee · Placed" as "In Recruitly: owned by you · Placed". */
export const ownedByYou = (note: string) => note.replace(/owned by [^·]+?(?=( · |$))/, "owned by you");

const VERDICT: Record<CheckVerdict, { mark: string; label: string }> = {
  met: { mark: "✓", label: "Met" },
  partly: { mark: "~", label: "Partly" },
  not_shown: { mark: "?", label: "Not shown" },
};

const CONTACT_LABEL: Record<ContactLine["kind"], string> = {
  work_email: "Work",
  personal_email: "Personal",
  phone: "Phone",
};

/** How to reach them: each address opens your mail app or phone, with a copy button. */
function Reach({ contacts, blocked }: { contacts: ContactLine[]; blocked: boolean }) {
  const [copied, setCopied] = useState<string | null>(null);
  if (blocked) return <div className="reach">Contact details hidden: do not contact.</div>;
  if (contacts.length === 0) return <div className="reach">No contact details yet.</div>;
  const copy = (value: string) => {
    navigator.clipboard?.writeText(value).then(
      () => {
        setCopied(value);
        window.setTimeout(() => setCopied((c) => (c === value ? null : c)), 1500);
      },
      () => setCopied(null),
    );
  };
  return (
    <ul className="reach contacts" aria-label="Contact details">
      {contacts.map((c) => (
        <li key={`${c.kind}:${c.value}`} className="ct">
          <span className="k">{CONTACT_LABEL[c.kind]}</span>
          <a href={`${c.kind === "phone" ? "tel" : "mailto"}:${c.value}`}>{c.value}</a>
          <button className="cp" onClick={() => copy(c.value)} aria-label={`Copy ${c.value}`}>
            {copied === c.value ? "Copied" : "Copy"}
          </button>
        </li>
      ))}
    </ul>
  );
}

/** Claude's verdict on each line of the brief, in the brief's order. */
function Checks({ checks }: { checks: RankCheck[] }) {
  const met = checks.filter((c) => c.verdict === "met").length;
  return (
    <div className="checks">
      <span className="lbl2">
        {met} of {checks.length} met
      </span>
      <ul>
        {checks.map((c) => (
          <li key={c.item} className={`ck ${c.verdict}`}>
            <span className="m" aria-hidden="true">
              {VERDICT[c.verdict].mark}
            </span>
            <span className="sr">{VERDICT[c.verdict].label}: </span>
            {c.item}
          </li>
        ))}
      </ul>
    </div>
  );
}
