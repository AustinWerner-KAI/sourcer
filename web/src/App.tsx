import { NavLink, Navigate, Route, Routes } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./api/client";
import { Screen } from "./screens/Screen";
import { SignIn } from "./screens/SignIn";

const screens = [
  { path: "/today", label: "Today", note: "Replies waiting, follow-ups due and new matches for live roles." },
  { path: "/brief", label: "Brief", note: "Upload a spec and confirm the five-line check before any credits are spent." },
  { path: "/candidates", label: "Candidates", note: "Ranked list with reasons and unknowns. Shortlist or reject with a reason code." },
  { path: "/outreach", label: "Outreach", note: "Drafts for email, LinkedIn and WhatsApp. Approve each message before it goes." },
  { path: "/client-map", label: "Client map", note: "Where a client's hires come from: feeders, universities, titles." },
  { path: "/playbook", label: "Playbook", note: "Rules learned from your feedback, waiting for approval or active." },
];

export function App() {
  const me = useQuery({ queryKey: ["me"], queryFn: api.me, retry: false });
  const health = useQuery({ queryKey: ["health"], queryFn: api.health, refetchInterval: 30_000 });
  const queryClient = useQueryClient();

  if (me.isLoading) return <div className="loading" aria-busy="true" />;
  if (!me.data) return <SignIn />;

  // Only show the signed-out screen once the server has ended the session.
  const signOut = async () => {
    try {
      await api.logout();
      queryClient.setQueryData(["me"], null);
    } catch {
      window.alert("Sign-out did not complete. Please try again.");
    }
  };

  return (
    <div className="shell">
      <nav className="nav" aria-label="Sourcer">
        <div>
          <div className="brand">AUSTIN WERNER</div>
          <div className="brand-sub">Sourcer</div>
        </div>
        <div className="nav-links">
          {screens.map((s) => (
            <NavLink key={s.path} to={s.path}>
              {s.label}
            </NavLink>
          ))}
        </div>
        <div className="user">
          <div className="user-name">{me.data.name}</div>
          <div className="user-role">{me.data.role === "admin" ? "Admin" : "Resourcer"}</div>
          <button className="link-button" onClick={signOut}>
            Sign out
          </button>
        </div>
        <div className="status" role="status">
          Server
          <strong>
            {health.isLoading
              ? "Checking"
              : !health.data
                ? "Not reachable"
                : health.data.database
                  ? `Connected · v${health.data.version}`
                  : "Database down"}
          </strong>
        </div>
      </nav>
      <Routes>
        <Route path="/" element={<Navigate to="/today" replace />} />
        {screens.map((s) => (
          <Route key={s.path} path={s.path} element={<Screen title={s.label} note={s.note} />} />
        ))}
      </Routes>
    </div>
  );
}
