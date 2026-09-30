import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../api/client";
import type { RecruitlyLink } from "../api/types/RecruitlyLink";

/** Whether Recruitly is set up, shared by every screen that offers it. */
export function useRecruitly() {
  return useQuery({ queryKey: ["recruitly-status"], queryFn: api.recruitlyStatus, staleTime: 60_000 });
}

/** "Senior IAM Engineer (J-1042)" gives "J-1042"; a label without a reference gives "the job". */
export function jobRef(link: RecruitlyLink | null | undefined): string {
  if (!link) return "Recruitly";
  const m = /\(([^()]+)\)\s*$/.exec(link.label);
  return m ? m[1] : "the job";
}

/** "2 hours ago" from seconds since 1970. */
export function ago(seconds: number, now = Date.now()): string {
  const mins = Math.max(0, Math.round((now / 1000 - seconds) / 60));
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins} ${mins === 1 ? "minute" : "minutes"} ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours} ${hours === 1 ? "hour" : "hours"} ago`;
  const days = Math.round(hours / 24);
  return `${days} ${days === 1 ? "day" : "days"} ago`;
}

/**
 * Find a Recruitly job. Shows the newest jobs first; a search costs one call.
 * `onPick` gets the job id. Jobs already made into roles link to them instead.
 */
export function JobSearch({ onPick, onCancel }: { onPick: (id: string) => void; onCancel?: () => void }) {
  const [words, setWords] = useState("");
  const [asked, setAsked] = useState("");
  const jobs = useQuery({
    queryKey: ["recruitly-jobs", asked],
    queryFn: () => api.recruitlyJobs(asked),
    staleTime: 60_000,
    retry: false,
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setAsked(words.trim());
  };

  return (
    <div className="jobsearch">
      <form className="jobfind" onSubmit={submit} role="search">
        <label className="f grow">
          Find the Recruitly job
          <input
            className="in"
            value={words}
            onChange={(e) => setWords(e.target.value)}
            placeholder="Job title, reference or client"
            maxLength={100}
          />
        </label>
        <button className="btn-ghost" type="submit" disabled={jobs.isFetching}>
          {jobs.isFetching ? "Searching" : "Search"}
        </button>
        {onCancel && (
          <button type="button" className="link-button" onClick={onCancel}>
            Cancel
          </button>
        )}
      </form>
      {jobs.isError && (
        <p className="form-error" role="alert">
          {jobs.error.message}
        </p>
      )}
      {jobs.data && jobs.data.length === 0 && (
        <p className="note">{asked ? `No Recruitly jobs match "${asked}".` : "No jobs in Recruitly yet."}</p>
      )}
      {jobs.data && jobs.data.length > 0 && (
        <ul className="joblist" aria-label={asked ? `Jobs matching ${asked}` : "Newest jobs"}>
          {jobs.data.map((j) => (
            <li key={j.id} className="job">
              <div>
                <div className="t">{j.title}</div>
                <div className="m">
                  {[j.company, j.reference && `Ref ${j.reference}`, j.status, j.location].filter(Boolean).join(" · ")}
                </div>
              </div>
              {j.role_id ? (
                <Link className="link-button" to={`/brief/${j.role_id}`}>
                  Open its role
                </Link>
              ) : (
                <button type="button" className="btn-ghost" onClick={() => onPick(j.id)}>
                  Use this job
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Where people sent from this role land in Recruitly, with a way to link or change it. */
export function RecruitlyJobBar({ roleId, link }: { roleId: string; link: RecruitlyLink | null }) {
  const queryClient = useQueryClient();
  const [picking, setPicking] = useState(false);
  const save = useMutation({
    mutationFn: (jobId: string | null) => api.linkJob(roleId, jobId),
    onSuccess: () => {
      setPicking(false);
      queryClient.invalidateQueries({ queryKey: ["role", roleId] });
      queryClient.invalidateQueries({ queryKey: ["candidates", roleId] });
      queryClient.invalidateQueries({ queryKey: ["recruitly-jobs"] });
    },
  });

  return (
    <div className="rankstate">
      {link ? (
        <span>
          Recruitly job: <strong>{link.label}</strong>. People you add go into its pipeline.
        </span>
      ) : (
        <span>No Recruitly job linked. Link one so people you add go into its pipeline.</span>
      )}
      {!picking && (
        <button type="button" className="link-button" onClick={() => setPicking(true)}>
          {link ? "Change" : "Link a job"}
        </button>
      )}
      {link && !picking && (
        <button type="button" className="link-button" onClick={() => save.mutate(null)} disabled={save.isPending}>
          Unlink
        </button>
      )}
      {picking && <JobSearch onPick={(id) => save.mutate(id)} onCancel={() => setPicking(false)} />}
      {save.error && (
        <p className="form-error" role="alert">
          {save.error.message}
        </p>
      )}
    </div>
  );
}

/** Admins: is Recruitly connected, and how much of today's allowance is used. */
export function RecruitlyPanel() {
  const status = useRecruitly();
  const test = useMutation({ mutationFn: api.recruitlyTest });
  const s = status.data;
  if (!s) return null;
  const near = s.calls_today >= s.daily_cap * 0.8;

  return (
    <section className="panel">
      <h2 className="panel-title">Recruitly</h2>
      {!s.configured ? (
        <p>
          Not set up. Add <code>RECRUITLY_API_KEY</code> to the settings file (from Recruitly: My Profile, then API
          Keys) and restart Sourcer.
        </p>
      ) : (
        <>
          <p className="panel-note">
            The key stays on the server. People added to Recruitly are owned by whoever adds them, matched by email.
          </p>
          <div className="stats">
            <div className="stat">
              <div className="v">{s.calls_today.toLocaleString("en-GB")}</div>
              <div className="k">calls today</div>
            </div>
            <div className="stat">
              <div className="v">{s.daily_cap.toLocaleString("en-GB")}</div>
              <div className="k">daily cap (the plan allows 10,000)</div>
            </div>
            <div className={`stat${near ? " warn" : ""}`}>
              <div className="v">{near ? "Near the cap" : "Fine today"}</div>
              <div className="k">{near ? "Checks and adds stop at the cap until tomorrow." : "Plenty left."}</div>
            </div>
          </div>
          <div className="actions-row">
            <button className="btn-ghost" onClick={() => test.mutate()} disabled={test.isPending}>
              {test.isPending ? "Testing" : "Test connection"}
            </button>
            {test.data && (
              <span className="ok-text" role="status">
                Connected as {test.data.connected_as}.
              </span>
            )}
          </div>
          {test.error && (
            <p className="form-error" role="alert">
              {test.error.message}
            </p>
          )}
        </>
      )}
    </section>
  );
}
