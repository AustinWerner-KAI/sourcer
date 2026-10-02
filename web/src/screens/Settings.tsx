import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../api/client";

/** Settings: how you sign off in emails, and the Chrome button. */
export function Settings() {
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
