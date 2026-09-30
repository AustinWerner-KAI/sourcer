import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";

/** Pick a role to review its candidates. Only roles with a confirmed brief can have any. */
export function CandidateRoles() {
  const roles = useQuery({ queryKey: ["roles"], queryFn: api.roles });
  const ready = roles.data?.filter((r) => r.brief_state === "confirmed") ?? [];

  return (
    <main>
      <header>
        <div className="eyebrow">Sourcer</div>
        <h1>Candidates</h1>
      </header>
      <section className="panel">
        {roles.isLoading && <p className="panel-note">Loading</p>}
        {roles.isError && <p className="form-error">Could not load roles. Please refresh.</p>}
        {roles.data && ready.length === 0 && (
          <p>
            No role has a confirmed brief yet. <Link to="/brief">Go to Brief</Link> to check one.
          </p>
        )}
        {ready.length > 0 && (
          <table className="team">
            <thead>
              <tr>
                <th>Role</th>
                <th>Client</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {ready.map((r) => (
                <tr key={r.id}>
                  <td>
                    <Link to={`/brief/${r.id}/candidates`}>{r.title}</Link>
                  </td>
                  <td>{r.client_name ?? "None"}</td>
                  <td className="actions">
                    <Link to={`/brief/${r.id}/search`}>Search</Link>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
      <section className="panel">
        <h2 className="panel-title">Save LinkedIn profiles with the Chrome button</h2>
        <p className="panel-note">
          On someone's LinkedIn profile, click the Sourcer button, check the details and pick the role. They join that
          role's list and get ranked. It reads only the page you are on, only when you click.
        </p>
        <ol className="steps-list">
          <li>
            In Chrome, open <code>chrome://extensions</code> and turn on Developer mode.
          </li>
          <li>
            Click Load unpacked and choose the <code>sourcer/extension</code> folder.
          </li>
          <li>Pin the Sourcer button, open its Options and enter this Sourcer address:</li>
        </ol>
        <p className="addr">
          <code>{window.location.origin}</code>
        </p>
      </section>
    </main>
  );
}
