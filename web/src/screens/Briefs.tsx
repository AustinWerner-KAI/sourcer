import { useState, type FormEvent, type KeyboardEvent } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../api/client";
import { JobSearch, useRecruitly } from "./Recruitly";

const briefLabel: Record<string, string> = {
  none: "No brief yet",
  draft: "Draft",
  confirmed: "Confirmed",
};

/** Every role, newest first: open ones by default, closed ones a tap away. */
export function Briefs() {
  const queryClient = useQueryClient();
  const roles = useQuery({ queryKey: ["roles"], queryFn: api.roles });
  const [tab, setTab] = useState<"open" | "closed">("open");
  // The role whose Close is waiting for a second tap.
  const [asking, setAsking] = useState<string | null>(null);
  const close = useMutation({
    mutationFn: ({ id, closed }: { id: string; closed: boolean }) => api.closeRole(id, closed),
    onSuccess: (rows, { id }) => {
      queryClient.setQueryData(["roles"], rows);
      queryClient.invalidateQueries({ queryKey: ["role", id] });
      queryClient.invalidateQueries({ queryKey: ["search", id] });
      queryClient.invalidateQueries({ queryKey: ["today"] });
      setAsking(null);
    },
  });
  const all = roles.data ?? [];
  const open = all.filter((r) => !r.closed);
  const closed = all.filter((r) => r.closed);
  const shown = tab === "open" ? open : closed;

  return (
    <main>
      <div className="head-row">
        <header>
          <div className="eyebrow">Sourcer</div>
          <h1>Roles</h1>
        </header>
        <Link className="btn-primary btn-inline" to="/roles/new">
          New role
        </Link>
      </div>
      <section className="panel">
        {roles.isLoading && <p className="panel-note">Loading</p>}
        {roles.isError && <p className="form-error">Could not load roles. Please refresh.</p>}
        {roles.data && all.length === 0 && (
          <p>No roles yet. Add one with its job spec, and Claude drafts the brief for you to check.</p>
        )}
        {all.length > 0 && (
          <span className="seg pick role-tabs" role="tablist" aria-label="Which roles">
            {(
              [
                ["open", `Open (${open.length})`],
                ["closed", `Closed (${closed.length})`],
              ] as const
            ).map(([k, label]) => (
              <button
                key={k}
                role="tab"
                aria-selected={tab === k}
                className={tab === k ? "sel" : undefined}
                onClick={() => {
                  setTab(k);
                  setAsking(null);
                }}
              >
                {label}
              </button>
            ))}
          </span>
        )}
        {all.length > 0 && shown.length === 0 && (
          <p className="note">{tab === "open" ? "No open roles." : "No closed roles."}</p>
        )}
        {shown.length > 0 && (
          <table className="team roles">
            <thead>
              <tr>
                <th>Role</th>
                <th>Client</th>
                <th>Brief</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {shown.map((r) => (
                <tr key={r.id}>
                  <td>
                    <Link to={`/roles/${r.id}`}>{r.title}</Link>
                  </td>
                  <td>{r.client_name ?? "None"}</td>
                  <td>
                    <span className={`pill pill-${r.brief_state === "confirmed" ? "active" : "invited"}`}>
                      {briefLabel[r.brief_state] ?? r.brief_state}
                    </span>
                  </td>
                  <td className="actions">
                    {asking === r.id ? (
                      <span className="ask-close" role="group" aria-label={`Close ${r.title}`}>
                        <span className="note">
                          {r.active_sequences > 0
                            ? `Stops ${r.active_sequences} email sequence${r.active_sequences === 1 ? "" : "s"} still going.`
                            : "No more searching or emails."}
                        </span>
                        <button className="link-button" onClick={() => setAsking(null)}>
                          Keep open
                        </button>
                        <button
                          className="btn-ghost"
                          onClick={() => close.mutate({ id: r.id, closed: true })}
                          disabled={close.isPending}
                        >
                          Close role
                        </button>
                      </span>
                    ) : (
                      <span className="row-acts">
                        {r.closed ? (
                          <button
                            className="link-button"
                            onClick={() => close.mutate({ id: r.id, closed: false })}
                            disabled={close.isPending}
                          >
                            Reopen
                          </button>
                        ) : (
                          <>
                            {r.brief_state === "confirmed" ? (
                              <Link to={`/roles/${r.id}/candidates`}>Candidates</Link>
                            ) : (
                              <Link to={`/roles/${r.id}/brief`}>Check the brief</Link>
                            )}
                            <button className="link-button quiet" onClick={() => setAsking(r.id)}>
                              Close
                            </button>
                          </>
                        )}
                      </span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {close.error && (
          <p className="form-error" role="alert">
            {close.error.message}
          </p>
        )}
      </section>
    </main>
  );
}

/** Step 1: client, title and spec, typed in or read from a Recruitly job. Then Claude drafts the brief. */
export function NewRole() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const clients = useQuery({ queryKey: ["clients"], queryFn: api.clients });
  const recruitly = useRecruitly();
  const [fromRecruitly, setFromRecruitly] = useState(true);
  const [jobId, setJobId] = useState<string | null>(null);
  const [clientId, setClientId] = useState("");
  const [title, setTitle] = useState("");
  const [spec, setSpec] = useState("");
  const [adding, setAdding] = useState(false);
  const useJob = recruitly.data?.configured === true && fromRecruitly;

  // The job only fills the form. Nothing is saved until "Save and draft the brief".
  const job = useQuery({
    queryKey: ["recruitly-job", jobId],
    queryFn: () => api.recruitlyJob(jobId as string),
    enabled: jobId !== null,
    staleTime: Infinity,
    retry: false,
  });
  const [filledFrom, setFilledFrom] = useState<string | null>(null);
  if (job.data && filledFrom !== job.data.id) {
    setFilledFrom(job.data.id);
    setTitle(job.data.title);
    setSpec(job.data.spec_text);
    setClientId(job.data.client_id ?? "");
    setAdding(!job.data.client_id);
  }
  const changeJob = () => {
    setJobId(null);
    setFilledFrom(null);
    setTitle("");
    setSpec("");
    setClientId("");
    setAdding(false);
  };

  const create = useMutation({
    mutationFn: async () => {
      const role =
        useJob && jobId
          ? await api.importRole({ job_id: jobId, client_id: clientId, title, spec_text: spec })
          : await api.createRole({ client_id: clientId, title, spec_text: spec });
      let draftNote: string | null = null;
      if (spec.trim()) {
        try {
          await api.draftBrief(role.id);
        } catch (e) {
          draftNote = (e as Error).message;
        }
      }
      return { role, draftNote };
    },
    onSuccess: ({ role, draftNote }) => {
      queryClient.invalidateQueries({ queryKey: ["roles"] });
      queryClient.invalidateQueries({ queryKey: ["recruitly-jobs"] });
      // The role is saved even if drafting failed; the editor explains why.
      navigate(`/roles/${role.id}/brief`, { state: { draftNote } });
    },
  });

  const submit = (e: FormEvent) => {
    e.preventDefault();
    create.mutate();
  };

  // A job that already has a role opens that role instead of making a second one.
  const showForm = !recruitly.isLoading && (!useJob || (jobId !== null && job.data && !job.data.role_id));

  return (
    <main>
      <div className="head-row">
        <header>
          <div className="eyebrow">New role</div>
          <h1>Spec and brief</h1>
        </header>
        <Steps at={1} />
      </div>
      <section className="panel narrow">
        <h2 className="panel-title">The role</h2>
        <p className="panel-note">
          {useJob
            ? "Pick the job in Recruitly. Its title, client, spec and Client Brief fill the form for you to check. Nothing is spent until you confirm the brief."
            : "Paste the client's spec. Nothing is spent until you confirm the brief."}
        </p>
        {recruitly.data?.configured && (
          <div className="tabs source" role="group" aria-label="Where the spec comes from">
            <button
              type="button"
              aria-pressed={fromRecruitly}
              className={fromRecruitly ? "on" : undefined}
              onClick={() => setFromRecruitly(true)}
            >
              From Recruitly
            </button>
            <button
              type="button"
              aria-pressed={!fromRecruitly}
              className={!fromRecruitly ? "on" : undefined}
              onClick={() => {
                if (!fromRecruitly) return;
                // Keep what is in the form; it just stops being tied to the job.
                setFromRecruitly(false);
                setJobId(null);
                setFilledFrom(null);
              }}
            >
              Paste a spec
            </button>
          </div>
        )}
        {recruitly.isLoading && <p className="panel-note">Loading</p>}
        {useJob && jobId === null && <JobSearch onPick={setJobId} />}
        {useJob && jobId !== null && (
          <div className="picked">
            {job.isLoading && <span>Reading the job from Recruitly</span>}
            {job.data && (
              <span>
                From Recruitly: <strong>{job.data.label}</strong>
                {job.data.company_name ? ` for ${job.data.company_name}` : ""}.
              </span>
            )}
            {job.data?.role_id && <Link to={`/roles/${job.data.role_id}`}>This job already has a role. Open it.</Link>}
            <button type="button" className="link-button" onClick={changeJob}>
              Change job
            </button>
            {job.isError && (
              <p className="form-error" role="alert">
                {job.error.message}
              </p>
            )}
          </div>
        )}
        {showForm && (
          <form onSubmit={submit}>
            <div className="row2">
              <label className="f">
                Client
                <select
                  className="in"
                  value={clientId}
                  onChange={(e) => (e.target.value === "+" ? setAdding(true) : setClientId(e.target.value))}
                  required
                >
                  <option value="">Choose a client</option>
                  {clients.data?.map((c) => (
                    <option key={c.id} value={c.id}>
                      {c.name}
                      {c.off_limits ? " (off-limits)" : ""}
                    </option>
                  ))}
                  <option value="+">+ Add a client</option>
                </select>
              </label>
              <label className="f">
                Role title
                <input
                  className="in"
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                  required
                  maxLength={200}
                />
              </label>
            </div>
            {adding && (
              <AddClient
                key={filledFrom ?? "new"}
                initialName={job.data?.company_name ?? ""}
                initialDomain={job.data?.company_domain ?? ""}
                fromRecruitly={Boolean(job.data)}
                onDone={(id) => {
                  setAdding(false);
                  if (id) setClientId(id);
                }}
              />
            )}
            <label className="f">
              Job spec
              <textarea
                className="in spec"
                value={spec}
                onChange={(e) => setSpec(e.target.value)}
                maxLength={30000}
                placeholder="Paste the job spec here"
              />
            </label>
            <button className="btn-primary btn-inline" type="submit" disabled={create.isPending || !clientId}>
              {create.isPending ? "Drafting the brief" : "Save and draft the brief"}
            </button>
            {create.isError && (
              <p className="form-error" role="alert">
                {create.error.message}
              </p>
            )}
          </form>
        )}
      </section>
    </main>
  );
}

/** Add a client. The web domain is required so their staff can be kept out. */
function AddClient({
  onDone,
  initialName = "",
  initialDomain = "",
  fromRecruitly = false,
}: {
  onDone: (id: string | null) => void;
  initialName?: string;
  initialDomain?: string;
  fromRecruitly?: boolean;
}) {
  // This sits inside the role form: Enter must add the client, not submit the role.
  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (!add.isPending) add.mutate();
    }
  };
  const queryClient = useQueryClient();
  const [name, setName] = useState(initialName);
  const [domain, setDomain] = useState(initialDomain);
  const [offLimits, setOffLimits] = useState(false);
  const add = useMutation({
    mutationFn: () => api.createClient({ name, domain, off_limits: offLimits }),
    onSuccess: (c) => {
      queryClient.invalidateQueries({ queryKey: ["clients"] });
      onDone(c.id);
    },
  });

  return (
    <div className="subpanel">
      {fromRecruitly && (
        <p className="hint">
          This client is new to Sourcer. Check the details from Recruitly
          {initialDomain ? "" : ", and add their web domain (Recruitly has none)"}.
        </p>
      )}
      <div className="row2">
        <label className="f">
          Client name
          <input className="in" value={name} onChange={(e) => setName(e.target.value)} onKeyDown={onKey} />
        </label>
        <label className="f">
          Web domain
          <input
            className="in"
            value={domain}
            onChange={(e) => setDomain(e.target.value)}
            onKeyDown={onKey}
            placeholder="example.com"
          />
        </label>
      </div>
      <p className="hint">Their staff are never searched or contacted for their own roles. We match on this domain and the name.</p>
      <label className="check">
        <input type="checkbox" checked={offLimits} onChange={(e) => setOffLimits(e.target.checked)} />
        Off-limits for every role (never approach their staff)
      </label>
      <div className="actions-row">
        <button type="button" className="btn-ghost" onClick={() => add.mutate()} disabled={add.isPending}>
          Add client
        </button>
        <button type="button" className="link-button" onClick={() => onDone(null)}>
          Cancel
        </button>
      </div>
      {add.isError && (
        <p className="form-error" role="alert">
          {add.error.message}
        </p>
      )}
    </div>
  );
}

/** Where a role is: spec and brief, search, candidates. Links once the role exists. */
export function Steps({ at, roleId }: { at: 1 | 2 | 3; roleId?: string }) {
  const steps = [
    { n: "Spec and brief", to: "brief" },
    { n: "Search", to: "search" },
    { n: "Candidates", to: "candidates" },
  ];
  return (
    <nav className="steps" aria-label="Steps for this role">
      {steps.map((s, i) => {
        const cls = i + 1 === at ? "on" : i + 1 < at ? "done" : undefined;
        const label = `${i + 1} · ${s.n}`;
        return roleId && i + 1 !== at ? (
          <Link key={s.n} className={cls} to={`/roles/${roleId}/${s.to}`}>
            {label}
          </Link>
        ) : (
          <span key={s.n} className={cls} aria-current={i + 1 === at ? "step" : undefined}>
            {label}
          </span>
        );
      })}
    </nav>
  );
}
