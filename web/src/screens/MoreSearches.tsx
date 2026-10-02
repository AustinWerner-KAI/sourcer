import { useEffect, useRef, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { api, keepKey } from "../api/client";
import type { BriefLines } from "../api/types/BriefLines";
import type { MoreSearch } from "../api/types/MoreSearch";
import type { SearchState } from "../api/types/SearchState";
import type { Widen } from "../api/types/Widen";
import { EMPLOYER_OPTIONS } from "./BriefEditor";
import { ErrorLine, FEW, useIntentKey } from "./Search";

/** Pulls bigger than this ask for a second click (the server checks too). */
const CONFIRM_ABOVE = 50;
const SIZES = [25, 50, 100];
/** Most one place can give in one pull. */
const PAGE = 100;
const CLAUDE_SLOTS = [1, 2];
const OWN_SLOT = 3;
const OWN_NOTE = "Start from the brief and add titles, levels, places or employers. The brief itself stays as it is.";

const credits = (n: number) => `${n} credit${n === 1 ? "" : "s"}`;
const people = (n: number) => `${n} ${n === 1 ? "person" : "people"}`;

const noWiden: Widen = {
  add_titles: [],
  add_levels: [],
  add_locations: [],
  add_employer_types: [],
  tools_to_nice: [],
  domains_to_plus: [],
  min_years: null,
};

/** One thing a search adds to the brief, and the search without it. */
type Addition = { key: string; label: string; without: Widen };

function additions(w: Widen): Addition[] {
  const out: Addition[] = [];
  const list = (field: keyof Omit<Widen, "min_years">, label: (v: string) => string) =>
    w[field].forEach((v) =>
      out.push({
        key: `${field}:${v}`,
        label: label(v),
        without: { ...w, [field]: w[field].filter((x) => x !== v) },
      }),
    );
  list("add_titles", (v) => `+ ${v}`);
  list("add_levels", (v) => `+ ${v}`);
  list("add_locations", (v) => `+ ${v}`);
  list("add_employer_types", (v) => `+ ${v}`);
  list("tools_to_nice", (v) => `${v} optional`);
  list("domains_to_plus", (v) => `${v} optional`);
  if (w.min_years !== null)
    out.push({
      key: "years",
      label: w.min_years > 0 ? `${w.min_years}+ years` : "Any years",
      without: { ...w, min_years: null },
    });
  return out;
}

/** What one search can give in one pull: at most a page per place. */
function most(s: MoreSearch) {
  return (s.count?.locations ?? []).reduce((a, c) => a + Math.min(Math.max(c.total, 0), PAGE), 0);
}

function sizesFor(cap: number): number[] {
  if (cap <= 0) return [];
  if (cap <= SIZES[0]) return [cap];
  const out = SIZES.filter((n) => n <= cap);
  if (cap <= PAGE && !out.includes(cap)) out.push(cap);
  return out;
}

/**
 * Up to three wider searches beside the brief: Claude's two picks and the
 * resourcer's own. Everyone found joins one list, ranked against the brief.
 */
export function MoreSearches({
  roleId,
  s,
  lines,
  found,
  running,
  onState,
}: {
  roleId: string;
  s: SearchState;
  lines: BriefLines;
  found: number;
  running: boolean;
  onState: (s: SearchState) => void;
}) {
  const bySlot = (slot: number) => s.more.find((m) => m.slot === slot);
  const [picks, setPicks] = useState<Record<number, number>>({});
  const [confirming, setConfirming] = useState(false);
  const [editing, setEditing] = useState(false);
  const pullKey = useIntentKey();
  const countKeys = useRef<Record<number, string>>({});

  // A new count of any search starts a fresh set of choices.
  const counts = s.more.map((m) => `${m.slot}:${m.count?.id ?? ""}:${m.count?.pulled}:${m.version}`).join(",");
  useEffect(() => {
    setPicks({});
    setConfirming(false);
  }, [counts]);

  const suggest = useMutation({ mutationFn: () => api.suggestSearches(roleId), onSuccess: onState });
  // Claude chooses once, as soon as the brief has been counted. If only one
  // of its picks was usable, asking again is left to the resourcer.
  const asked = useRef<number | null>(null);
  const none = CLAUDE_SLOTS.every((n) => !bySlot(n));
  useEffect(() => {
    if (none && !s.blocked && asked.current !== s.brief_version) {
      asked.current = s.brief_version;
      suggest.mutate();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [none, s.blocked, s.brief_version]);

  const save = useMutation({
    mutationFn: ({ slot, widen }: { slot: number; widen: Widen }) => api.saveSearch(roleId, slot, widen),
    onSuccess: (st) => {
      onState(st);
      setEditing(false);
    },
  });
  const count = useMutation({
    mutationFn: (slot: number) => {
      countKeys.current[slot] ??= crypto.randomUUID();
      return api.countSearch(roleId, slot, countKeys.current[slot]);
    },
    onSuccess: (st, slot) => {
      delete countKeys.current[slot];
      onState(st);
    },
    // A dropped or still-running count keeps its key, so a retry never pays twice.
    onError: (e, slot) => {
      if (!keepKey(e)) delete countKeys.current[slot];
    },
  });
  const chosen = s.more.filter((m) => (picks[m.slot] ?? 0) > 0 && m.count && !m.count.stale && !m.count.pulled);
  const requested = chosen.reduce((a, m) => a + picks[m.slot], 0);
  const pull = useMutation({
    mutationFn: () =>
      api.pullSearches(roleId, {
        picks: chosen.map((m) => ({ slot: m.slot, count_id: m.count!.id, size: picks[m.slot] })),
        confirmed: confirming,
        key: pullKey.get(),
      }),
    onSuccess: (st) => {
      pullKey.settle();
      onState(st);
      setConfirming(false);
    },
    onError: (e) => pullKey.settle(e),
  });

  const busy = save.isPending || count.isPending || pull.isPending || running || !!s.blocked;
  const pressPull = () => (requested > CONFIRM_ABOVE ? setConfirming(true) : pull.mutate());

  const card = (slot: number) => {
    const m = bySlot(slot);
    if (!m) {
      if (slot === OWN_SLOT)
        return (
          <div className="opt" key={slot}>
            <div className="lbl mine">Your own</div>
            <h3>Your search</h3>
            <p>{OWN_NOTE}</p>
            {editing ? (
              <OwnEditor
                lines={lines}
                start={noWiden}
                saving={save.isPending}
                error={save.error}
                onSave={(w) => save.mutate({ slot, widen: w })}
                onCancel={() => setEditing(false)}
              />
            ) : (
              <div className="foot">
                <span className="found">Not set up</span>
                <button className="btn-ghost" onClick={() => setEditing(true)} disabled={save.isPending}>
                  Set up
                </button>
              </div>
            )}
          </div>
        );
      return (
        <div className="opt" key={slot} aria-busy={suggest.isPending}>
          <div className="lbl">Claude's pick</div>
          {suggest.isPending ? (
            <p role="status">Claude is choosing a wider search. This can take a minute.</p>
          ) : (
            <div className="foot">
              <span className="found">{suggest.error ? suggest.error.message : "No search chosen yet."}</span>
              <button className="btn-ghost" onClick={() => suggest.mutate()}>
                Ask Claude
              </button>
            </div>
          )}
        </div>
      );
    }
    const adds = additions(m.widen);
    const c = m.count && !m.count.stale ? m.count : null;
    const newPeople = c ? c.locations.reduce((a, l) => a + l.total, 0) : 0;
    const cap = most(m);
    const value = picks[m.slot] ?? 0;
    const countButton = (again: boolean) => (
      <button className="btn-ghost" onClick={() => count.mutate(m.slot)} disabled={busy}>
        {count.isPending && count.variables === m.slot
          ? "Counting"
          : `${again ? "Count again" : "Count"} · ${credits(m.locations.length)}`}
      </button>
    );
    const editingThis = editing && m.slot === OWN_SLOT;
    return (
      <div className={`opt${c && !c.pulled && newPeople > 0 ? " on" : ""}`} key={slot}>
        <div className={`lbl${m.by_claude ? "" : " mine"}`}>{m.by_claude ? "Claude's pick" : "Your own"}</div>
        <h3>{m.name}</h3>
        <p>{m.note || (m.by_claude ? "" : OWN_NOTE)}</p>
        {editingThis ? (
          <OwnEditor
            lines={lines}
            start={m.widen}
            saving={save.isPending}
            error={save.error}
            onSave={(w) => save.mutate({ slot: m.slot, widen: w })}
            onCancel={() => setEditing(false)}
          />
        ) : (
          <>
            <div className="chg">
              {adds.map((a) => (
                <span className="chip yes" key={a.key}>
                  {a.label}
                  {adds.length > 1 && (
                    <button
                      className="x"
                      aria-label={`Remove ${a.label}`}
                      onClick={() => save.mutate({ slot: m.slot, widen: a.without })}
                      disabled={busy}
                    >
                      ×
                    </button>
                  )}
                </span>
              ))}
            </div>
            {!m.by_claude && (
              <button className="link-button" onClick={() => setEditing(true)} disabled={busy}>
                Change
              </button>
            )}
            <div className="foot">
              {!c ? (
                <>
                  <span className="found">{m.count ? "Changed since it was counted." : "Not counted yet"}</span>
                  {countButton(false)}
                </>
              ) : c.pulled ? (
                <>
                  <span className="found">Pulled from this count. Count again to find more new people.</span>
                  {countButton(true)}
                </>
              ) : newPeople === 0 ? (
                <>
                  <span className="found">No new people. Remove an addition or try another search.</span>
                  {countButton(true)}
                </>
              ) : (
                <>
                  <div>
                    <span className="n">{newPeople.toLocaleString()}</span>{" "}
                    <span className="found">new, not already on your list</span>
                  </div>
                  <span className="seg pick" role="radiogroup" aria-label={`How many to pull from ${m.name}`}>
                    {[0, ...sizesFor(cap)].map((n) => (
                      <button
                        key={n}
                        role="radio"
                        aria-checked={value === n}
                        className={value === n ? "sel" : undefined}
                        onClick={() => {
                          setConfirming(false);
                          setPicks((p) => ({ ...p, [m.slot]: n }));
                        }}
                        disabled={busy}
                      >
                        {n === 0 ? "None" : n === cap && n <= PAGE ? `All ${n}` : n}
                      </button>
                    ))}
                  </span>
                </>
              )}
            </div>
          </>
        )}
        {save.error && save.variables?.slot === m.slot && !editingThis && <ErrorLine e={save.error} />}
        {count.error && count.variables === m.slot && <ErrorLine e={count.error} />}
      </div>
    );
  };

  return (
    <section className="panel full more" aria-labelledby="more-title">
      <h2 className="panel-title" id="more-title">
        More searches <span className="c">up to 3</span>
      </h2>
      <p className="panel-note">
        Everyone found joins the same candidate list, ranked against the brief. People already found are left out, so
        you never pay twice.
      </p>
      {found < FEW && (
        <p className="warnline">
          {found === 0 ? "The brief found no one." : `The brief found only ${found}.`} Claude suggests two wider searches
          below, or set up your own.
        </p>
      )}
      <div className="opts">{[...CLAUDE_SLOTS, OWN_SLOT].map(card)}</div>
      {requested > 0 && (
        <>
          <div className="bar">
            <span className="sum">
              <span className="from">{chosen.map((m) => `${picks[m.slot]} from ${m.name}`).join(" · ")}</span>
              <span className="total">
                This month:{" "}
                <strong>
                  {s.credits_this_month} → {s.credits_this_month + requested} credits
                </strong>
              </span>
            </span>
            {confirming ? (
              <button className="link-button" onClick={() => setConfirming(false)}>
                Change my choices
              </button>
            ) : (
              <button className="btn-primary btn-inline" onClick={pressPull} disabled={busy}>
                {pull.isPending ? "Starting" : `Pull ${people(requested)} · ${credits(requested)}`}
              </button>
            )}
          </div>
          {confirming && (
            <div className="confirm" role="group" aria-label="Confirm the pull">
              <p>
                Pull <strong>{people(requested)}</strong> for <strong>{credits(requested)}</strong>? This month goes
                from {s.credits_this_month} to {s.credits_this_month + requested}.
              </p>
              <button className="btn-primary btn-inline" onClick={() => pull.mutate()} disabled={busy}>
                {pull.isPending ? "Starting" : `Yes, pull ${people(requested)}`}
              </button>
            </div>
          )}
        </>
      )}
      {pull.error && <ErrorLine e={pull.error} />}
    </section>
  );
}

/** Comma-separated words to a clean list. */
const words = (t: string) =>
  t
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean);

/** The resourcer's own search: what to add to the brief. */
function OwnEditor({
  lines,
  start,
  saving,
  error,
  onSave,
  onCancel,
}: {
  lines: BriefLines;
  start: Widen;
  saving: boolean;
  error: Error | null;
  onSave: (w: Widen) => void;
  onCancel: () => void;
}) {
  const [titles, setTitles] = useState(start.add_titles.join(", "));
  const [levels, setLevels] = useState(start.add_levels.join(", "));
  const [places, setPlaces] = useState(start.add_locations.join(", "));
  const [employers, setEmployers] = useState<string[]>(start.add_employer_types);
  const [optional, setOptional] = useState<string[]>(start.tools_to_nice);
  const [years, setYears] = useState<number | null>(start.min_years);
  const required = lines.tools.filter((t) => t.status === "required").map((t) => t.name);
  const otherEmployers = EMPLOYER_OPTIONS.filter((t) => !lines.employer_types.includes(t));
  const fewer = lines.min_years && lines.min_years > 0 ? [0, ...Array.from({ length: lines.min_years - 1 }, (_, i) => i + 1)] : [];
  const toggle = (list: string[], v: string) => (list.includes(v) ? list.filter((x) => x !== v) : [...list, v]);
  const widen: Widen = {
    ...noWiden,
    domains_to_plus: start.domains_to_plus,
    add_titles: words(titles),
    add_levels: words(levels),
    add_locations: lines.locations.length ? words(places) : [],
    add_employer_types: employers,
    tools_to_nice: optional,
    min_years: years,
  };
  return (
    <form
      className="own"
      onSubmit={(e) => {
        e.preventDefault();
        onSave(widen);
      }}
    >
      <label>
        Add job titles
        <input className="in" value={titles} onChange={(e) => setTitles(e.target.value)} placeholder="e.g. Platform Security Engineer" />
      </label>
      <label>
        Add levels
        <input className="in" value={levels} onChange={(e) => setLevels(e.target.value)} placeholder="e.g. Principal, Staff" />
      </label>
      {lines.locations.length > 0 && (
        <label>
          Add places
          <input className="in" value={places} onChange={(e) => setPlaces(e.target.value)} placeholder="e.g. Riyadh, Doha" />
        </label>
      )}
      {lines.employer_types.length > 0 && otherEmployers.length > 0 && (
        <fieldset>
          <legend>Add employers</legend>
          <div className="chg">
            {otherEmployers.map((t) => (
              <button
                type="button"
                key={t}
                className={`chip${employers.includes(t) ? " yes" : ""}`}
                aria-pressed={employers.includes(t)}
                onClick={() => setEmployers((x) => toggle(x, t))}
              >
                {t}
              </button>
            ))}
          </div>
        </fieldset>
      )}
      {required.length > 0 && (
        <fieldset>
          <legend>Make optional</legend>
          <div className="chg">
            {required.map((t) => (
              <button
                type="button"
                key={t}
                className={`chip${optional.includes(t) ? " yes" : ""}`}
                aria-pressed={optional.includes(t)}
                onClick={() => setOptional((x) => toggle(x, t))}
              >
                {t}
              </button>
            ))}
          </div>
        </fieldset>
      )}
      {fewer.length > 0 && (
        <label>
          Fewest years
          <select className="in" value={years ?? ""} onChange={(e) => setYears(e.target.value === "" ? null : Number(e.target.value))}>
            <option value="">{lines.min_years}+, as the brief</option>
            {fewer.map((y) => (
              <option key={y} value={y}>
                {y === 0 ? "Any" : `${y}+`}
              </option>
            ))}
          </select>
        </label>
      )}
      <div className="actions-row">
        <button type="button" className="link-button" onClick={onCancel}>
          Cancel
        </button>
        <button type="submit" className="btn-ghost" disabled={saving}>
          {saving ? "Saving" : "Save"}
        </button>
      </div>
      {error && <ErrorLine e={error} />}
    </form>
  );
}
