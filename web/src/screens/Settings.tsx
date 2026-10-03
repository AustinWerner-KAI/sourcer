import { useState } from "react";
import { useSearchParams } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../api/client";

/** What happened when Microsoft sent you back, in words. */
const OUTCOME: Record<string, { ok?: boolean; text: string }> = {
  connected: { ok: true, text: "Outlook connected. Approved emails go out from it in working hours." },
  "other-account": {
    text: "That was a different Microsoft account. Connect the one you sign in to Sourcer with.",
  },
  cancelled: { text: "Connecting was cancelled. Nothing changed." },
  expired: { text: "That took too long. Please try again." },
  failed: {
    text: "Microsoft did not finish connecting. Try again. If it keeps failing, ask your admin to check the Sourcer app's permissions in Entra.",
  },
  "no-offline": { text: "Microsoft did not allow Sourcer to stay connected. Ask your admin to add offline_access in Entra." },
  "not-set-up": { text: "Sending from Outlook is not set up yet." },
  "signed-out": { text: "Sign in to Sourcer first, then connect Outlook." },
};

/** Settings: your Outlook, how you sign off in emails, and the Chrome button. */
export function Settings({ isAdmin = false }: { isAdmin?: boolean }) {
  const queryClient = useQueryClient();
  const q = useQuery({ queryKey: ["outreach-settings"], queryFn: api.outreachSettings });
  const [signature, setSignature] = useState<string | null>(null);
  const [intro, setIntro] = useState<string | null>(null);
  const save = useMutation({
    mutationFn: () =>
      api.saveOutreachSettings({ signature: signature ?? q.data!.signature, intro: intro ?? q.data!.intro }),
    onSuccess: (s) => {
      queryClient.setQueryData(["outreach-settings"], s);
      setSignature(null);
      setIntro(null);
    },
  });
  const dirty = signature !== null || intro !== null;

  return (
    <main>
      <header>
        <div className="eyebrow">You</div>
        <h1>Settings</h1>
      </header>
      <Outlook isAdmin={isAdmin} />
      <section className="panel narrow">
        <h2 className="panel-title">Your emails</h2>
        <p className="panel-note">Used in every email you draft. Outlook does not add your signature, so Sourcer does.</p>
        {q.isLoading && <p className="panel-note">Loading</p>}
        {q.isError && <p className="form-error">Could not load your settings. Please refresh.</p>}
        {q.data && (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              save.mutate();
            }}
          >
            <label className="f">
              How you introduce yourself
              <input
                className="in"
                value={intro ?? q.data.intro}
                placeholder="I'm a Director at Austin Werner, a recruitment business"
                maxLength={200}
                onChange={(e) => setIntro(e.target.value)}
              />
            </label>
            <label className="f">
              Signature
              <textarea
                className="in sig"
                rows={11}
                value={signature ?? q.data.signature}
                onChange={(e) => setSignature(e.target.value)}
              />
              <span className="hint">Links: [LinkedIn](https://...) or a full https:// address.</span>
            </label>
            <button className="btn-ghost" type="submit" disabled={!dirty || save.isPending}>
              {save.isPending ? "Saving" : "Save"}
            </button>
            {save.isSuccess && !dirty && <span className="saved"> Saved</span>}
            {save.error && (
              <p className="form-error" role="alert">
                {save.error.message}
              </p>
            )}
          </form>
        )}
      </section>
      <section className="panel narrow">
        <h2 className="panel-title">Save LinkedIn profiles with the Chrome button</h2>
        <p className="panel-note">
          On someone's LinkedIn profile, click the Sourcer button, check the details and pick the role. They join that
          role's list and get ranked. It reads only the page you are on, only when you click.
        </p>
        <ol className="steps-list">
          <li>
            In Chrome, open <code>chrome://extensions</code> and turn on Developer mode.
          </li>
          <li>
            Click Load unpacked and choose the <code>sourcer/extension</code> folder.
          </li>
          <li>Pin the Sourcer button, open its Options and enter this Sourcer address:</li>
        </ol>
        <p className="addr">
          <code>{window.location.origin}</code>
        </p>
      </section>
    </main>
  );
}

/** Your Outlook: connect it once, and approved emails go out from it. */
function Outlook({ isAdmin }: { isAdmin: boolean }) {
  const queryClient = useQueryClient();
  const [params, setParams] = useSearchParams();
  const outcome = OUTCOME[params.get("outlook") ?? ""];
  const q = useQuery({ queryKey: ["mail"], queryFn: api.mailStatus });
  const disconnect = useMutation({
    mutationFn: api.disconnectMail,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["mail"] }),
  });
  const test = useMutation({ mutationFn: api.testMail });
  const m = q.data;
  const redirect = m?.redirect_uri || `${window.location.origin}/api/mail/callback`;

  return (
    <section className="panel narrow">
      <h2 className="panel-title">Your Outlook</h2>
      <p className="panel-note">
        Approved emails go out from your own mailbox, Monday to Friday, 8:00 to 18:00 Dubai time. Replies land in your
        Outlook as usual, and any reply stops the rest.
      </p>
      {outcome && (
        <div className={`notice${outcome.ok ? " ok" : " warn-notice"}`} role="status">
          {outcome.text}{" "}
          <button type="button" className="link-button inline" onClick={() => setParams({}, { replace: true })}>
            Close
          </button>
        </div>
      )}
      {q.isLoading && <p className="panel-note">Loading</p>}
      {q.isError && <p className="form-error">Could not load your Outlook connection. Please refresh.</p>}
      {m && !m.configured && (
        <>
          <p className="warnline">Sending from Outlook is not set up yet.</p>
          {isAdmin ? (
            <div className="setup">
              <p className="hint">An admin does this once, in Microsoft Entra:</p>
              <ol className="steps-list">
                <li>Open App registrations, then the Sourcer app.</li>
                <li>
                  Authentication: add this web redirect address: <code>{redirect}</code>
                </li>
                <li>
                  API permissions: add Microsoft Graph, Delegated: <code>Mail.Send</code>, <code>Mail.ReadWrite</code>{" "}
                  and <code>offline_access</code>. Then click Grant admin consent.
                </li>
                <li>Run the Sourcer update on the Mac. It makes the encryption key by itself.</li>
              </ol>
            </div>
          ) : (
            <p className="hint">Ask your admin to set it up.</p>
          )}
        </>
      )}
      {m && m.configured && !m.connected && (
        <a className="btn-primary btn-inline" href="/api/mail/connect">
          Connect Outlook
        </a>
      )}
      {m && m.configured && m.connected && (
        <div className={`switch${m.broken ? " off" : ""}`}>
          <div>
            <div className="sw-title">
              {m.address}
              <span className={`pill ${m.broken ? "pill-disabled" : "pill-active"}`}>
                {m.broken ? "Needs connecting" : "Connected"}
              </span>
            </div>
            <p className="sw-note">
              {m.broken
                ? m.broken
                : `${m.first_emails_today} of ${m.first_emails_per_day} first emails sent today. Follow-ups do not count.`}
            </p>
          </div>
          <span className="actions-row">
            {m.broken && (
              <a className="btn-ghost" href="/api/mail/connect">
                Connect again
              </a>
            )}
            <button
              type="button"
              className="rej"
              disabled={disconnect.isPending}
              onClick={() =>
                window.confirm("Disconnect Outlook? Approved emails wait until you connect it again.") &&
                disconnect.mutate()
              }
            >
              Disconnect
            </button>
          </span>
        </div>
      )}
      {m && m.configured && m.connected && !m.broken && (
        <div className="mailtest">
          <p className="hint">
            See exactly what a candidate gets: one sample first email, with your signature and the footer, sent to you.
          </p>
          <button type="button" className="btn-ghost" onClick={() => test.mutate()} disabled={test.isPending}>
            {test.isPending ? "Sending" : "Send a test to myself"}
          </button>
          {test.data && (
            <p className="ok-line" role="status">
              Sent to {test.data.sent_to}. Check your inbox.
            </p>
          )}
          {test.error && (
            <p className="form-error" role="alert">
              {test.error.message}
            </p>
          )}
        </div>
      )}
      {disconnect.error && (
        <p className="form-error" role="alert">
          {disconnect.error.message}
        </p>
      )}
    </section>
  );
}
