// Where Sourcer runs. Chrome asks the user to allow this one address.
const $ = (id) => document.getElementById(id);

chrome.storage.sync.get("origin").then(({ origin }) => {
  if (origin) $("origin").value = origin;
});

$("form").onsubmit = async (e) => {
  e.preventDefault();
  const result = $("result");
  let origin;
  try {
    const u = new URL($("origin").value.trim());
    if (u.protocol !== "http:" && u.protocol !== "https:") throw new Error();
    origin = u.origin;
  } catch {
    result.className = "msg warn";
    result.textContent = "That does not look like a web address, e.g. http://sourcer-office:8080";
    result.hidden = false;
    return;
  }
  const granted = await chrome.permissions.request({ origins: [origin + "/*"] });
  if (!granted) {
    result.className = "msg warn";
    result.textContent = "Chrome did not allow it. Try again and press Allow.";
    result.hidden = false;
    return;
  }
  await chrome.storage.sync.set({ origin });
  result.className = "msg ok";
  result.textContent = `Saved. Open a LinkedIn profile and click the Sourcer button.`;
  result.hidden = false;
};
