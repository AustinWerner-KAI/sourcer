import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../api/client";
import type { CandidateRow } from "../api/types/CandidateRow";
import type { CvAssessment } from "../api/types/CvAssessment";
import type { CvView } from "../api/types/CvView";
import type { FeedbackVerdict } from "../api/types/FeedbackVerdict";
import type { RoleChoice } from "../api/types/RoleChoice";
import { ago } from "./Recruitly";

/** Other roles assessed in one go, besides this one. */
const MAX_OTHER_ROLES = 3;

/** The CV line on a candidate card: its score once assessed, and the full assessment. */
export function CvLine({ p, roleId }: { p: CandidateRow; roleId: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="cvline">
      <button type="button" className="link-button cvtoggle" aria-expanded={open} onClick={() => setOpen(!open)}>
        {p.cv_score !== null ? (
          <>
            CV <b>{p.cv_score}/10</b> · {open ? "Hide assessment" : "Show assessment"}
          </>
        ) : open ? (
          "Close"
        ) : (
          "Add CV"
        )}
      </button>
      {open && <CvPanel candidacy={p.id} roleId={roleId} />}
    </div>
  );
}

function CvPanel({ candidacy, roleId }: { candidacy: string; roleId: string }) {
  const q = useQuery({ queryKey: ["cv", candidacy], queryFn: () => api.cv(candidacy) });
  if (q.isLoading) return <p className="note">Loading</p>;
  if (!q.data)
    return (
      <p className="form-error" role="alert">
        Could not load the CV.
      </p>
    );
  const v = q.data;
  if (!v.cv) return <Upload candidacy={candidacy} roleId={roleId} roles={v.roles} />;
  return <Assessed candidacy={candidacy} roleId={roleId} v={v} />;
}

/** Keeps the card and the panel in step after any change. */
function useSaved(candidacy: string, roleId: string) {
  const queryClient = useQueryClient();
  return (v: CvView) => {
    queryClient.setQueryData(["cv", candidacy], v);
    queryClient.invalidateQueries({ queryKey: ["candidates", roleId] });
  };
}

function RolePicker({
  roles,
  picked,
  setPicked,
  legend,
}: {
  roles: RoleChoice[];
  picked: string[];
  setPicked: (ids: string[]) => void;
  legend: string;
}) {
  if (roles.length === 0) return null;
  const full = picked.length >= MAX_OTHER_ROLES;
  return (
    <fieldset className="cvroles">
      <legend>{legend}</legend>
      {roles.map((r) => {
        const on = picked.includes(r.id);
        return (
          <label key={r.id} className="check">
            <input
              type="checkbox"
              checked={on}
              disabled={!on && full}
              onChange={() => setPicked(on ? picked.filter((x) => x !== r.id) : [...picked, r.id])}
            />
            {r.title}
            {r.client_name && <span className="dim"> · {r.client_name}</span>}
          </label>
        );
      })}
      {full && <p className="hint">Up to {MAX_OTHER_ROLES} other roles at once.</p>}
    </fieldset>
  );
}

function Upload({
  candidacy,
  roleId,
  roles,
  newer,
  onCancel,
}: {
  candidacy: string;
  roleId: string;
  roles: RoleChoice[];
  newer?: boolean;
  onCancel?: () => void;
}) {
  const saved = useSaved(candidacy, roleId);
  const [file, setFile] = useState<File | null>(null);
  const [others, setOthers] = useState<string[]>([]);
  const up = useMutation({
    mutationFn: () => api.uploadCv(candidacy, file as File, others),
    onSuccess: (v) => {
      saved(v);
      onCancel?.();
    },
  });
  return (
    <div className="cvup">
      <label className="f">
        {newer ? "Newer CV" : "CV"} (PDF, Word or text)
        <input
          className="in"
          type="file"
          accept=".pdf,.docx,.txt,application/pdf,application/vnd.openxmlformats-officedocument.wordprocessingml.document,text/plain"
          onChange={(e) => setFile(e.target.files?.[0] ?? null)}
        />
      </label>
      <RolePicker roles={roles} picked={others} setPicked={setOthers} legend="Also assess for" />
      <p className="hint">
        Claude scores it out of 10 for this role and any you tick. The name, emails, phone numbers and links are removed
        before Claude reads it, and the file itself is not kept.
      </p>
      <div className="actions-row">
        <button type="button" className="btn-ghost" disabled={!file || up.isPending} onClick={() => up.mutate()}>
          {up.isPending ? "Claude is reading the CV" : "Assess CV"}
        </button>
        {onCancel && (
          <button type="button" className="link-button" onClick={onCancel}>
            Cancel
          </button>
        )}
        {up.isPending && <span className="note">This takes about a minute.</span>}
      </div>
      {up.error && (
        <p className="form-error" role="alert">
          {up.error.message}
        </p>
      )}
    </div>
  );
}

function Assessed({ candidacy, roleId, v }: { candidacy: string; roleId: string; v: CvView }) {
  const cv = v.cv!;
  const saved = useSaved(candidacy, roleId);
  const [mode, setMode] = useState<"none" | "more" | "newer">("none");
  const [more, setMore] = useState<string[]>([]);
  const assessed = new Set(cv.assessments.map((a) => a.role_id));
  const left = v.roles.filter((r) => !assessed.has(r.id));
  const assess = useMutation({
    mutationFn: () => api.assessCv(candidacy, more),
    onSuccess: (x) => {
      saved(x);
      setMode("none");
      setMore([]);
    },
  });
  const note = useMutation({ mutationFn: () => api.cvToRecruitly(candidacy), onSuccess: saved });

  return (
    <div className="cvpanel">
      <p className="meta">
        {cv.file_name} · assessed {ago(cv.uploaded_at)}
        {cv.uploaded_by ? ` by ${cv.uploaded_by}` : ""}
      </p>
      {cv.assessments.map((a) => (
        <Assessment key={a.id} a={a} candidacy={candidacy} />
      ))}
      {cv.call && (
        <div className="read cvcall">
          <div className="lbl">Claude's call</div>
          <p>{cv.call}</p>
        </div>
      )}
      <div className="actions-row cvacts">
        {v.recruitly &&
          (cv.recruitly_noted ? (
            <span className="flag sent">Added to Recruitly notes {cv.recruitly_noted}</span>
          ) : (
            <button
              type="button"
              className="btn-ghost"
              onClick={() => note.mutate()}
              disabled={note.isPending || !v.in_recruitly}
              title={v.in_recruitly ? undefined : "Add them to Recruitly first"}
            >
              {note.isPending ? "Adding" : "Add to Recruitly notes"}
            </button>
          ))}
        {left.length > 0 && mode === "none" && (
          <button type="button" className="link-button" onClick={() => setMode("more")}>
            Assess for another role
          </button>
        )}
        {mode === "none" && (
          <button type="button" className="link-button" onClick={() => setMode("newer")}>
            Upload a newer CV
          </button>
        )}
      </div>
      {v.recruitly && !v.in_recruitly && !cv.recruitly_noted && (
        <p className="hint">Not in Recruitly yet. Add them to Recruitly first, then add the assessment as a note.</p>
      )}
      {note.error && (
        <p className="form-error" role="alert">
          {note.error.message}
        </p>
      )}
      {mode === "more" && (
        <div className="cvup">
          <RolePicker roles={left} picked={more} setPicked={setMore} legend="Assess this CV for" />
          <div className="actions-row">
            <button
              type="button"
              className="btn-ghost"
              disabled={more.length === 0 || assess.isPending}
              onClick={() => assess.mutate()}
            >
              {assess.isPending ? "Claude is reading the CV" : "Assess"}
            </button>
            <button type="button" className="link-button" onClick={() => setMode("none")}>
              Cancel
            </button>
          </div>
          {assess.error && (
            <p className="form-error" role="alert">
              {assess.error.message}
            </p>
          )}
        </div>
      )}
      {mode === "newer" && (
        <Upload candidacy={candidacy} roleId={roleId} roles={v.roles} newer onCancel={() => setMode("none")} />
      )}
    </div>
  );
}

const VERDICTS: { v: FeedbackVerdict; label: string }[] = [
  { v: "accurate", label: "Accurate" },
  { v: "too_high", label: "Too high" },
  { v: "too_low", label: "Too low" },
];

function List({ label, items, cls }: { label: string; items: string[]; cls: string }) {
  if (items.length === 0) return null;
  return (
    <div className={`cvsec ${cls}`}>
      <div className="lbl">{label}</div>
      <ul>
        {items.map((x, i) => (
          <li key={i}>{x}</li>
        ))}
      </ul>
    </div>
  );
}

/** One role's assessment, with the resourcer's verdict on it. */
function Assessment({ a, candidacy }: { a: CvAssessment; candidacy: string }) {
  const queryClient = useQueryClient();
  const fb = a.feedback;
  const [editing, setEditing] = useState(!fb);
  const [verdict, setVerdict] = useState<FeedbackVerdict | null>(fb?.verdict ?? null);
  const [mine, setMine] = useState<number | null>(fb?.your_score ?? null);
  const [why, setWhy] = useState(fb?.note ?? "");
  const [vital, setVital] = useState<number[]>(fb?.vital ?? []);
  const save = useMutation({
    mutationFn: () =>
      api.cvFeedback(a.id, {
        verdict: verdict as FeedbackVerdict,
        your_score: verdict === "accurate" ? null : mine,
        note: why,
        vital,
      }),
    onSuccess: () => {
      setEditing(false);
      queryClient.invalidateQueries({ queryKey: ["cv", candidacy] });
    },
  });
  // Your own score must agree with the verdict: under Claude's for too high, over for too low.
  const range =
    verdict === "too_high"
      ? Array.from({ length: a.score - 1 }, (_, i) => i + 1)
      : verdict === "too_low"
        ? Array.from({ length: 10 - a.score }, (_, i) => a.score + 1 + i)
        : [];
  const pick = (v: FeedbackVerdict) => {
    setVerdict(v);
    setMine(null);
  };
  const star = (i: number) => setVital(vital.includes(i) ? vital.filter((x) => x !== i) : [...vital, i]);
  const said = fb && VERDICTS.find((x) => x.v === fb.verdict)?.label;

  return (
    <section className="cva" aria-label={`${a.fit_title}, ${a.score} out of 10`}>
      <header className="cvhead">
        <div>
          <div className="cvtitle">{a.fit_title}</div>
          <div className="meta">
            {a.this_role ? "This role" : "Another role"}
            {a.role_title !== a.fit_title && `: ${a.role_title}`}
            {a.profile_score !== null && ` · profile ranked ${a.profile_score}/100`}
          </div>
        </div>
        <div className={`cvscore s${a.score >= 7 ? "hi" : a.score >= 5 ? "mid" : "lo"}`}>
          {a.score}
          <span>/10</span>
        </div>
      </header>
      <div className="cvgrid">
        <List label="Matches" items={a.matches} cls="ok" />
        <List label="Falls short" items={a.gaps} cls="gap" />
        <List label="Flags" items={a.flags} cls="flagged" />
      </div>
      {a.questions.length > 0 && (
        <div className="cvsec qs">
          <div className="lbl">Questions to ask</div>
          <ol>
            {a.questions.map((q, i) => (
              <li key={i}>
                <span>{q}</span>
                <button
                  type="button"
                  className={`star${vital.includes(i) ? " on" : ""}`}
                  aria-pressed={vital.includes(i)}
                  aria-label={`Vital: ${q}`}
                  disabled={!editing}
                  onClick={() => star(i)}
                >
                  {vital.includes(i) ? "★ Vital" : "☆"}
                </button>
              </li>
            ))}
          </ol>
        </div>
      )}
      {!editing && fb ? (
        <div className="fbdone">
          <span className="flag rc">
            You said {said?.toLowerCase()}
            {fb.your_score !== null ? `: ${fb.your_score}/10` : ""}
          </span>
          {fb.note && <span className="fbnote">{fb.note}</span>}
          <button type="button" className="link-button" onClick={() => setEditing(true)}>
            Change
          </button>
        </div>
      ) : (
        <div className="fb" role="group" aria-label={`Is ${a.score} out of 10 right?`}>
          <span className="fbq">Is {a.score}/10 right?</span>
          <div className="seg">
            {VERDICTS.map((x) => (
              <button
                key={x.v}
                type="button"
                className={verdict === x.v ? "sel" : undefined}
                aria-pressed={verdict === x.v}
                onClick={() => pick(x.v)}
              >
                {x.label}
              </button>
            ))}
          </div>
          {range.length > 0 && (
            <div className="seg pick" role="group" aria-label="Your score">
              {range.map((n) => (
                <button
                  key={n}
                  type="button"
                  className={mine === n ? "sel" : undefined}
                  aria-pressed={mine === n}
                  onClick={() => setMine(n)}
                >
                  {n}
                </button>
              ))}
            </div>
          )}
          <input
            className="in fbwhy"
            placeholder="Why? (optional, Claude learns from it)"
            value={why}
            maxLength={500}
            onChange={(e) => setWhy(e.target.value)}
          />
          <button
            type="button"
            className="btn-ghost"
            disabled={!verdict || save.isPending}
            onClick={() => save.mutate()}
          >
            {save.isPending ? "Saving" : "Save"}
          </button>
          {fb && (
            <button
              type="button"
              className="link-button"
              onClick={() => {
                setVerdict(fb.verdict);
                setMine(fb.your_score);
                setWhy(fb.note);
                setVital(fb.vital);
                setEditing(false);
              }}
            >
              Cancel
            </button>
          )}
          <p className="hint">Star the questions that matter most. Sourcer uses your answers to calibrate later rankings.</p>
          {save.error && (
            <p className="form-error" role="alert">
              {save.error.message}
            </p>
          )}
        </div>
      )}
    </section>
  );
}
