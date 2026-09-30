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
    </main>
  );
}
