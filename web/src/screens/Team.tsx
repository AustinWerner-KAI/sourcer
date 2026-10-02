import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, SignedOut } from "../api/client";
import type { Me } from "../api/types/Me";
import type { Role } from "../api/types/Role";
import type { TeamMember } from "../api/types/TeamMember";

const statusLabel: Record<TeamMember["status"], string> = {
  invited: "Invited",
  active: "Active",
  disabled: "Switched off",
};

/** Admin › Team: add people to Sourcer and switch them off or back on. */
export function Team({ me }: { me: Me }) {
  const queryClient = useQueryClient();
  const team = useQuery({
    queryKey: ["team"],
    queryFn: api.team,
    // An ended session will not come back by retrying.
    retry: (n, e) => !(e instanceof SignedOut) && n < 2,
  });
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<Role>("resourcer");

  const refresh = () => queryClient.invalidateQueries({ queryKey: ["team"] });
  // A session that ended mid-way sends the user back to sign-in.
  const onError = (e: Error) => {
    if (e instanceof SignedOut) queryClient.setQueryData(["me"], null);
  };
  useEffect(() => {
    if (team.error) onError(team.error);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [team.error]);
  const invite = useMutation({
    mutationFn: api.invite,
    onSuccess: () => {
      setName("");
      setEmail("");
      setRole("resourcer");
      refresh();
    },
    onError,
  });
  const change = useMutation({
    mutationFn: ({ id, disabled }: { id: string; disabled: boolean }) => api.updateMember(id, { disabled }),
    onSuccess: refresh,
    onError,
  });

  const submit = (e: FormEvent) => {
    e.preventDefault();
    invite.mutate({ name, email, role });
  };

  const toggle = (m: TeamMember) => {
    const off = m.status !== "disabled";
    if (off && !window.confirm(`Switch off ${m.name}? They will be signed out at once.`)) return;
    change.mutate({ id: m.id, disabled: off });
  };

  return (
    <>
      <section className="panel">
        <h2 className="panel-title">Add someone</h2>
        <p className="panel-note">They sign in with their Microsoft 365 account. Nothing is sent to them.</p>
        <form className="invite" onSubmit={submit}>
          <label>
            Name
            <input value={name} onChange={(e) => setName(e.target.value)} required maxLength={200} />
          </label>
          <label>
            Work email
            <input type="email" value={email} onChange={(e) => setEmail(e.target.value)} required />
          </label>
          <label>
            Role
            <select value={role} onChange={(e) => setRole(e.target.value as Role)}>
              <option value="resourcer">Resourcer</option>
              <option value="admin">Admin</option>
            </select>
          </label>
          <button className="btn-primary" type="submit" disabled={invite.isPending}>
            {invite.isPending ? "Adding" : "Add"}
          </button>
        </form>
        {invite.isError && (
          <p className="form-error" role="alert">
            {invite.error.message}
          </p>
        )}
      </section>

      <section className="panel">
        <h2 className="panel-title">People</h2>
        {change.isError && (
          <p className="form-error" role="alert">
            {change.error.message}
          </p>
        )}
        {team.isLoading && <p className="panel-note">Loading</p>}
        {team.isError && <p className="form-error">Could not load the team. Please refresh.</p>}
        {team.data && (
          <table className="team people">
            <thead>
              <tr>
                <th>Name</th>
                <th>Email</th>
                <th>Role</th>
                <th>Status</th>
                <th aria-label="Actions" />
              </tr>
            </thead>
            <tbody>
              {team.data.map((m) => (
                <tr key={m.id} className={m.status === "disabled" ? "is-off" : undefined}>
                  <td>
                    {m.name}
                    {m.id === me.id && <span className="you">You</span>}
                  </td>
                  <td>{m.email}</td>
                  <td>{m.role === "admin" ? "Admin" : "Resourcer"}</td>
                  <td>
                    <span className={`pill pill-${m.status}`}>{statusLabel[m.status]}</span>
                  </td>
                  <td className="actions">
                    {m.id !== me.id && (
                      <button className="link-button" onClick={() => toggle(m)} disabled={change.isPending}>
                        {m.status === "disabled" ? "Switch on" : "Switch off"}
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </>
  );
}
