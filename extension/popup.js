// Save the LinkedIn profile in the active tab to a Sourcer role (SRS F10).
// Reads the page only when the resourcer clicks the button (activeTab), never
// visits LinkedIn itself, and sends nothing until they press Save.

const PROFILE = /^https:\/\/([a-z]{2,3}\.)?linkedin\.com\/in\/[^/?#]+/i;
const $ = (id) => document.getElementById(id);
const show = (id) => ($(id).hidden = false);

/** Runs inside the LinkedIn tab: the name, headline and location on screen. */
function readProfile() {
  const text = (sel) => (document.querySelector(sel)?.innerText || "").trim();
  const name = text("main h1") || text("h1") || document.title.split("|")[0].trim();
  const headline = text("main .text-body-medium") || text(".text-body-medium");
  const place = text("main .text-body-small.inline") || text(".text-body-small.inline");
  return { name, headline, location: place };
}

/** "Senior IAM Engineer at Examplepay" into a title and an employer. */
function splitHeadline(headline) {
  const first = (headline || "").split("|")[0].trim();
  const m = first.match(/^(.*?)\s+(?:at|@)\s+(.+)$/i);
  return m ? { title: m[1].trim(), employer: m[2].trim() } : { title: first, employer: "" };
}

async function sourcer(origin, path, init = {}) {
  const res = await fetch(origin + path, {
    credentials: "include",
    ...init,
    headers: { "Content-Type": "application/json", "X-Sourcer": "1", ...(init.headers || {}) },
  });
  return res;
}

async function main() {
  const { origin } = await chrome.storage.sync.get("origin");
  const allowed = origin && (await chrome.permissions.contains({ origins: [origin + "/*"] }));
  if (!allowed) {
    show("setup");
    $("open-options").onclick = () => chrome.runtime.openOptionsPage();
    return;
  }

  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || !PROFILE.test(tab.url || "")) {
    show("not-profile");
    return;
  }

  const rolesRes = await sourcer(origin, "/api/roles").catch(() => null);
  if (!rolesRes || rolesRes.status === 401) {
    $("sign-in").href = origin + "/";
    show("signed-out");
    return;
  }
  if (!rolesRes.ok) throw new Error(`Sourcer answered ${rolesRes.status}`);
  // Only roles that can take people: a confirmed brief and a client to keep out.
  const roles = (await rolesRes.json()).filter((r) => r.brief_state === "confirmed" && r.client_name);
  if (roles.length === 0) {
    $("to-briefs").href = origin + "/brief";
    show("no-roles");
    return;
  }

  // If the page cannot be read, the resourcer types the details instead.
  const page = await chrome.scripting
    .executeScript({ target: { tabId: tab.id }, func: readProfile })
    .then((r) => r?.[0]?.result || {})
    .catch(() => ({}));
  const { title, employer } = splitHeadline(page.headline);
  $("name").value = page.name || "";
  $("title").value = title;
  $("employer").value = employer;
  $("location").value = (page.location || "").split(",")[0].trim();

  const { lastRole } = await chrome.storage.local.get("lastRole");
  for (const r of roles) {
    const o = document.createElement("option");
    o.value = r.id;
    o.textContent = r.client_name ? `${r.title} · ${r.client_name}` : r.title;
    o.selected = r.id === lastRole;
    $("role").append(o);
  }
  show("form");
  $("name").focus();

  $("form").onsubmit = async (e) => {
    e.preventDefault();
    const roleId = $("role").value;
    const roleName = $("role").selectedOptions[0]?.textContent || "the role";
    $("save").disabled = true;
    $("save").textContent = "Saving";
    const result = $("result");
    try {
      const res = await sourcer(origin, "/api/people/save", {
        method: "POST",
        body: JSON.stringify({
          role_id: roleId,
          linkedin_url: tab.url,
          name: $("name").value,
          title: $("title").value || null,
          employer: $("employer").value || null,
          location: $("location").value || null,
        }),
      });
      if (!res.ok) {
        result.className = "msg warn";
        result.textContent =
          res.status === 401 ? "Your Sourcer session ended. Sign in again, then retry." : (await res.text()) || `Could not save (${res.status}).`;
        result.hidden = false;
        $("save").disabled = false;
        $("save").textContent = "Save to Sourcer";
        return;
      }
      const saved = await res.json();
      await chrome.storage.local.set({ lastRole: roleId });
      const c = saved.candidate;
      const lines = [saved.added ? `Saved to ${roleName}. Claude ranks them in about a minute.` : `Already on ${roleName}.`];
      if (c.do_not_contact) lines.push("On the do-not-contact list: never contact.");
      if (c.known) lines.push(`${c.known}. You decide on the Candidates screen.`);
      result.className = c.do_not_contact || c.known ? "msg warn" : "msg ok";
      result.textContent = lines.join(" ") + " ";
      const open = document.createElement("a");
      open.href = `${origin}/brief/${roleId}/candidates`;
      open.target = "_blank";
      open.rel = "noopener";
      open.textContent = "Open candidates";
      result.append(open);
      result.hidden = false;
      $("form").hidden = true;
    } catch {
      result.className = "msg warn";
      result.textContent = "Could not reach Sourcer. Check you are on the office network, then retry.";
      result.hidden = false;
      $("save").disabled = false;
      $("save").textContent = "Save to Sourcer";
    }
  };
}

main().catch(() => {
  const result = $("result");
  result.className = "msg warn";
  result.textContent = "Something went wrong. Close this and click the button again.";
  result.hidden = false;
});
