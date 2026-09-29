import type { Health } from "./types/Health";
import type { Me } from "./types/Me";
import type { MemberUpdate } from "./types/MemberUpdate";
import type { NewMember } from "./types/NewMember";
import type { TeamMember } from "./types/TeamMember";

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
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (res.status === 401) throw new SignedOut();
  if (res.status === 403) throw new Error("Only an active admin can do this.");
  if (!res.ok) {
    // Our own refusals (400, 404, 409) carry a message written for people.
    const text = [400, 404, 409].includes(res.status) ? await res.text() : "";
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

  logout: async (): Promise<void> => {
    const res = await fetch("/api/auth/logout", { method: "POST", credentials: "include" });
    if (!res.ok) throw new Error(`Sign-out failed: ${res.status}`);
  },
};
