import { useEffect, useState, type ReactNode } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, SignedOut } from "../api/client";
import type { CandidateRow } from "../api/types/CandidateRow";
import type { CandidateTab } from "../api/types/CandidateTab";
import type { DecisionAction } from "../api/types/DecisionAction";
import type { ReasonCode } from "../api/types/ReasonCode";
import { Steps } from "./Briefs";

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

const reasonLabel = (code: ReasonCode | null) => REASONS.find((r) => r.code === code)?.label ?? code;

const isTab = (t: string | null): t is CandidateTab => t === "review" || t === "shortlisted" || t === "rejected";

/** Claude marks its strongest evidence with **double asterisks**; show those in bold, as text. */
export function Evidence({ text }: { text: string }) {
  const parts: ReactNode[] = text.split("**").map((part, i) => (i % 2 === 1 ? <b key={i}>{part}</b> : part));
  return <>{parts}</>;
}

/** Step 4: ranked people for one role. Shortlist or reject; nothing is sent from here. */
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
        <Steps at={4} />
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
        {v.unranked > 0 && (
          <div className="rankstate" role="status">
            {v.ranking ? (
              <span>
                Ranking {v.unranked} {v.unranked === 1 ? "person" : "people"}. This takes about a minute.
              </span>
            ) : v.rank_blocked ? (
              <span className="warnline">
                {v.unranked} not ranked. {v.rank_blocked}
              </span>
            ) : (
              <>
                <span>{v.unranked} not ranked yet.</span>
                <button className="btn-ghost" onClick={() => rank.mutate()} disabled={rank.isPending}>
                  {rank.isPending ? "Starting" : "Rank now"}
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
      </section>

      <section className="panel people" aria-label={TABS.find((t) => t.tab === tab)?.label}>
        {v.people.length === 0 ? (
          <Empty tab={tab} roleId={id} />
        ) : (
          v.people.map((p) => <Person key={p.id} p={p} tab={tab} roleId={id} />)
        )}
        {counts[tab] > v.people.length && (
          <p className="note">
            Showing the best {v.people.length} of {counts[tab]}.
          </p>
        )}
      </section>
      <p className="later">Nothing is sent from this screen. Shortlisting only moves a person to the next step.</p>
    </main>
  );
}

function Empty({ tab, roleId }: { tab: CandidateTab; roleId: string }) {
  if (tab === "shortlisted") return <p className="panel-note">No one shortlisted yet.</p>;
  if (tab === "rejected") return <p className="panel-note">No one rejected yet.</p>;
  return (
    <p className="panel-note">
      No one to review. <Link to={`/brief/${roleId}/search`}>Go to search</Link> to find people.
    </p>
  );
}

function Person({ p, tab, roleId }: { p: CandidateRow; tab: CandidateTab; roleId: string }) {
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
  const reach = [p.has_work_email && "Work email", p.has_phone && "Phone"].filter(Boolean).join(" · ");

  return (
    <article className={`cand${p.do_not_contact ? " blocked" : ""}`} aria-label={p.name}>
      <div className={`tier ${(p.tier ?? "none").toLowerCase()}`} aria-label={p.tier ? `Tier ${p.tier}` : "Not ranked"}>
        {p.tier ?? "·"}
      </div>
      <div className="body">
        <div className="who">
          <span className="nm">{p.name}</span>
          {where && <span className="ti">{where}</span>}
          {p.do_not_contact && <span className="flag known">Do not contact</span>}
          {p.known && <span className="flag known">{p.known}</span>}
          {p.employer_unknown && <span className="flag emp">Check employer</span>}
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
        <div className="reach">{reach || "No contact details yet"}</div>
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
