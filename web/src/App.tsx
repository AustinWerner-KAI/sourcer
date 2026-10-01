import { NavLink, Navigate, Route, Routes, useLocation } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./api/client";
import { Screen } from "./screens/Screen";
import { SignIn } from "./screens/SignIn";
import { Team } from "./screens/Team";
import { Briefs, NewRole } from "./screens/Briefs";
import { BriefEditor } from "./screens/BriefEditor";
import { Search } from "./screens/Search";
import { Candidates } from "./screens/Candidates";
import { CandidateRoles } from "./screens/CandidateRoles";
import { useParams } from "react-router-dom";

/** A fresh editor per role, so edits on one role never carry to another. */
function BriefEditorPage() {
  const { id } = useParams();
  return <BriefEditor key={id} />;
}

/** A fresh candidates screen per role, so an open reject never carries over. */
function CandidatesPage() {
  const { id } = useParams();
  return <Candidates key={id} />;
}

/** A fresh search screen per role, so choices never carry to another role. */
function SearchPage() {
  const { id } = useParams();
  return <Search key={id} />;
}

const screens = [
  { path: "/today", label: "Today", note: "Replies waiting, follow-ups due and new matches for live roles." },
  {
    path: "/outreach",
    label: "Outreach",
    note: "Drafts for email, LinkedIn and WhatsApp. Approve each message before it goes.",
  },
  {
    path: "/client-map",
    label: "Client map",
    note: "Where a client's hires come from: feeders, universities, titles.",
  },
  { path: "/playbook", label: "Playbook", note: "Rules learned from your feedback, waiting for approval or active." },
];

/** Green when connected, red when not, grey when unknown. The words beside it say which. */
function Light({ on }: { on: boolean | null }) {
  return <span className={`light ${on === null ? "idle" : on ? "on" : "off"}`} aria-hidden="true" />;
}

export function App() {
  const me = useQuery({ queryKey: ["me"], queryFn: api.me, retry: false });
  const health = useQuery({ queryKey: ["health"], queryFn: api.health, refetchInterval: 30_000 });
  // Read every five minutes; the server calls Recruitly at most once in ten.
  const recruitly = useQuery({
    queryKey: ["recruitly-status"],
    queryFn: api.recruitlyStatus,
    enabled: Boolean(me.data),
    refetchInterval: 300_000,
    staleTime: 60_000,
  });
  const queryClient = useQueryClient();
  // A role's candidates live under /brief/:id but belong to Candidates in the menu.
  const onCandidates = /^\/brief\/[^/]+\/candidates/.test(useLocation().pathname);

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
          <NavLink to="/brief" className={({ isActive }) => (isActive && !onCandidates ? "active" : undefined)}>
            Brief
          </NavLink>
          <NavLink to="/candidates" className={({ isActive }) => (isActive || onCandidates ? "active" : undefined)}>
            Candidates
          </NavLink>
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
        <div className="statuses">
          <div className="status" role="status">
            Server{health.data ? ` · v${health.data.version}` : ""}
            <strong>
              <Light on={health.isLoading ? null : Boolean(health.data?.database)} />
              {health.isLoading
                ? "Checking"
                : !health.data
                  ? "Not reachable"
                  : health.data.database
                    ? "Connected"
                    : "Database down"}
            </strong>
          </div>
          {recruitly.data && (
            <div className="status" role="status">
              Recruitly
              <strong>
                <Light on={recruitly.data.configured ? recruitly.data.connected : null} />
                {!recruitly.data.configured
                  ? "Not set up"
                  : recruitly.data.connected === null
                    ? "Not checked"
                    : recruitly.data.connected
                      ? "Connected"
                      : "Not connected"}
              </strong>
            </div>
          )}
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
        <Route path="/brief/:id/candidates" element={<CandidatesPage />} />
        <Route path="/candidates" element={<CandidateRoles />} />
        <Route path="/team" element={isAdmin ? <Team me={me.data} /> : <Navigate to="/today" replace />} />
      </Routes>
    </div>
  );
}
