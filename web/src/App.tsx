import { NavLink, Navigate, Route, Routes } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./api/client";
import { Screen } from "./screens/Screen";
import { SignIn } from "./screens/SignIn";
import { Team } from "./screens/Team";
import { Briefs, NewRole } from "./screens/Briefs";
import { BriefEditor } from "./screens/BriefEditor";
import { Search } from "./screens/Search";
import { useParams } from "react-router-dom";

/** A fresh editor per role, so edits on one role never carry to another. */
function BriefEditorPage() {
  const { id } = useParams();
  return <BriefEditor key={id} />;
}

/** A fresh search screen per role, so choices never carry to another role. */
function SearchPage() {
  const { id } = useParams();
  return <Search key={id} />;
}

const screens = [
  { path: "/today", label: "Today", note: "Replies waiting, follow-ups due and new matches for live roles." },
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
  const isAdmin = me.data.role === "admin";

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
          <NavLink to="/today">Today</NavLink>
          <NavLink to="/brief">Brief</NavLink>
          {screens.slice(1).map((s) => (
            <NavLink key={s.path} to={s.path}>
              {s.label}
            </NavLink>
          ))}
          {isAdmin && <NavLink to="/team">Team</NavLink>}
        </div>
        <div className="user">
          <div className="user-name">{me.data.name}</div>
          <div className="user-role">{isAdmin ? "Admin" : "Resourcer"}</div>
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
        <Route path="/brief" element={<Briefs />} />
        <Route path="/brief/new" element={<NewRole />} />
        <Route path="/brief/:id" element={<BriefEditorPage />} />
        <Route path="/brief/:id/search" element={<SearchPage />} />
        <Route path="/team" element={isAdmin ? <Team me={me.data} /> : <Navigate to="/today" replace />} />
      </Routes>
    </div>
  );
}
