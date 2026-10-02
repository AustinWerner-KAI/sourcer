import { useState, type FormEvent } from "react";
import { NavLink, Outlet } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../api/client";
import type { Client } from "../api/types/Client";
import type { Controls as ControlsT } from "../api/types/Controls";
import type { DncReason } from "../api/types/DncReason";
import { RecruitlyPanel } from "./Recruitly";

const TABS = [
  { to: "/admin/team", label: "Team" },
  { to: "/admin/clients", label: "Clients" },
  { to: "/admin/do-not-contact", label: "Do not contact" },
  { to: "/admin/controls", label: "Controls" },
];

/** Admin: one page, four tabs. Only admins reach it. */
export function AdminLayout() {
  return (
    <main>
      <div className="head-row">
        <header>
          <div className="eyebrow">Sourcer</div>
          <h1>Admin</h1>
        </header>
      </div>
      <nav className="tabs admin-tabs" aria-label="Admin">
        {TABS.map((t) => (
          <NavLink key={t.to} to={t.to} className={({ isActive }) => (isActive ? "on" : undefined)}>
            {t.label}
          </NavLink>
        ))}
      </nav>
      <Outlet />
    </main>
  );
}

/** Admin › Controls: the two kill switches, the daily email limit, then the Recruitly connection. */
export function Controls() {
  const queryClient = useQueryClient();
  const q = useQuery({ queryKey: ["controls"], queryFn: api.controls });
  const save = useMutation({
    mutationFn: api.saveControls,
    onSuccess: (c) => queryClient.setQueryData(["controls"], c),
  });
  const set = (change: Partial<ControlsT>) => q.data && save.mutate({ ...q.data, ...change });

  return (
    <>
      <section className="panel narrow">
        <h2 className="panel-title">Safety switches</h2>
        <p className="panel-note">Each one takes effect at once, for everyone on the team.</p>
        {q.isLoading && <p className="panel-note">Loading</p>}
        {q.isError && <p className="form-error">Could not load the switches. Please refresh.</p>}
        {q.data && (
          <div className="switches">
            <Switch
              title="Sending"
              on={!q.data.sending_paused}
              onText="Emails go out as approved."
              offText="Paused. No email goes out until you resume."
              pause="Pause sending"
              resume="Resume sending"
              busy={save.isPending}
              onChange={(running) => set({ sending_paused: !running })}
            />
            <Switch
              title="Paid calls"
              on={!q.data.paid_calls_paused}
              onText="Searches, ranking and CV checks run as normal."
              offText="Paused. No search, ranking or CV check is paid for until you resume."
              pause="Pause paid calls"
              resume="Resume paid calls"
              busy={save.isPending}
              onChange={(running) => set({ paid_calls_paused: !running })}
            />
            <DailyLimit
              key={q.data.first_emails_per_day}
              value={q.data.first_emails_per_day}
              busy={save.isPending}
              onSave={(n) => set({ first_emails_per_day: n })}
            />
          </div>
        )}
        {save.error && (
          <p className="form-error" role="alert">
            {save.error.message}
          </p>
        )}
      </section>
      <RecruitlyPanel />
    </>
  );
}

/** First emails each person may send in a day. Follow-ups never count. */
function DailyLimit({ value, busy, onSave }: { value: number; busy: boolean; onSave: (n: number) => void }) {
  const [n, setN] = useState(String(value));
  const parsed = Number(n);
  const ok = n.trim() !== "" && Number.isInteger(parsed) && parsed >= 0 && parsed <= 200;
  return (
    <form
      className="switch"
      onSubmit={(e) => {
        e.preventDefault();
        if (ok) onSave(parsed);
      }}
    >
      <div>
        <div className="sw-title">First emails a day</div>
        <p className="sw-note">Per person, Monday to Friday. Follow-ups do not count. Fewer is safer for your mailbox.</p>
      </div>
      <span className="limit">
        <input
          className="in"
          type="number"
          inputMode="numeric"
          min={0}
          max={200}
          aria-label="First emails a day, per person"
          value={n}
          onChange={(e) => setN(e.target.value)}
        />
        <button type="submit" className="btn-ghost" disabled={busy || !ok || parsed === value}>
          Save
        </button>
      </span>
    </form>
  );
}

function Switch(props: {
  title: string;
  on: boolean;
  onText: string;
  offText: string;
  pause: string;
  resume: string;
  busy: boolean;
  onChange: (running: boolean) => void;
}) {
  return (
    <div className={`switch${props.on ? "" : " off"}`}>
      <div>
        <div className="sw-title">
          {props.title}
          <span className={`pill ${props.on ? "pill-active" : "pill-disabled"}`}>{props.on ? "On" : "Paused"}</span>
        </div>
        <p className="sw-note">{props.on ? props.onText : props.offText}</p>
      </div>
      <button
        type="button"
        className={props.on ? "rej" : "btn-ghost"}
        disabled={props.busy}
        onClick={() => props.onChange(!props.on)}
      >
        {props.on ? props.pause : props.resume}
      </button>
    </div>
  );
}

/** Admin › Clients: every client, its domain and whether it is off-limits. */
export function Clients() {
  const queryClient = useQueryClient();
  const clients = useQuery({ queryKey: ["clients"], queryFn: api.clients });
  const [editing, setEditing] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const offLimits = clients.data?.filter((c) => c.off_limits).length ?? 0;

  return (
    <section className="panel">
      <div className="panel-head">
        <div>
          <h2 className="panel-title">Clients</h2>
          <p className="panel-note">
            A client's own staff are never approached for its roles. Off-limits clients' staff are never approached for
            any role. {offLimits > 0 && `${offLimits} off-limits.`}
          </p>
        </div>
        {!adding && (
          <button type="button" className="btn-ghost" onClick={() => setAdding(true)}>
            Add client
          </button>
        )}
      </div>
      {adding && (
        <ClientForm
          onDone={() => {
            setAdding(false);
            queryClient.invalidateQueries({ queryKey: ["clients"] });
          }}
        />
      )}
      {clients.isLoading && <p className="panel-note">Loading</p>}
      {clients.isError && <p className="form-error">Could not load clients. Please refresh.</p>}
      {clients.data && clients.data.length === 0 && <p>No clients yet.</p>}
      {clients.data && clients.data.length > 0 && (
        <table className="team clients">
          <thead>
            <tr>
              <th>Client</th>
              <th>Web domain</th>
              <th>Off-limits</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {clients.data.map((c) =>
              editing === c.id ? (
                <tr key={c.id}>
                  <td colSpan={4}>
                    <ClientForm
                      client={c}
                      onDone={() => {
                        setEditing(null);
                        queryClient.invalidateQueries({ queryKey: ["clients"] });
                      }}
                    />
                  </td>
                </tr>
              ) : (
                <tr key={c.id}>
                  <td>{c.name}</td>
                  <td>{c.domain ?? <span className="dim">None</span>}</td>
                  <td>{c.off_limits ? <span className="flag stop">Off-limits</span> : <span className="dim">No</span>}</td>
                  <td className="actions">
                    <button type="button" className="link-button" onClick={() => setEditing(c.id)}>
                      Edit
                    </button>
                  </td>
                </tr>
              ),
            )}
          </tbody>
        </table>
      )}
    </section>
  );
}

function ClientForm({ client, onDone }: { client?: Client; onDone: () => void }) {
  const [name, setName] = useState(client?.name ?? "");
  const [domain, setDomain] = useState(client?.domain ?? "");
  const [offLimits, setOffLimits] = useState(client?.off_limits ?? false);
  const save = useMutation({
    mutationFn: () =>
      client
        ? api.updateClient(client.id, { name, domain, off_limits: offLimits })
        : api.createClient({ name, domain, off_limits: offLimits }),
    onSuccess: onDone,
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    save.mutate();
  };
  return (
    <form className="subpanel" onSubmit={submit}>
      <div className="row2">
        <label className="f">
          Client name
          <input className="in" value={name} onChange={(e) => setName(e.target.value)} required maxLength={200} />
        </label>
        <label className="f">
          Web domain
          <input
            className="in"
            value={domain}
            onChange={(e) => setDomain(e.target.value)}
            placeholder="example.com"
            required
          />
        </label>
      </div>
      <label className="check">
        <input type="checkbox" checked={offLimits} onChange={(e) => setOffLimits(e.target.checked)} />
        Off-limits for every role (never approach their staff)
      </label>
      <div className="actions-row">
        <button type="submit" className="btn-ghost" disabled={save.isPending}>
          {save.isPending ? "Saving" : client ? "Save" : "Add client"}
        </button>
        <button type="button" className="link-button" onClick={onDone}>
          Cancel
        </button>
      </div>
      {save.error && (
        <p className="form-error" role="alert">
          {save.error.message}
        </p>
      )}
    </form>
  );
}

const REASON_LABEL: Record<DncReason, string> = {
  opt_out: "Asked not to be contacted",
  erasure_request: "Asked to be erased",
};

/** Admin › Do not contact: everyone who must never be approached. Entries are permanent. */
export function DoNotContact() {
  const queryClient = useQueryClient();
  const [q, setQ] = useState("");
  const [page, setPage] = useState(0);
  const list = useQuery({ queryKey: ["dnc", q, page], queryFn: () => api.dnc(q, page) });
  const [identifier, setIdentifier] = useState("");
  const [reason, setReason] = useState<DncReason>("opt_out");
  const add = useMutation({
    mutationFn: () => api.addDnc({ identifier, reason }),
    onSuccess: () => {
      setIdentifier("");
      queryClient.invalidateQueries({ queryKey: ["dnc"] });
      queryClient.invalidateQueries({ queryKey: ["candidates"] });
    },
  });
  const pages = Math.max(1, Math.ceil((list.data?.total ?? 0) / 50));

  return (
    <>
      <section className="panel narrow">
        <h2 className="panel-title">Add someone</h2>
        <p className="panel-note">
          Sourcer checks this list before every shortlist and every email. Adding someone stops any emails waiting for
          them. Entries are permanent.
        </p>
        <form
          className="dncadd"
          onSubmit={(e) => {
            e.preventDefault();
            add.mutate();
          }}
        >
          <label className="f grow">
            Email, LinkedIn profile or phone
            <input
              className="in"
              value={identifier}
              onChange={(e) => setIdentifier(e.target.value)}
              placeholder="name@example.com"
              required
            />
          </label>
          <label className="f">
            Reason
            <select className="in" value={reason} onChange={(e) => setReason(e.target.value as DncReason)}>
              <option value="opt_out">{REASON_LABEL.opt_out}</option>
              <option value="erasure_request">{REASON_LABEL.erasure_request}</option>
            </select>
          </label>
          <button type="submit" className="btn-ghost" disabled={add.isPending}>
            {add.isPending ? "Adding" : "Add"}
          </button>
        </form>
        {add.error && (
          <p className="form-error" role="alert">
            {add.error.message}
          </p>
        )}
      </section>
      <section className="panel">
        <div className="panel-head">
          <h2 className="panel-title">The list{list.data ? ` (${list.data.total})` : ""}</h2>
          <input
            className="in search"
            aria-label="Search the list"
            placeholder="Search"
            value={q}
            onChange={(e) => {
              setQ(e.target.value);
              setPage(0);
            }}
          />
        </div>
        {list.isLoading && <p className="panel-note">Loading</p>}
        {list.isError && <p className="form-error">Could not load the list. Please refresh.</p>}
        {list.data && list.data.entries.length === 0 && (
          <p className="panel-note">{q ? "No one matches." : "No one on the list yet."}</p>
        )}
        {list.data && list.data.entries.length > 0 && (
          <table className="team">
            <thead>
              <tr>
                <th>Email, LinkedIn or phone</th>
                <th>Reason</th>
                <th>Added</th>
              </tr>
            </thead>
            <tbody>
              {list.data.entries.map((d) => (
                <tr key={d.identifier}>
                  <td className="ident">{d.identifier}</td>
                  <td>{REASON_LABEL[d.reason]}</td>
                  <td>{d.added}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {pages > 1 && (
          <div className="pager">
            <button type="button" className="link-button" disabled={page === 0} onClick={() => setPage(page - 1)}>
              Newer
            </button>
            <span>
              Page {page + 1} of {pages}
            </span>
            <button
              type="button"
              className="link-button"
              disabled={page + 1 >= pages}
              onClick={() => setPage(page + 1)}
            >
              Older
            </button>
          </div>
        )}
      </section>
    </>
  );
}
