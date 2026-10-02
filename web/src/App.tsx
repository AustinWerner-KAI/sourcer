import { NavLink, Navigate, Route, Routes, useParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./api/client";
import { SignIn } from "./screens/SignIn";
import { Team } from "./screens/Team";
import { Briefs, NewRole } from "./screens/Briefs";
import { BriefEditor } from "./screens/BriefEditor";
import { Search } from "./screens/Search";
import { Candidates } from "./screens/Candidates";
import { Settings } from "./screens/Settings";
import { Today } from "./screens/Today";
import { AdminLayout, Clients, Controls, DoNotContact } from "./screens/Admin";

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

/** A role's own address opens the step it is at: candidates once the brief is confirmed. */
function RoleHome() {
  const { id } = useParams();
  const role = useQuery({ queryKey: ["role", id], queryFn: () => api.role(id!) });
  if (role.isLoading) return <main aria-busy="true" />;
  if (!role.data) return <Navigate to="/roles" replace />;
  return <Navigate to={`/roles/${id}/${role.data.brief?.confirmed ? "candidates" : "brief"}`} replace />;
}

/** Old /brief/:id links keep working. */
function Moved({ step }: { step: "brief" | "search" | "candidates" }) {
  const { id } = useParams();
  return <Navigate to={`/roles/${id}/${step}`} replace />;
}

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
          <NavLink to="/roles">Roles</NavLink>
        </div>
        <div className="nav-links util">
          <NavLink to="/settings">Settings</NavLink>
          {isAdmin && <NavLink to="/admin">Admin</NavLink>}
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
        <Route path="/today" element={<Today />} />
        <Route path="/roles" element={<Briefs />} />
        <Route path="/roles/new" element={<NewRole />} />
        <Route path="/roles/:id" element={<RoleHome />} />
        <Route path="/roles/:id/brief" element={<BriefEditorPage />} />
        <Route path="/roles/:id/search" element={<SearchPage />} />
        <Route path="/roles/:id/candidates" element={<CandidatesPage />} />
        <Route path="/settings" element={<Settings isAdmin={isAdmin} />} />
        {isAdmin && (
          <Route path="/admin" element={<AdminLayout />}>
            <Route index element={<Navigate to="/admin/team" replace />} />
            <Route path="team" element={<Team me={me.data} />} />
            <Route path="clients" element={<Clients />} />
            <Route path="do-not-contact" element={<DoNotContact />} />
            <Route path="controls" element={<Controls />} />
          </Route>
        )}
        {/* Old addresses. */}
        <Route path="/brief" element={<Navigate to="/roles" replace />} />
        <Route path="/brief/new" element={<Navigate to="/roles/new" replace />} />
        <Route path="/brief/:id" element={<Moved step="brief" />} />
        <Route path="/brief/:id/search" element={<Moved step="search" />} />
        <Route path="/brief/:id/candidates" element={<Moved step="candidates" />} />
        <Route path="/team" element={<Navigate to="/admin/team" replace />} />
        <Route path="*" element={<Navigate to="/today" replace />} />
      </Routes>
    </div>
  );
}
