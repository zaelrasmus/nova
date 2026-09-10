/**
 * Popup — status, pairing, and the folder captures land in.
 *
 * Holds no logic of its own: it asks the background page, which owns the
 * connection, the token and the queue. A popup is destroyed every time it
 * closes, so anything it remembered would be lost anyway.
 */

/** See the note in background.js — Firefox has `browser`, Chromium only `chrome`. */
const ext = globalThis.browser ?? globalThis.chrome;

const el = (id) => document.getElementById(id);

/**
 * The four states, with the fix for each written into the copy. "Nova isn't
 * running" and "no library is open" look identical from here if you only report
 * failure, and they are fixed in completely different places.
 */
const STATES = {
  ready: {
    dot: "ok",
    headline: "Connected",
    detail: "Right-click any image, video or audio and choose Save to Nova.",
  },
  unpaired: {
    dot: "warn",
    headline: "Not paired",
    detail: "Pair once to let this browser save into your library. Nova must be open to approve it.",
  },
  no_library: {
    dot: "warn",
    headline: "No library open",
    detail: "Nova is running but hasn't got a library open. Captures will queue until it does.",
  },
  no_nova: {
    dot: "",
    headline: "Nova isn't running",
    detail: "Saving still works — captures queue and import the next time Nova is reachable.",
  },
  no_connector: {
    dot: "warn",
    headline: "Launcher not registered",
    detail: "Run register-host.ps1 from Nova's extension folder so the browser can start Nova.",
  },
};

async function ask(cmd, extra = {}) {
  return await ext.runtime.sendMessage({ cmd, ...extra });
}

function renderRecent(recent) {
  if (!recent?.length) {
    el("recentBlock").hidden = true;
    return;
  }
  el("recentBlock").hidden = false;
  el("recent").replaceChildren(
    ...recent.map((entry) => {
      const li = document.createElement("li");
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = entry.title;
      const status = document.createElement("span");
      status.className = "status";
      status.textContent = entry.status === "imported" ? "saved" : entry.status;
      li.append(name, status);
      return li;
    }),
  );
}

function renderFolders(folders, selected) {
  const select = el("folder");
  const options = [{ id: "", name: "Library root" }, ...folders];
  select.replaceChildren(
    ...options.map((folder) => {
      const option = document.createElement("option");
      option.value = folder.id ?? "";
      option.textContent = folder.name;
      option.selected = (folder.id ?? "") === (selected ?? "");
      return option;
    }),
  );
}

async function refresh() {
  const status = await ask("status");
  const shape = STATES[status.state] ?? STATES.no_nova;

  el("dot").className = `dot ${shape.dot}`;
  el("headline").textContent = shape.headline;
  el("detail").textContent = shape.detail;

  const paired = status.state !== "unpaired";
  el("pair").hidden = paired;
  el("unpair").hidden = !paired;
  el("retry").hidden = status.queue === 0 || status.state === "unpaired";
  el("actions").hidden = el("pair").hidden && el("retry").hidden;

  // Only meaningful when there is a library to file things into.
  const canPick = status.state === "ready";
  el("folderField").hidden = !canPick;
  if (canPick) renderFolders(status.folders ?? [], status.folderId);

  el("queue").textContent =
    status.queue > 0 ? `${status.queue} waiting to import` : "Nothing queued";

  renderRecent(status.recent);
}

el("pair").addEventListener("click", async () => {
  const button = el("pair");
  button.disabled = true;
  button.textContent = "Waiting for Nova…";
  const result = await ask("pair");
  button.disabled = false;
  button.textContent = "Pair with Nova";
  if (!result?.ok) {
    // The two interesting failures are both actionable, so say which it was.
    el("detail").textContent =
      result?.code === "no_window"
        ? "Open Nova's window and try again — approving needs it."
        : result?.message || "Pairing failed.";
    return;
  }
  await refresh();
});

el("retry").addEventListener("click", async () => {
  const button = el("retry");
  button.disabled = true;
  button.textContent = "Retrying…";
  const result = await ask("flush");
  button.disabled = false;
  button.textContent = "Retry queued";
  if (result?.left > 0) {
    el("detail").textContent = `${result.sent} imported, ${result.left} still waiting.`;
  }
  await refresh();
});

el("unpair").addEventListener("click", async () => {
  await ask("unpair");
  await refresh();
});

el("folder").addEventListener("change", async (event) => {
  await ask("setFolder", { folderId: event.target.value || null });
});

refresh();
