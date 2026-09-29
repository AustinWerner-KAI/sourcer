import { NavLink, Navigate, Route, Routes } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { api } from "./api/client";
import { Screen } from "./screens/Screen";

const screens = [
  { path: "/today", label: "Today", note: "Replies waiting, follow-ups due and new matches for live roles." },
  { path: "/brief", label: "Brief", note: "Upload a spec and confirm the five-line check before any credits are spent." },
  { path: "/candidates", label: "Candidates", note: "Ranked list with reasons and unknowns. Shortlist or reject with a reason code." },
  { path: "/outreach", label: "Outreach", note: "Drafts for email, LinkedIn and WhatsApp. Approve each message before it goes." },
  { path: "/client-map", label: "Client map", note: "Where a client's hires come from: feeders, universities, titles." },
  { path: "/playbook", label: "Playbook", note: "Rules learned from your feedback, waiting for approval or active." },
];

export function App() {
  const health = useQuery({ queryKey: ["health"], queryFn: api.health, refetchInterval: 30_000 });

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
