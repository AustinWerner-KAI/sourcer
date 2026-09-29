import { useEffect, useState, type KeyboardEvent, type ReactNode } from "react";
import { useLocation, useParams } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, SignedOut } from "../api/client";
import type { BriefLines } from "../api/types/BriefLines";
import type { RoleDetail } from "../api/types/RoleDetail";
import type { ToolStatus } from "../api/types/ToolStatus";
import { Steps } from "./Briefs";

const EMPLOYER_OPTIONS = [
  "Payments and neobanks",
  "Crypto and digital assets",
  "Trading firms",
  "E-commerce",
  "Adtech",
];
const TOOL_CHOICES: { value: ToolStatus; label: string }[] = [
  { value: "required", label: "Required" },
  { value: "nice", label: "Nice to have" },
  { value: "replacing", label: "Being replaced" },
];

const empty: BriefLines = {
  levels: [],
  excluded_titles: ["Manager", "Director", "Head of", "VP", "Chief"],
  must_haves: [],
  tools: [],
  locations: [],
  remote: false,
  employer_types: EMPLOYER_OPTIONS.slice(0, 3),
  leave_out: [],
};

/** The same checks the server makes before confirming. */
export function problems(l: BriefLines): string[] {
  const out: string[] = [];
  if (l.levels.length === 0) out.push("Choose at least one level.");
  if (l.must_haves.length === 0) out.push("Add at least one must-have.");
  const open = l.tools.filter((t) => t.status === null).length;
  if (open > 0) out.push(`Answer ${open} named tool${open === 1 ? "" : "s"}.`);
  if (l.locations.length === 0 && !l.remote) out.push("Add a location, or allow remote.");
  if (l.employer_types.length === 0) out.push("Choose at least one employer type.");
  return out;
}

/** Step 2: check the five lines, answer every tool, confirm. */
export function BriefEditor() {
  const { id = "" } = useParams();
  const draftNote = (useLocation().state as { draftNote?: string | null } | null)?.draftNote;
  const queryClient = useQueryClient();
  const role = useQuery({
    queryKey: ["role", id],
    queryFn: () => api.role(id),
    retry: (n, e) => !(e instanceof SignedOut) && n < 2,
  });
  const [lines, setLines] = useState<BriefLines | null>(null);
  const [dirty, setDirty] = useState(false);
  const [title, setTitle] = useState("");
  const [spec, setSpec] = useState("");
  const [roleDirty, setRoleDirty] = useState(false);
  const clients = useQuery({ queryKey: ["clients"], queryFn: api.clients });

  // Load the server's copy whenever it changes, never over unsaved edits.
  useEffect(() => {
    if (!role.data) return;
    if (!roleDirty) {
      setTitle(role.data.title);
      setSpec(role.data.spec_text);
    }
    if (!dirty) setLines(role.data.brief ? role.data.brief.lines : null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [role.data]);

  useEffect(() => {
    if (role.error instanceof SignedOut) queryClient.setQueryData(["me"], null);
  }, [role.error, queryClient]);

  const settle = (d: RoleDetail) => {
    queryClient.setQueryData(["role", id], d);
    queryClient.invalidateQueries({ queryKey: ["roles"] });
    setDirty(false);
    setLines(d.brief ? d.brief.lines : null);
  };
  const onError = (e: Error) => {
    if (e instanceof SignedOut) queryClient.setQueryData(["me"], null);
  };
  // Whatever happened, show the server's current copy afterwards.
  const onSettled = () => queryClient.invalidateQueries({ queryKey: ["role", id] });
  const roleSaved = (d: RoleDetail) => {
    setRoleDirty(false);
    queryClient.setQueryData(["role", id], d);
    queryClient.invalidateQueries({ queryKey: ["roles"] });
  };

  const saveRole = useMutation({
    mutationFn: (client_id: string | null) => api.updateRole(id, { title, spec_text: spec, client_id }),
    onSuccess: roleSaved,
    onError,
  });
  const draft = useMutation({
    // Unsaved edits are saved first, so the tool answers and leave-out list
    // the resourcer was promised are kept are the ones on screen.
    mutationFn: async (unsaved: BriefLines | null) => {
      if (unsaved) await api.saveBrief(id, unsaved);
      roleSaved(await api.updateRole(id, { title, spec_text: spec, client_id: null }));
      return api.draftBrief(id);
    },
    onSuccess: settle,
    onError,
    onSettled,
  });
  const save = useMutation({
    mutationFn: (l: BriefLines) => api.saveBrief(id, l),
    onSuccess: settle,
    onError,
    onSettled,
  });
  const confirm = useMutation({
    mutationFn: (l: BriefLines) =>
      api.confirmBrief(id, l, queryClient.getQueryData<RoleDetail>(["role", id])?.brief?.version ?? null),
    onSuccess: settle,
    onError,
    onSettled,
  });
  const all = [saveRole, draft, save, confirm];
  /** Clear old errors, then run one action. */
  const run = (action: () => void) => {
    all.forEach((m) => m.reset());
    action();
  };
  const redraft = () => {
    const replacing = dirty || (brief !== null && brief !== undefined);
    if (
      replacing &&
      !window.confirm(
        "Draft again from the spec? Levels, must-haves, tools and locations are replaced. Your tool answers, employer types and leave-out list are kept.",
      )
    )
      return;
    run(() => draft.mutate(dirty ? lines : null));
  };

  if (role.isLoading) return <main className="panel-note">Loading</main>;
  if (!role.data) return <main className="form-error">Could not load this role.</main>;
  const r = role.data;
  const brief = r.brief;
  const confirmed = brief?.confirmed && !dirty;
  const set = (patch: Partial<BriefLines>) => {
    setLines((l) => ({ ...(l ?? empty), ...patch }));
    setDirty(true);
  };
  const issues = lines ? problems(lines) : [];
  const busy = all.some((m) => m.isPending);
  const error = all.map((m) => m.error).find((e) => e);

  return (
    <main>
      <div className="head-row">
        <header>
          <div className="eyebrow">{r.client?.name ?? "Role"}</div>
          <h1>{r.title}</h1>
        </header>
        <Steps at={confirmed ? 3 : 2} />
      </div>

      <div className="brief-grid">
        <section className="panel">
          <h2 className="panel-title">The role</h2>
          {!r.client && (
            <label className="f">
              Client (needed before the brief can be confirmed)
              <select
                className="in"
                value=""
                disabled={busy}
                onChange={(e) => e.target.value && run(() => saveRole.mutate(e.target.value))}
              >
                <option value="">Choose a client</option>
                {clients.data?.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </select>
            </label>
          )}
          <label className="f">
            Role title
            <input
              className="in"
              value={title}
              onChange={(e) => {
                setTitle(e.target.value);
                setRoleDirty(true);
              }}
              maxLength={200}
            />
          </label>
          <label className="f">
            Job spec
            <textarea
              className="in spec"
              value={spec}
              maxLength={30000}
              onChange={(e) => {
                setSpec(e.target.value);
                setRoleDirty(true);
              }}
            />
          </label>
          <div className="actions-row">
            <button className="btn-ghost" onClick={redraft} disabled={busy || !spec.trim()}>
              {draft.isPending ? "Drafting" : brief ? "Draft the brief again" : "Draft the brief"}
            </button>
            <button className="link-button" onClick={() => run(() => saveRole.mutate(null))} disabled={busy || !roleDirty}>
              Save spec
            </button>
          </div>
        </section>

        <section className="panel">
          {!lines ? (
            <div>
              <h2 className="panel-title">No brief yet</h2>
              <p className="panel-note">Draft it from the spec, or fill in the five lines yourself.</p>
              {draftNote && !draft.isSuccess && <p className="form-error">{draftNote}</p>}
              <button className="btn-ghost" onClick={() => set({})}>
                Fill in myself
              </button>
            </div>
          ) : (
            <>
              {confirmed ? (
                <div className="notice ok">
                  Brief confirmed (version {brief?.version}). Any change starts version {(brief?.version ?? 0) + 1}.
                </div>
              ) : brief?.drafted_by_ai ? (
                <div className="notice">Claude drafted these five lines from the spec. Check each one.</div>
              ) : null}

              <Line n={1} title="Level" hint="Hands-on levels to search. Crossed out titles are never searched.">
                <ChipList items={lines.levels} onChange={(levels) => set({ levels })} add="Add level" tone="yes" />
                <ChipList
                  items={lines.excluded_titles}
                  onChange={(excluded_titles) => set({ excluded_titles })}
                  add="Exclude a title"
                  tone="no"
                />
              </Line>

              <Line n={2} title="Top three must-haves" hint="In order of weight. Use the arrows to reorder.">
                <MustHaves items={lines.must_haves} onChange={(must_haves) => set({ must_haves })} />
              </Line>

              <Line n={3} title="Named tools" hint="A tool in a spec can be one the client is replacing. Answer each one.">
                {lines.tools.map((t, i) => (
                  <div key={t.name} className={`tool${t.status === null ? " ask" : ""}`}>
                    <span className="name">
                      {t.name}
                      <button
                        className="x"
                        aria-label={`Remove ${t.name}`}
                        onClick={() => set({ tools: lines.tools.filter((_, j) => j !== i) })}
                      >
                        ×
                      </button>
                    </span>
                    <span className="seg" role="radiogroup" aria-label={`How to treat ${t.name}`}>
                      {TOOL_CHOICES.map((c) => (
                        <button
                          key={c.value}
                          role="radio"
                          aria-checked={t.status === c.value}
                          className={t.status === c.value ? `sel${c.value === "replacing" ? " rep" : ""}` : undefined}
                          onClick={() =>
                            set({ tools: lines.tools.map((x, j) => (j === i ? { ...x, status: c.value } : x)) })
                          }
                        >
                          {c.label}
                        </button>
                      ))}
                    </span>
                  </div>
                ))}
                <AddInput
                  placeholder="Add a tool"
                  onAdd={(name) =>
                    !lines.tools.some((t) => t.name.toLowerCase() === name.toLowerCase()) &&
                    set({ tools: [...lines.tools, { name, status: null }] })
                  }
                />
              </Line>

              <Line n={4} title="Location">
                <ChipList items={lines.locations} onChange={(locations) => set({ locations })} add="Add city" tone="yes" />
                <label className="check">
                  <input type="checkbox" checked={lines.remote} onChange={(e) => set({ remote: e.target.checked })} />
                  Remote counts
                </label>
                {lines.locations.some((l) => l.toLowerCase() === "dubai") && lines.locations.length > 1 && (
                  <p className="warnline">Dubai will run as its own search. Few profiles there for niche roles.</p>
                )}
              </Line>

              <Line n={5} title="Employer type">
                <div className="lbl">Search in</div>
                <div className="chips">
                  {[...EMPLOYER_OPTIONS, ...lines.employer_types.filter((t) => !EMPLOYER_OPTIONS.includes(t))].map(
                    (t) => {
                      const on = lines.employer_types.includes(t);
                      return (
                        <button
                          key={t}
                          className={`chip${on ? " yes" : ""}`}
                          aria-pressed={on}
                          onClick={() =>
                            set({
                              employer_types: on
                                ? lines.employer_types.filter((x) => x !== t)
                                : [...lines.employer_types, t],
                            })
                          }
                        >
                          {t}
                        </button>
                      );
                    },
                  )}
                </div>
                <div className="lbl">Leave out</div>
                <div className="chips">
                  {r.locked_out.map((c) => (
                    <span key={c.id} className="chip lock" title="Always left out. Cannot be removed.">
                      🔒 {c.name}
                      {c.hiring ? " (hiring client)" : ""}
                    </span>
                  ))}
                </div>
                <ChipList items={lines.leave_out} onChange={(leave_out) => set({ leave_out })} add="Add company" tone="no" />
                {r.client && (
                  <p className="offl">
                    {r.client.name} staff are never searched, shortlisted or contacted for this role.
                  </p>
                )}
              </Line>

              <div className="foot">
                <div className="cost">
                  {confirmed ? (
                    <>Search comes next: count the matches for <strong>1 credit</strong>, then choose how many to pull.</>
                  ) : issues.length > 0 ? (
                    <span className="warn">{issues.join(" ")}</span>
                  ) : (
                    <>Ready. Next: count the matches for <strong>1 credit</strong>.</>
                  )}
                </div>
                {!confirmed && (
                  <div className="actions-row">
                    <button className="link-button" onClick={() => run(() => save.mutate(lines))} disabled={busy || !dirty}>
                      Save draft
                    </button>
                    <button
                      className="btn-primary btn-inline"
                      onClick={() => run(() => confirm.mutate(lines))}
                      disabled={busy || issues.length > 0}
                    >
                      {confirm.isPending ? "Confirming" : "Confirm brief"}
                    </button>
                  </div>
                )}
              </div>
            </>
          )}
          {error && (
            <p className="form-error" role="alert">
              {error.message}
            </p>
          )}
        </section>
      </div>
    </main>
  );
}

function Line({ n, title, hint, children }: { n: number; title: string; hint?: string; children: ReactNode }) {
  return (
    <div className="line">
      <div className="num">{n}</div>
      <div>
        <h3>{title}</h3>
        {hint && <p className="hint">{hint}</p>}
        {children}
      </div>
    </div>
  );
}

function AddInput({ placeholder, onAdd }: { placeholder: string; onAdd: (v: string) => void }) {
  const [v, setV] = useState("");
  const commit = () => {
    const t = v.trim();
    if (t) onAdd(t);
    setV("");
  };
  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      commit();
    }
  };
  return (
    <input
      className="add-input"
      value={v}
      placeholder={`+ ${placeholder}`}
      onChange={(e) => setV(e.target.value)}
      onKeyDown={onKey}
      onBlur={commit}
      maxLength={120}
    />
  );
}

function ChipList({
  items,
  onChange,
  add,
  tone,
}: {
  items: string[];
  onChange: (v: string[]) => void;
  add: string;
  tone: "yes" | "no";
}) {
  return (
    <div className="chips">
      {items.map((it) => (
        <span key={it} className={`chip ${tone}`}>
          {it}
          <button className="x" aria-label={`Remove ${it}`} onClick={() => onChange(items.filter((x) => x !== it))}>
            ×
          </button>
        </span>
      ))}
      <AddInput
        placeholder={add}
        onAdd={(v) => !items.some((x) => x.toLowerCase() === v.toLowerCase()) && onChange([...items, v])}
      />
    </div>
  );
}

function MustHaves({ items, onChange }: { items: string[]; onChange: (v: string[]) => void }) {
  const move = (i: number, d: -1 | 1) => {
    const next = [...items];
    [next[i], next[i + d]] = [next[i + d], next[i]];
    onChange(next);
  };
  return (
    <>
      <ol className="must">
        {items.map((m, i) => (
          <li key={m}>
            <span className="order">
              <button aria-label="Move up" disabled={i === 0} onClick={() => move(i, -1)}>
                ↑
              </button>
              <button aria-label="Move down" disabled={i === items.length - 1} onClick={() => move(i, 1)}>
                ↓
              </button>
            </span>
            {m}
            {i === 0 && <span className="w">Most weight</span>}
            <button className="x" aria-label={`Remove ${m}`} onClick={() => onChange(items.filter((_, j) => j !== i))}>
              ×
            </button>
          </li>
        ))}
      </ol>
      {items.length < 3 && (
        <AddInput
          placeholder="Add a must-have"
          onAdd={(v) => !items.some((x) => x.toLowerCase() === v.toLowerCase()) && onChange([...items, v])}
        />
      )}
    </>
  );
}
