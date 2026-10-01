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
import type { CandidateRow } from "./types/CandidateRow";
import type { CandidateTab } from "./types/CandidateTab";
import type { CandidatesView } from "./types/CandidatesView";
import type { Decision } from "./types/Decision";
import type { RecruitlyStatus } from "./types/RecruitlyStatus";
import type { RecruitlyTest } from "./types/RecruitlyTest";
import type { RecruitlyJob } from "./types/RecruitlyJob";
import type { JobPreview } from "./types/JobPreview";
import type { ImportRole } from "./types/ImportRole";
import type { HandoverResult } from "./types/HandoverResult";
import type { RetuneView } from "./types/RetuneView";
import type { CvView } from "./types/CvView";
import type { FeedbackRequest } from "./types/FeedbackRequest";
import type { OutreachView } from "./types/OutreachView";
import type { OutreachStepEdit } from "./types/OutreachStepEdit";
import type { OutreachSettings } from "./types/OutreachSettings";

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
  if (!res.ok) throw await failure(res);
  return (await res.json()) as T;
}

/** The server's own message when it gave one, else a plain one. */
async function failure(res: Response): Promise<Error> {
  // Our own refusals carry a message written for people.
  const text = [400, 403, 404, 409, 422, 429, 502, 503].includes(res.status) ? await res.text() : "";
  if (!text && res.status === 403) return new Error("Only an active admin can do this.");
  return new Error(text || `Something went wrong (${res.status}). Please try again.`);
}

async function get<T>(path: string): Promise<T> {
  const res = await fetch(path, { credentials: "include" });
  if (res.status === 401) throw new SignedOut();
  if (!res.ok) throw await failure(res);
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
  /** Round 2: Claude's reading of a thin count and a relaxed brief. Saves and searches nothing. */
  retune: (id: string) => send<RetuneView>("POST", `/api/roles/${id}/search/retune`, {}),

  candidates: (id: string, tab: CandidateTab) => get<CandidatesView>(`/api/roles/${id}/candidates?tab=${tab}`),
  rankNow: (id: string) => send<CandidatesView>("POST", `/api/roles/${id}/candidates/rank`, {}),
  /** `version` is the one the screen showed, so two people cannot overwrite each other. */
  decide: (candidacy: string, d: Decision) => send<CandidateRow>("POST", `/api/candidates/${candidacy}/decide`, d),

  recruitlyStatus: () => get<RecruitlyStatus>("/api/recruitly/status"),
  recruitlyTest: () => send<RecruitlyTest>("POST", "/api/recruitly/test", {}),
  recruitlyJobs: (q: string) => get<RecruitlyJob[]>(`/api/recruitly/jobs?q=${encodeURIComponent(q)}`),
  recruitlyJob: (jobId: string) => get<JobPreview>(`/api/recruitly/jobs/${encodeURIComponent(jobId)}`),
  importRole: (r: ImportRole) => send<RoleDetail>("POST", "/api/roles/from-recruitly", r),
  linkJob: (roleId: string, jobId: string | null) =>
    send<RoleDetail>("PUT", `/api/roles/${roleId}/recruitly`, { job_id: jobId }),
  recruitlyCheck: (candidacy: string) => send<CandidateRow>("POST", `/api/candidates/${candidacy}/recruitly-check`, {}),
  /** `confirmed` holds the keys of the questions the resourcer has said yes to. */
  handover: (candidacy: string, confirmed: string[]) =>
    send<HandoverResult>("POST", `/api/candidates/${candidacy}/handover`, { confirmed }),

  cv: (candidacy: string) => get<CvView>(`/api/candidates/${candidacy}/cv`),
  /** The file is sent as it is; the server removes the name and contact details before anything is kept. */
  uploadCv: async (candidacy: string, file: File, otherRoles: string[]): Promise<CvView> => {
    const q = new URLSearchParams({ name: file.name, roles: otherRoles.join(",") });
    const res = await fetch(`/api/candidates/${candidacy}/cv?${q}`, {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/octet-stream", [CHANGE_HEADER]: "1" },
      body: file,
    });
    if (res.status === 401) throw new SignedOut();
    if (!res.ok) throw await failure(res);
    return (await res.json()) as CvView;
  },
  assessCv: (candidacy: string, roleIds: string[]) =>
    send<CvView>("POST", `/api/candidates/${candidacy}/cv/assess`, { role_ids: roleIds }),
  cvToRecruitly: (candidacy: string) => send<CvView>("POST", `/api/candidates/${candidacy}/cv/recruitly`, {}),
  cvFeedback: async (assessment: string, f: FeedbackRequest): Promise<void> => {
    const res = await fetch(`/api/cv-assessments/${assessment}/feedback`, {
      method: "PUT",
      credentials: "include",
      headers: { "Content-Type": "application/json", [CHANGE_HEADER]: "1" },
      body: JSON.stringify(f),
    });
    if (res.status === 401) throw new SignedOut();
    if (!res.ok) throw await failure(res);
  },

  outreach: (candidacy: string) => get<OutreachView>(`/api/candidates/${candidacy}/outreach`),
  /** Draft the three emails. `fresh` replaces an existing draft or a stopped sequence. */
  draftOutreach: (candidacy: string, fresh = false) =>
    send<OutreachView>("POST", `/api/candidates/${candidacy}/outreach${fresh ? "?fresh=true" : ""}`, {}),
  saveOutreach: (candidacy: string, version: number, steps: OutreachStepEdit[]) =>
    send<OutreachView>("PUT", `/api/candidates/${candidacy}/outreach`, { version, steps }),
  /** One approval for all three emails. Nothing is sent until Outlook is connected. */
  approveOutreach: (candidacy: string, version: number) =>
    send<OutreachView>("POST", `/api/candidates/${candidacy}/outreach/approve`, { version }),
  stopOutreach: (candidacy: string, version: number) =>
    send<OutreachView>("POST", `/api/candidates/${candidacy}/outreach/stop`, { version }),
  outreachSettings: () => get<OutreachSettings>("/api/me/outreach"),
  saveOutreachSettings: (s: OutreachSettings) => send<OutreachSettings>("PUT", "/api/me/outreach", s),

  logout: async (): Promise<void> => {
    const res = await fetch("/api/auth/logout", {
      method: "POST",
      credentials: "include",
      headers: { [CHANGE_HEADER]: "1" },
    });
    if (!res.ok) throw new Error(`Sign-out failed: ${res.status}`);
  },
};
