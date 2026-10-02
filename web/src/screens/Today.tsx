import { Link } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../api/client";
import type { TodayItem } from "../api/types/TodayItem";
import type { TodayReply } from "../api/types/TodayReply";

const KIND: Record<string, { label: string; cls: string }> = {
  reply: { label: "Replied", cls: "sent" },
  auto: { label: "Automatic reply", cls: "emp" },
  bounce: { label: "Bounced", cls: "stop" },
};

/** Today: replies first, then emails to approve, then what is going out. */
export function Today() {
  const q = useQuery({ queryKey: ["today"], queryFn: api.today, refetchInterval: 60_000 });
  const v = q.data;

  return (
    <main>
      <header>
        <div className="eyebrow">Sourcer</div>
        <h1>Today</h1>
      </header>
      {q.isLoading && <p className="panel-note">Loading</p>}
      {q.isError && <p className="form-error">Could not load Today. Please refresh.</p>}
      {v && (
        <>
          {v.paused && (
            <div className="notice warn-notice" role="status">
              An admin has paused sending. Nothing goes out until it is resumed in Admin, Controls.
            </div>
          )}
          {!v.outlook_ready && (
            <div className="notice" role="status">
              Your Outlook is not connected, so your approved emails wait. <Link to="/settings">Connect it in Settings</Link>
            </div>
          )}
          {v.outlook_ready && !v.in_hours && !v.paused && (
            <p className="later">Outside working hours. Emails go out Monday to Friday, 8:00 to 18:00 Dubai time.</p>
          )}

          <section className="panel">
            <h2 className="panel-title">
              Replies <span className="c">{v.replies.length}</span>
            </h2>
            <p className="panel-note">Any reply stops the rest of that person's emails. Read the reply in Outlook.</p>
            {v.replies.length === 0 ? (
              <p className="panel-note">No replies waiting.</p>
            ) : (
              <ul className="today">
                {v.replies.map((r) => (
                  <Reply key={r.candidacy_id} r={r} />
                ))}
              </ul>
            )}
          </section>

          <section className="panel">
            <h2 className="panel-title">
              To approve <span className="c">{v.to_approve.length}</span>
            </h2>
            {v.to_approve.length === 0 ? (
              <p className="panel-note">Nothing waiting for your approval.</p>
            ) : (
              <Items items={v.to_approve} />
            )}
          </section>

          <section className="panel">
            <h2 className="panel-title">
              Going out <span className="c">{v.going_out.length}</span>
            </h2>
            {v.going_out.length === 0 ? (
              <p className="panel-note">None of your approved emails are waiting.</p>
            ) : (
              <Items items={v.going_out} />
            )}
          </section>
        </>
      )}
    </main>
  );
}

function Items({ items }: { items: TodayItem[] }) {
  return (
    <ul className="today">
      {items.map((i) => (
        <li key={i.candidacy_id}>
          <div>
            <Link className="nm" to={`/roles/${i.role_id}/candidates?tab=shortlisted`}>
              {i.name}
            </Link>
            <span className="ti">{i.role_title}</span>
          </div>
          <span className="when">{i.note}</span>
        </li>
      ))}
    </ul>
  );
}

function Reply({ r }: { r: TodayReply }) {
  const queryClient = useQueryClient();
  const refresh = () => {
    queryClient.invalidateQueries({ queryKey: ["today"] });
    queryClient.invalidateQueries({ queryKey: ["candidates"] });
  };
  const done = useMutation({ mutationFn: () => api.replyHandled(r.candidacy_id), onSuccess: refresh });
  const optOut = useMutation({ mutationFn: () => api.optOut(r.candidacy_id), onSuccess: refresh });
  const k = KIND[r.kind] ?? KIND.reply;
  const busy = done.isPending || optOut.isPending;
  const error = done.error ?? optOut.error;

  return (
    <li>
      <div>
        <Link className="nm" to={`/roles/${r.role_id}/candidates?tab=shortlisted`}>
          {r.name}
        </Link>
        <span className="ti">{r.role_title}</span>
        <div className="sub">
          <span className={`flag ${k.cls}`}>{k.label}</span> {r.at} · to {r.sender}'s Outlook
        </div>
        {error && (
          <p className="form-error" role="alert">
            {error.message}
          </p>
        )}
      </div>
      <span className="actions-row">
        {r.kind === "reply" &&
          (r.opted_out ? (
            <span className="flag stop">Opted out</span>
          ) : (
            <button
              type="button"
              className="rej"
              disabled={busy}
              onClick={() =>
                window.confirm(`${r.name} said no? They are never contacted again, for any role.`) && optOut.mutate()
              }
            >
              They said no
            </button>
          ))}
        <button type="button" className="btn-ghost" disabled={busy} onClick={() => done.mutate()}>
          Done
        </button>
      </span>
    </li>
  );
}
