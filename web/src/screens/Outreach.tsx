import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../api/client";
import type { CandidateRow } from "../api/types/CandidateRow";
import type { OutreachView } from "../api/types/OutreachView";
import type { OutreachStepEdit } from "../api/types/OutreachStepEdit";

const WHEN = ["First email", "Follow-up", "Final email"];

const when = (step: number, delay: number) =>
  step === 1 ? "Sent first" : `${delay} day${delay === 1 ? "" : "s"} after the last, if no reply`;

/** The email line on a shortlisted card: draft, review, approve or stop the three emails. */
export function EmailLine({ p, roleId }: { p: CandidateRow; roleId: string }) {
  const [open, setOpen] = useState(false);
  const personal = p.contacts.some((c) => c.kind === "personal_email");
  const queryClient = useQueryClient();
  const draft = useMutation({
    mutationFn: () => api.draftOutreach(p.id),
    onSuccess: (v) => {
      queryClient.setQueryData(["outreach", p.id], v);
      setOpen(true);
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey: ["candidates", roleId] }),
  });

  let flag;
  if (p.do_not_contact) flag = <span className="flag stop">Do not email</span>;
  else if (p.state === "approved") flag = <span className="flag sent">Emails approved · waiting for Outlook</span>;
  else if (p.state === "drafted") flag = <span className="flag emp">Emails drafted, not approved</span>;
  else if (!personal) flag = <span className="flag rc">No personal email, so no email</span>;
  else flag = <span className="flag rc">No emails yet</span>;

  const canDraft = p.state === "shortlisted" && personal && !p.do_not_contact;
  const hasDraft = p.state === "drafted" || p.state === "approved";

  return (
    <div className="emline">
      <div className="rcflags">
        <span className="lbl2">Email</span>
        {flag}
        {hasDraft && (
          <button type="button" className="link-button" onClick={() => setOpen(!open)} aria-expanded={open}>
            {open ? "Close" : p.state === "drafted" ? "Review" : "View"}
          </button>
        )}
      </div>
      {canDraft && (
        <button type="button" className="btn-ghost rcadd" onClick={() => draft.mutate()} disabled={draft.isPending}>
          {draft.isPending ? "Drafting" : "Draft 3 emails"}
        </button>
      )}
      {draft.error && (
        <p className="form-error" role="alert">
          {draft.error.message}
        </p>
      )}
      {open && hasDraft && <Sequence id={p.id} roleId={roleId} onClosed={() => setOpen(false)} />}
    </div>
  );
}

/** The three emails: edit, preview, approve once for all three, or stop. */
function Sequence({ id, roleId, onClosed }: { id: string; roleId: string; onClosed: () => void }) {
  const queryClient = useQueryClient();
  const q = useQuery({ queryKey: ["outreach", id], queryFn: () => api.outreach(id) });
  const [edits, setEdits] = useState<Record<number, OutreachStepEdit>>({});
  const [preview, setPreview] = useState(false);
  const v = q.data;
  // A fresh copy from the server replaces local edits.
  useEffect(() => setEdits({}), [v?.version]);

  const done = (next: OutreachView) => {
    queryClient.setQueryData(["outreach", id], next);
    queryClient.invalidateQueries({ queryKey: ["candidates", roleId] });
  };
  const refresh = () => {
    queryClient.invalidateQueries({ queryKey: ["outreach", id] });
    queryClient.invalidateQueries({ queryKey: ["candidates", roleId] });
  };
  const save = useMutation({
    mutationFn: () => api.saveOutreach(id, v!.version, Object.values(edits)),
    onSuccess: done,
    onError: refresh,
  });
  const approve = useMutation({ mutationFn: () => api.approveOutreach(id, v!.version), onSuccess: done, onError: refresh });
  const stop = useMutation({
    mutationFn: () => api.stopOutreach(id, v!.version),
    onSuccess: (next) => {
      done(next);
      onClosed();
    },
    onError: refresh,
  });
  const redo = useMutation({ mutationFn: () => api.draftOutreach(id, v!.version), onSuccess: done, onError: refresh });

  if (q.isLoading) return <p className="panel-note">Loading the emails</p>;
  if (!v) return <p className="form-error">Could not load the emails. Please refresh.</p>;
  const draft = v.status === "draft";
  const dirty = Object.keys(edits).length > 0;
  const busy = save.isPending || approve.isPending || stop.isPending || redo.isPending;
  const error = save.error ?? approve.error ?? stop.error ?? redo.error;
  const change = (step: number, field: "subject" | "body", value: string) => {
    const s = v.steps.find((x) => x.step === step)!;
    const base = edits[step] ?? { step, subject: s.subject, body: s.body };
    setEdits({ ...edits, [step]: { ...base, [field]: value } });
  };

  return (
    <div className="seq" role="group" aria-label="Emails">
      <div className="seqhead">
        <span>
          To <strong>{v.to_email}</strong> · from {v.sender}'s Outlook
        </span>
        <button type="button" className="link-button" onClick={() => setPreview(!preview)}>
          {preview ? "Edit" : "Preview"}
        </button>
      </div>
      {v.steps.map((s) => {
        const e = edits[s.step];
        return (
          <div className="mail" key={s.step}>
            <div className="mailhead">
              <span className="n">{WHEN[s.step - 1]}</span>
              <span className="d">{s.sent_at ? `Sent ${s.sent_at}` : when(s.step, s.delay_days)}</span>
            </div>
            {preview || !draft ? (
              <>
                <div className="subj">{e?.subject ?? s.subject}</div>
                {e ? (
                  <p className="dirty">Save to see the preview.</p>
                ) : (
                  // Built by the server from escaped text; only http(s) links are made.
                  <div className="mailview" dangerouslySetInnerHTML={{ __html: s.html }} />
                )}
              </>
            ) : (
              <>
                <input
                  className="in"
                  aria-label={`${WHEN[s.step - 1]} subject`}
                  value={e?.subject ?? s.subject}
                  onChange={(x) => change(s.step, "subject", x.target.value)}
                />
                <textarea
                  className="in mailbody"
                  aria-label={`${WHEN[s.step - 1]} text`}
                  value={e?.body ?? s.body}
                  rows={s.step === 1 ? 15 : 4}
                  onChange={(x) => change(s.step, "body", x.target.value)}
                />
              </>
            )}
          </div>
        );
      })}
      {draft && v.problems.length > 0 && (
        <ul className="problems" aria-label="Fix before approving">
          {v.problems.map((m) => (
            <li key={m}>
              {m}
              {m.includes("Settings") && (
                <>
                  {" "}
                  <Link to="/settings">Open Settings</Link>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
      {draft && v.notes.length > 0 && (
        <ul className="notes">
          {v.notes.map((m) => (
            <li key={m}>{m}</li>
          ))}
        </ul>
      )}
      {v.status === "approved" && (
        <p className="seqnote">
          Approved {v.approved_at}. Nothing goes out until Outlook is connected. Any reply stops the rest.
        </p>
      )}
      <div className="seqacts">
        {draft && (
          <>
            <button type="button" className="btn-ghost" onClick={() => save.mutate()} disabled={!dirty || busy}>
              {save.isPending ? "Saving" : "Save"}
            </button>
            <button
              type="button"
              className="btn-ghost go"
              onClick={() => approve.mutate()}
              disabled={dirty || busy || v.problems.length > 0}
              title={dirty ? "Save first" : undefined}
            >
              {approve.isPending ? "Approving" : "Approve all 3"}
            </button>
            <button
              type="button"
              className="link-button"
              onClick={() => window.confirm("Start again from your template? Your edits will be lost.") && redo.mutate()}
              disabled={busy}
            >
              Start again
            </button>
          </>
        )}
        {(draft || v.status === "approved" || v.status === "active") && (
          <button type="button" className="rej" onClick={() => stop.mutate()} disabled={busy}>
            {draft ? "Discard" : "Stop"}
          </button>
        )}
      </div>
      {error && (
        <p className="form-error" role="alert">
          {error.message}
        </p>
      )}
    </div>
  );
}
