import type { Health } from "./types/Health";
import type { Me } from "./types/Me";
import type { MemberUpdate } from "./types/MemberUpdate";
import type { NewMember } from "./types/NewMember";
import type { TeamMember } from "./types/TeamMember";
import type { BriefLines } from "./types/BriefLines";
import type { Client } from "./types/Client";
import type { NewClient } from "./types/NewClient";
import type { NewRole } from "./types/NewRole";
import type { RoleDetail } from "./types/RoleDetail";
import type { RoleSummary } from "./types/RoleSummary";
import type { RoleUpdate } from "./types/RoleUpdate";
import type { PullRequest } from "./types/PullRequest";
import type { SearchState } from "./types/SearchState";

/** Sent with every change; the server refuses changes without it. */
const CHANGE_HEADER = "X-Sourcer";

/** The session has ended (timed out, signed out elsewhere or switched off). */
export class SignedOut extends Error {
  constructor() {
    super("Your session has ended. Please sign in again.");
  }
}

/** Send JSON; on failure, throw the server's own message when it gave one. */
async function send<T>(method: string, path: string, body: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    credentials: "include",
    headers: { "Content-Type": "application/json", [CHANGE_HEADER]: "1" },
    body: JSON.stringify(body),
  });
  if (res.status === 401) throw new SignedOut();
  if (!res.ok) {
    // Our own refusals carry a message written for people.
    const text = [400, 403, 404, 409, 429, 502, 503].includes(res.status) ? await res.text() : "";
    if (!text && res.status === 403) throw new Error("Only an active admin can do this.");
    throw new Error(text || `Something went wrong (${res.status}). Please try again.`);
  }
  return (await res.json()) as T;
}

async function get<T>(path: string): Promise<T> {
  const res = await fetch(path, { credentials: "include" });
  if (res.status === 401) throw new SignedOut();
  if (!res.ok) throw new Error(`${path} failed: ${res.status}`);
  return (await res.json()) as T;
}

export const api = {
  health: () => get<Health>("/api/health"),

  /** The signed-in user, or null when nobody is signed in. */
  me: async (): Promise<Me | null> => {
    const res = await fetch("/api/me", { credentials: "include" });
    if (res.status === 401) return null;
    if (!res.ok) throw new Error(`/api/me failed: ${res.status}`);
    return (await res.json()) as Me;
  },

  team: () => get<TeamMember[]>("/api/team"),
  invite: (m: NewMember) => send<TeamMember>("POST", "/api/team", m),
  updateMember: (id: string, change: MemberUpdate) => send<TeamMember>("PATCH", `/api/team/${id}`, change),

  clients: () => get<Client[]>("/api/clients"),
  createClient: (c: NewClient) => send<Client>("POST", "/api/clients", c),
  roles: () => get<RoleSummary[]>("/api/roles"),
  role: (id: string) => get<RoleDetail>(`/api/roles/${id}`),
  createRole: (r: NewRole) => send<RoleDetail>("POST", "/api/roles", r),
  updateRole: (id: string, r: RoleUpdate) => send<RoleDetail>("PATCH", `/api/roles/${id}`, r),
  draftBrief: (id: string) => send<RoleDetail>("POST", `/api/roles/${id}/brief/draft`, {}),
  saveBrief: (id: string, lines: BriefLines) => send<RoleDetail>("PUT", `/api/roles/${id}/brief`, lines),
  /** `basedOn` is the brief version the editor loaded, so stale windows are refused. */
  confirmBrief: (id: string, lines: BriefLines, basedOn: number | null) =>
    send<RoleDetail>("POST", `/api/roles/${id}/brief/confirm`, { lines, based_on: basedOn }),

  search: (id: string) => get<SearchState>(`/api/roles/${id}/search`),
  /** `key` is fresh per press, so a repeated request never pays twice. */
  countMatches: (id: string, key: string) => send<SearchState>("POST", `/api/roles/${id}/search/count`, { key }),
  pull: (id: string, req: PullRequest) => send<SearchState>("POST", `/api/roles/${id}/search/pull`, req),

  logout: async (): Promise<void> => {
    const res = await fetch("/api/auth/logout", {
      method: "POST",
      credentials: "include",
      headers: { [CHANGE_HEADER]: "1" },
    });
    if (!res.ok) throw new Error(`Sign-out failed: ${res.status}`);
  },
};
