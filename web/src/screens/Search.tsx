import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, SignedOut } from "../api/client";
import type { BriefLines } from "../api/types/BriefLines";
import type { CountLocation } from "../api/types/CountLocation";
import type { LockedOut } from "../api/types/LockedOut";
import type { PullView } from "../api/types/PullView";
import type { RetuneView } from "../api/types/RetuneView";
import type { SearchState } from "../api/types/SearchState";
import { Steps } from "./Briefs";

/** Pulls bigger than this ask for a second click (the server checks too). */
const CONFIRM_ABOVE = 50;
const SIZES = [25, 50, 100];
/** Most one location can pull in one go. */
const MAX_PULL = 100;
/** Fewer people than this in total, and Claude offers a round 2. */
const FEW = 25;

/**
 * One key per intent: a resend after a lost reply reuses it, so the server
 * returns the first result instead of charging again. Cleared once the server
 * has answered, success or refusal.
 */
function useIntentKey() {
  const key = useRef<string | null>(null);
  return {
    get: () => (key.current ??= newKey()),
    // Network failures keep the key; any answer from the server clears it.
    settle: (e?: unknown) => {
      if (!(e instanceof TypeError)) key.current = null;
    },
  };
}

function newKey() {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function ago(seconds: number) {
  const mins = Math.max(0, Math.round((Date.now() / 1000 - seconds) / 60));
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins} minute${mins === 1 ? "" : "s"} ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.round(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}

/** The choices for one location: sizes it can fill, or all of a small one. */
function choicesFor(total: number): number[] {
  if (total <= 0) return [];
  if (total <= SIZES[0]) return [total];
  return SIZES.filter((n) => n <= Math.min(total, MAX_PULL));
}

/** Step 3: count the matches, choose how many to pull, see what was saved. */
export function Search() {
  const { id = "" } = useParams();
  const queryClient = useQueryClient();
  const role = useQuery({
    queryKey: ["role", id],
    queryFn: () => api.role(id),
  });
  const search = useQuery({
    queryKey: ["search", id],
    queryFn: () => api.search(id),
    retry: (n, e) => !(e instanceof SignedOut) && n < 2,
    // Poll while a pull is running in the background.
    refetchInterval: (q) => (q.state.data?.pull && !q.state.data.pull.done ? 2000 : false),
  });
  const [picks, setPicks] = useState<Record<string, number>>({});
  const [confirming, setConfirming] = useState(false);
  // Counting again spends credits and clears the choices, so it asks first.
  const [recounting, setRecounting] = useState(false);

  // A new count starts a fresh set of choices.
  const countId = search.data?.count?.id;
  useEffect(() => {
    setPicks({});
    setConfirming(false);
    setRecounting(false);
  }, [countId]);

  useEffect(() => {
    if (search.error instanceof SignedOut || role.error instanceof SignedOut) queryClient.setQueryData(["me"], null);
  }, [search.error, role.error, queryClient]);

  const settle = (s: SearchState) => queryClient.setQueryData(["search", id], s);
  const countKey = useIntentKey();
  const pullKey = useIntentKey();
  const agreeKey = useIntentKey();
  const navigate = useNavigate();
  // Round 2: Claude's proposal. Asking is free; nothing is searched until agreed.
  const ask = useMutation({ mutationFn: () => api.retune(id) });
  // The version a round 2 was confirmed as, so a retry after a failed count
  // does not confirm twice.
  const confirmedFrom = useRef<number | null>(null);
  const agree = useMutation({
    mutationFn: async (p: RetuneView) => {
      if (confirmedFrom.current !== p.based_on) {
        await api.confirmBrief(id, p.lines, p.based_on);
        confirmedFrom.current = p.based_on;
        queryClient.invalidateQueries({ queryKey: ["role", id] });
      }
      return api.countMatches(id, agreeKey.get());
    },
    onSuccess: (s) => {
      agreeKey.settle();
      settle(s);
      ask.reset();
    },
    onError: (e) => agreeKey.settle(e),
  });
  // A proposal belongs to the count it read; a new count clears it.
  useEffect(() => {
    ask.reset();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [countId]);
  const editMyself = useMutation({
    mutationFn: (p: RetuneView) => api.saveBrief(id, p.lines),
    onSuccess: (d) => {
      queryClient.setQueryData(["role", id], d);
      navigate(`/brief/${id}`);
    },
  });
  const count = useMutation({
    mutationFn: () => api.countMatches(id, countKey.get()),
    onSuccess: (s) => {
      countKey.settle();
      settle(s);
    },
    onError: (e) => countKey.settle(e),
  });
  const pull = useMutation({
    mutationFn: () =>
      api.pull(id, {
        count_id: countId ?? "",
        picks: Object.entries(picks).map(([location, size]) => ({
          location,
          size,
        })),
        confirmed: confirming,
        key: pullKey.get(),
      }),
    onSuccess: (s) => {
      pullKey.settle();
      settle(s);
      setConfirming(false);
    },
    onError: (e) => pullKey.settle(e),
  });

  if (search.isLoading || role.isLoading) return <main className="panel-note">Loading</main>;
  if (!search.data || !role.data) return <main className="form-error">Could not load the search for this role.</main>;
  const s = search.data;
  const r = role.data;
  const lines = s.lines;
  const busy = count.isPending || pull.isPending || agree.isPending;
  const requested = Object.values(picks).reduce((a, b) => a + b, 0);
  const running = s.pull && !s.pull.done;
  const alreadyPulled = !!s.count && s.pull?.count_id === s.count.id;

  const credits = s.locations.length;
  const creditWord = `${credits} credit${credits === 1 ? "" : "s"}`;
  // A fresh count is the main step; once one exists, counting again is secondary.
  const countIsMain = !s.count || s.count.stale || alreadyPulled;
  const doCount = () => {
    setRecounting(false);
    pull.reset();
    count.mutate();
  };
  const pressCount = () => (s.count && requested > 0 ? setRecounting(true) : doCount());
  // Big pulls confirm with a separate button in a different place, so a
  // double click on Pull cannot pass the check.
  const pressPull = () => (requested > CONFIRM_ABOVE ? setConfirming(true) : pull.mutate());
  const people = `${requested} ${requested === 1 ? "person" : "people"}`;
  const found = s.count ? s.count.locations.reduce((a, c) => a + c.total, 0) : 0;
  // Offer a round 2 on a current count that found too few, before any pull.
  const offerRoundTwo =
    !!s.count && !s.count.stale && !alreadyPulled && found < FEW && !s.unconfirmed_edits && !running;

  return (
    <main>
      <div className="head-row">
        <header>
          <div className="eyebrow">{r.client?.name ?? "Role"}</div>
          <h1>{r.title}</h1>
        </header>
        <Steps at={3} />
      </div>

      {s.blocked && (
        <div className="notice warn-notice" role="status">
          {s.blocked}
          {!s.brief_version && (
            <>
              {" "}
              <Link to={`/brief/${id}`}>Go to the brief</Link>
            </>
          )}
        </div>
      )}
      {s.unconfirmed_edits && s.brief_version && (
        <div className="notice">
          The brief has edits that are not confirmed yet. Searches use confirmed version {s.brief_version}.{" "}
          <Link to={`/brief/${id}`}>Review the brief</Link>
        </div>
      )}

      <div className="search-grid">
        <section className="panel">
          <h2 className="panel-title">What we will search</h2>
          <p className="panel-note">
            {s.brief_version ? `From brief version ${s.brief_version}. ` : ""}
            Nothing is spent until you count.
          </p>
          {lines && <Plan lines={lines} lockedOut={r.locked_out} />}
          <div className="bar">
            <span className="total">
              One count per location: <strong>{creditWord}</strong>
            </span>
            {!recounting && (
              <button
                className={countIsMain ? "btn-primary btn-inline" : "btn-ghost"}
                onClick={pressCount}
                disabled={busy || !!s.blocked || !!running}
              >
                {count.isPending ? "Counting" : `${s.count ? "Count again" : "Count matches"} · ${creditWord}`}
              </button>
            )}
          </div>
          {recounting && (
            <div className="confirm" role="group" aria-label="Count again">
              <p>
                Counting again spends <strong>{creditWord}</strong> and clears the {people} you chose.
              </p>
              <span className="actions-row">
                <button className="link-button" onClick={() => setRecounting(false)}>
                  Keep my choices
                </button>
                <button className="btn-ghost" onClick={doCount} disabled={busy}>
                  Count again · {creditWord}
                </button>
              </span>
            </div>
          )}
          {count.error && <ErrorLine e={count.error} />}
        </section>

        {s.count && (
          <section className="panel">
            <h2 className="panel-title">How many to pull</h2>
            <p className="panel-note">Each person costs 1 credit. Nothing is chosen for you.</p>
            <p className="meta">
              Counted {ago(s.count.counted_at)} from brief version {s.count.brief_version}.
            </p>
            {s.count.stale ? (
              <p className="warnline">The brief has changed since this count. Count again before pulling.</p>
            ) : alreadyPulled ? (
              <>
                {s.count.locations.map((c) => (
                  <div className="count" key={c.label}>
                    <span className="l">{c.label}</span>
                    <span className="n">{c.total.toLocaleString()}</span>
                    <span />
                  </div>
                ))}
                <p className="note">
                  Pulled from this count. To pull more, count again: people already found for this role are skipped, so
                  you only pay for new ones.
                </p>
              </>
            ) : (
              <>
                {s.count.locations.map((c) => (
                  <CountRow
                    key={c.label}
                    c={c}
                    value={picks[c.label] ?? 0}
                    disabled={busy || !!running}
                    onChange={(n) => {
                      setConfirming(false);
                      setPicks((p) => {
                        const next = { ...p };
                        if (n > 0) next[c.label] = n;
                        else delete next[c.label];
                        return next;
                      });
                    }}
                  />
                ))}
                <div className="bar">
                  <span className="total">
                    This month:{" "}
                    <strong>
                      {requested > 0
                        ? `${s.credits_this_month} → ${s.credits_this_month + requested} credits`
                        : `${s.credits_this_month} credits`}
                    </strong>
                  </span>
                  {confirming ? (
                    <button className="link-button" onClick={() => setConfirming(false)}>
                      Change my choices
                    </button>
                  ) : (
                    <button
                      className="btn-primary btn-inline"
                      onClick={pressPull}
                      disabled={busy || requested === 0 || !!s.blocked || !!running}
                    >
                      {pull.isPending
                        ? "Starting"
                        : requested === 0
                          ? "Choose how many to pull"
                          : `Pull ${people} · ${requested} credits`}
                    </button>
                  )}
                </div>
                {confirming ? (
                  <div className="confirm" role="group" aria-label="Confirm the pull">
                    <p>
                      Pull <strong>{people}</strong> for <strong>{requested} credits</strong>? This month goes from{" "}
                      {s.credits_this_month} to {s.credits_this_month + requested}.
                    </p>
                    <button className="btn-primary btn-inline" onClick={() => pull.mutate()} disabled={busy}>
                      {pull.isPending ? "Starting" : `Yes, pull ${people}`}
                    </button>
                  </div>
                ) : (
                  <p className="note">Over {CONFIRM_ABOVE} people asks you to confirm once more.</p>
                )}
                {pull.error && <ErrorLine e={pull.error} />}
              </>
            )}
          </section>
        )}

        {offerRoundTwo && !ask.data && (
          <section className="panel full r2">
            <h2 className="panel-title">
              {found === 0 ? "No one found. Try round 2" : `Only ${found} found. Try round 2`}
            </h2>
            <p>
              Claude reads this brief and the counts, says why so few were found, and suggests what to relax. Nothing is
              saved or searched until you agree.
            </p>
            {ask.isPending && (
              <p className="note" role="status">
                Claude is reading the search. This can take a minute.
              </p>
            )}
            <div className="bar r2-bar">
              <span className="total">
                Asking uses <strong>no search credits</strong>.
              </span>
              <button className="btn-primary btn-inline" onClick={() => ask.mutate()} disabled={ask.isPending}>
                {ask.isPending ? "Claude is reading" : "Ask Claude why"}
              </button>
            </div>
            {ask.error && <ErrorLine e={ask.error} />}
          </section>
        )}
        {offerRoundTwo && ask.data && (
          <RoundTwo
            p={ask.data}
            busy={busy || editMyself.isPending}
            blocked={!!s.blocked}
            agreeing={agree.isPending}
            onAgree={() => agree.mutate(ask.data)}
            onEdit={() => editMyself.mutate(ask.data)}
            onDismiss={() => ask.reset()}
            error={agree.error ?? editMyself.error}
          />
        )}

        {s.pull && <PullResult p={s.pull} roleId={id} />}
      </div>
    </main>
  );
}

function ErrorLine({ e }: { e: Error }) {
  return (
    <p className="form-error" role="alert">
      {e.message}
    </p>
  );
}

function Plan({ lines, lockedOut }: { lines: BriefLines; lockedOut: LockedOut[] }) {
  const required = lines.tools.filter((t) => t.status === "required").map((t) => t.name);
  const nice = lines.tools.filter((t) => t.status === "nice").map((t) => t.name);
  const must = lines.domains.filter((d) => d.weight === "must").map((d) => d.name);
  const plus = lines.domains.filter((d) => d.weight === "plus").map((d) => d.name);
  const where = lines.locations.length ? lines.locations.join(" · ") : "Anywhere (remote)";
  const locked = lockedOut.map((c) => `🔒 ${c.name}${c.hiring ? " (hiring client)" : ""}`);
  return (
    <>
      <div className="grp">Narrows the search</div>
      <dl className="q">
        {lines.titles.length > 0 && (
          <>
            <dt>Title (any)</dt>
            <dd>
              <b>{lines.titles.join(", ")}</b>
            </dd>
          </>
        )}
        <dt>Title has</dt>
        <dd>
          <b>{lines.levels.join(", ")}</b>
          {lines.excluded_titles.length > 0 && <>, not {lines.excluded_titles.join(", ")}</>}
        </dd>
        {lines.min_years !== null && lines.min_years > 0 && (
          <>
            <dt>Experience</dt>
            <dd>
              <b>{lines.min_years}+ years</b>
            </dd>
          </>
        )}
        {required.length > 0 && (
          <>
            <dt>Knows</dt>
            <dd>
              <b>{required.join(", ")}</b>
            </dd>
          </>
        )}
        {must.length > 0 && (
          <>
            <dt>Domain (any)</dt>
            <dd>
              <b>{must.join(", ")}</b>
            </dd>
          </>
        )}
        <dt>Where</dt>
        <dd>
          <b>{where}</b>
          {lines.locations.length > 1 && " (each its own search)"}
        </dd>
        <dt>Employers</dt>
        <dd>{lines.employer_types.join(", ")}</dd>
        <dt>Left out</dt>
        <dd>{[...locked, ...lines.leave_out].join(", ")}</dd>
      </dl>
      <div className="grp dim">Only ranks</div>
      <dl className="q">
        <dt>Must-haves</dt>
        <dd>{lines.must_haves.join(", ")}</dd>
        {lines.capabilities.length > 0 && (
          <>
            <dt>Capabilities</dt>
            <dd>{lines.capabilities.join(", ")}</dd>
          </>
        )}
        {lines.frameworks.length + lines.certifications.length > 0 && (
          <>
            <dt>Standards</dt>
            <dd>{[...lines.frameworks, ...lines.certifications].join(", ")}</dd>
          </>
        )}
        {nice.length + plus.length > 0 && (
          <>
            <dt>Nice to have</dt>
            <dd>{[...nice, ...plus].join(" · ")}</dd>
          </>
        )}
      </dl>
    </>
  );
}

function CountRow({
  c,
  value,
  disabled,
  onChange,
}: {
  c: CountLocation;
  value: number;
  disabled: boolean;
  onChange: (n: number) => void;
}) {
  const choices = choicesFor(c.total);
  return (
    <>
      <div className="count">
        <span className="l">{c.label}</span>
        <span className="n">{c.total.toLocaleString()}</span>
        <span className="seg pick" role="radiogroup" aria-label={`How many to pull in ${c.label}`}>
          <button
            role="radio"
            aria-checked={value === 0}
            className={value === 0 ? "sel" : undefined}
            onClick={() => onChange(0)}
            disabled={disabled}
          >
            Skip
          </button>
          {choices.map((n) => (
            <button
              key={n}
              role="radio"
              aria-checked={value === n}
              className={value === n ? "sel" : undefined}
              onClick={() => onChange(n)}
              disabled={disabled}
            >
              {n === c.total && n <= SIZES[0] ? `All ${n}` : n}
            </button>
          ))}
        </span>
      </div>
      {c.total === 0 && <p className="note">No one matched here.</p>}
      {c.total > 0 && c.total <= SIZES[0] && (
        <p className="note">
          {c.label} is small, so all {c.total} is the only size.
        </p>
      )}
    </>
  );
}

/** Claude's round 2: why so few, what it would relax, and the choice. */
function RoundTwo({
  p,
  busy,
  blocked,
  agreeing,
  onAgree,
  onEdit,
  onDismiss,
  error,
}: {
  p: RetuneView;
  busy: boolean;
  blocked: boolean;
  agreeing: boolean;
  onAgree: () => void;
  onEdit: () => void;
  onDismiss: () => void;
  error: Error | null;
}) {
  const searches = Math.max(1, p.lines.locations.length);
  const creditWord = `${searches} credit${searches === 1 ? "" : "s"}`;
  return (
    <section className="panel full r2" aria-labelledby="r2-title">
      <h2 className="panel-title" id="r2-title">
        Round 2 from Claude
      </h2>
      <div className="read">
        <div className="lbl">Why so few</div>
        <p>{p.diagnosis}</p>
      </div>
      <div className="lbl">What changes</div>
      <ul className="r2-list">
        {p.changes.map((c) => (
          <li key={c.line} className="r2-row">
            <span className="k">{c.line}</span>
            <span>
              <span className="now">{c.after}</span>
              <span className="was">was {c.before}</span>
            </span>
            {c.reason && <span className="rsn">{c.reason}</span>}
          </li>
        ))}
      </ul>
      <p className="note">Everything relaxed still counts when Claude ranks the people found.</p>
      <div className="bar r2-bar">
        <span className="total">
          Agreeing confirms this as brief version {p.based_on + 1} and counts it.
        </span>
        <span className="actions-row">
          <button className="link-button" onClick={onDismiss} disabled={busy}>
            Not now
          </button>
          <button className="btn-ghost" onClick={onEdit} disabled={busy}>
            Edit it myself
          </button>
          <button className="btn-primary btn-inline" onClick={onAgree} disabled={busy || blocked}>
            {agreeing ? "Counting" : `Agree and count · ${creditWord}`}
          </button>
        </span>
      </div>
      {error && <ErrorLine e={error} />}
    </section>
  );
}

function PullResult({ p, roleId }: { p: PullView; roleId: string }) {
  if (!p.done) {
    const pct = Math.round((100 * p.locations_done) / Math.max(1, p.locations));
    return (
      <section className="panel full" aria-live="polite">
        <h2 className="panel-title">
          Pulling {p.requested} {p.requested === 1 ? "person" : "people"}
        </h2>
        <div className="prog" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
          <i style={{ width: `${pct}%` }} />
        </div>
        <p className="note">
          {p.locations_done} of {p.locations} location
          {p.locations === 1 ? "" : "s"} done. You can leave this page; it carries on and shows here when done.
        </p>
      </section>
    );
  }
  return (
    <section className="panel full">
      <h2 className="panel-title">Search done</h2>
      {p.failed && <Failed p={p} />}
      <div className="stats">
        <div className="stat">
          <div className="v">{p.pulled}</div>
          <div className="k">
            Pulled · {p.credits_used} credit{p.credits_used === 1 ? "" : "s"} used in total
          </div>
        </div>
        <div className="stat">
          <div className="v">{p.new_candidates}</div>
          <div className="k">
            New for this role
            {p.already > 0 ? ` · ${p.already} ${p.already === 1 ? "was" : "were"} already on it` : ""}
          </div>
        </div>
        <div className="stat">
          <div className="v">{p.left_out}</div>
          <div className="k">Removed after pulling: current job matched a locked-out company</div>
        </div>
        <div className={`stat${p.unknown_employer > 0 ? " warn" : ""}`}>
          <div className="v">{p.unknown_employer}</div>
          <div className="k">No known employer: check before any contact</div>
        </div>
      </div>
      <div className="bar">
        <span className="total">Claude ranks the new people in about a minute.</span>
        <Link className="btn-primary btn-inline" to={`/brief/${roleId}/candidates`}>
          See the candidates
        </Link>
      </div>
    </section>
  );
}

/** Says which location failed and what its credits paid for. */
function Failed({ p }: { p: PullView }) {
  const where = p.failed_locations.join(" and ") || "One location";
  const saved = p.credits_used - p.credits_unsaved;
  return (
    <div className="failbox" role="alert">
      <strong>{where} could not be pulled after several tries.</strong>
      <p>
        {p.credits_used} credits in total: {saved} for the {p.pulled} people pulled
        {p.credits_unsaved > 0
          ? `, and ${p.credits_unsaved} charged for ${where} before it failed. Those people were not saved.`
          : `. Nothing was charged for ${where}.`}
      </p>
      <p>To retry, count again above. People already found for this role are skipped, so you only pay for new ones.</p>
    </div>
  );
}
