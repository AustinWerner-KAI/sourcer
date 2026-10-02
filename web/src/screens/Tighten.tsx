import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { api, Refused } from "../api/client";
import type { SearchState } from "../api/types/SearchState";
import type { TightenMove } from "../api/types/TightenMove";
import type { TightenView } from "../api/types/TightenView";
import { ErrorLine } from "./Search";

/** A count above this is too many to pull well (the server checks too). */
export const TOO_MANY = 300;
/** Tightenings in a row before the resourcer edits the brief themselves. */
const MAX_ROUNDS = 2;

const credits = (n: number) => `${n} credit${n === 1 ? "" : "s"}`;

/** Places the tightened brief will count, so the button says what it costs. */
function placesAfter(s: SearchState, moves: TightenMove[]) {
  const set = moves.find((m) => m.kind === "set_locations");
  const cities = set ? new Set(set.value.split(",").map((c) => c.trim().toLowerCase()).filter(Boolean)) : null;
  const base = cities ? Math.min(cities.size, 10) : s.locations.length;
  return Math.max(1, base - moves.filter((m) => m.kind === "drop_location").length);
}

/**
 * Too many found: Claude tightens the brief towards the spec, a change or two
 * a round, quoting the spec for each. The resourcer keeps the changes they
 * agree with; those are confirmed as the next brief version and counted.
 */
export function Tighten({
  roleId,
  s,
  found,
  onState,
  onApplied,
}: {
  roleId: string;
  s: SearchState;
  found: number;
  onState: (s: SearchState) => void;
  onApplied: (s: SearchState, before: number, version: number) => void;
}) {
  const [keep, setKeep] = useState<boolean[]>([]);
  const ask = useMutation({
    mutationFn: () => api.tighten(roleId),
    onSuccess: (t) => setKeep(t.moves.map(() => true)),
  });
  // Agreeing confirms the kept changes; the screen then counts the new
  // version with its own Count step, so a failed count shows there.
  const agree = useMutation({
    mutationFn: (t: TightenView) =>
      api.applyTighten(roleId, { based_on: t.based_on, moves: t.moves.filter((_, i) => keep[i]) }),
    onSuccess: (st, t) => onApplied(st, t.before, t.based_on + 1),
    // The brief may have moved on (another window, or a lost reply): show it as it is.
    onError: async (e) => {
      if (e instanceof Refused && e.status === 409) onState(await api.search(roleId));
    },
  });

  if (s.tighten_round >= MAX_ROUNDS)
    return (
      <section className="panel full r3">
        <h2 className="panel-title">Still {found.toLocaleString()} found</h2>
        <p className="note">Tightened twice already. To go further, edit the brief yourself.</p>
      </section>
    );

  const t = ask.data;
  if (!t)
    return (
      <section className="panel full r3">
        <h2 className="panel-title">{found.toLocaleString()} found. Too many to pull well</h2>
        <p>
          People Data Labs returns its first people, not its best. Claude tightens the brief towards the spec, a change
          or two at a time, and quotes the spec for each. Nothing changes until you agree. The aim is about 100.
        </p>
        {ask.isPending && (
          <p className="note" role="status">
            Claude is reading the spec and the search. This can take a minute.
          </p>
        )}
        <div className="bar">
          <span className="total">
            Asking uses <strong>no search credits</strong>.
          </span>
          <button className="btn-primary btn-inline" onClick={() => ask.mutate()} disabled={ask.isPending}>
            {ask.isPending ? "Claude is reading" : "Ask Claude to tighten"}
          </button>
        </div>
        {ask.error && <ErrorLine e={ask.error} />}
      </section>
    );

  const kept = t.moves.filter((_, i) => keep[i]);
  const places = placesAfter(s, kept);
  return (
    <section className="panel full r3" aria-labelledby="r3-title">
      <h2 className="panel-title" id="r3-title">
        Tighten towards the spec <span className="c">round {t.round} of {MAX_ROUNDS}</span>
      </h2>
      <div className="read">
        <div className="lbl">Why so many</div>
        <p>{t.why}</p>
      </div>
      <div className="lbl">Changes from the spec</div>
      <ul className="r3-list">
        {t.moves.map((m, i) => (
          <li key={`${m.kind}:${m.value}`}>
            <label>
              <input
                type="checkbox"
                checked={keep[i] ?? false}
                onChange={() => setKeep((k) => k.map((x, j) => (j === i ? !x : x)))}
                disabled={agree.isPending}
              />
              <span>
                <span className="now">{m.label}</span>
                <q>{m.quote}</q>
              </span>
            </label>
          </li>
        ))}
      </ul>
      <p className="note">
        Untick any you disagree with. The brief&apos;s leave-out list, excluded titles and must-haves never change.
      </p>
      <div className="bar">
        <span className="total">
          Now {t.before.toLocaleString()}. Agreeing confirms brief version {t.based_on + 1} and counts it.
        </span>
        <span className="actions-row">
          <button className="link-button" onClick={() => ask.reset()} disabled={agree.isPending}>
            Not now
          </button>
          <button
            className="btn-primary btn-inline"
            onClick={() => agree.mutate(t)}
            disabled={agree.isPending || kept.length === 0 || !!s.blocked}
          >
            {agree.isPending ? "Saving" : `Agree and count · ${credits(places)}`}
          </button>
        </span>
      </div>
      {agree.error && <ErrorLine e={agree.error} />}
    </section>
  );
}
