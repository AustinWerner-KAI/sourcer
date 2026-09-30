import { useState, type FormEvent, type KeyboardEvent } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../api/client";

const briefLabel: Record<string, string> = {
  none: "No brief yet",
  draft: "Draft",
  confirmed: "Confirmed",
};

/** Every role, newest first, with where its brief is. */
export function Briefs() {
  const roles = useQuery({ queryKey: ["roles"], queryFn: api.roles });

  return (
    <main>
      <div className="head-row">
        <header>
          <div className="eyebrow">Sourcer</div>
          <h1>Brief</h1>
        </header>
        <Link className="btn-primary btn-inline" to="/brief/new">
          New role
        </Link>
      </div>
      <section className="panel">
        {roles.isLoading && <p className="panel-note">Loading</p>}
        {roles.isError && <p className="form-error">Could not load roles. Please refresh.</p>}
        {roles.data && roles.data.length === 0 && (
          <p>No roles yet. Add one with its job spec, and Claude drafts the brief for you to check.</p>
        )}
        {roles.data && roles.data.length > 0 && (
          <table className="team">
            <thead>
              <tr>
                <th>Role</th>
                <th>Client</th>
                <th>Brief</th>
              </tr>
            </thead>
            <tbody>
              {roles.data.map((r) => (
                <tr key={r.id}>
                  <td>
                    <Link to={`/brief/${r.id}`}>{r.title}</Link>
                  </td>
                  <td>{r.client_name ?? "None"}</td>
                  <td>
                    <span className={`pill pill-${r.brief_state === "confirmed" ? "active" : "invited"}`}>
                      {briefLabel[r.brief_state] ?? r.brief_state}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </main>
  );
}

/** Step 1: client, title and spec. Then Claude drafts the brief. */
export function NewRole() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const clients = useQuery({ queryKey: ["clients"], queryFn: api.clients });
  const [clientId, setClientId] = useState("");
  const [title, setTitle] = useState("");
  const [spec, setSpec] = useState("");
  const [adding, setAdding] = useState(false);

  const create = useMutation({
    mutationFn: async () => {
      const role = await api.createRole({ client_id: clientId, title, spec_text: spec });
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
      // The role is saved even if drafting failed; the editor explains why.
      navigate(`/brief/${role.id}`, { state: { draftNote } });
    },
  });

  const submit = (e: FormEvent) => {
    e.preventDefault();
    create.mutate();
  };

  return (
    <main>
      <div className="head-row">
        <header>
          <div className="eyebrow">New role</div>
          <h1>Brief</h1>
        </header>
        <Steps at={1} />
      </div>
      <section className="panel narrow">
        <h2 className="panel-title">The role</h2>
        <p className="panel-note">Paste the client's spec. Nothing is spent until you confirm the brief.</p>
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
              <input className="in" value={title} onChange={(e) => setTitle(e.target.value)} required maxLength={200} />
            </label>
          </div>
          {adding && (
            <AddClient
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
      </section>
    </main>
  );
}

/** Add a client. The web domain is required so their staff can be kept out. */
function AddClient({ onDone }: { onDone: (id: string | null) => void }) {
  // This sits inside the role form: Enter must add the client, not submit the role.
  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (!add.isPending) add.mutate();
    }
  };
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [domain, setDomain] = useState("");
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

export function Steps({ at }: { at: 1 | 2 | 3 | 4 }) {
  const names = ["Spec", "Check the brief", "Search", "Candidates"];
  return (
    <div className="steps">
      {names.map((n, i) => (
        <span key={n} className={i + 1 === at ? "on" : i + 1 < at ? "done" : undefined}>
          {i + 1} · {n}
        </span>
      ))}
    </div>
  );
}
