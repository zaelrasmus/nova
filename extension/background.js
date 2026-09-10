/**
 * Save to Nova — background event page.
 *
 * Three jobs, and the ordering between them is the whole design:
 *
 *   1. WAKE. Nova is not expected to be running. An extension cannot start a
 *      process, so `nova-connector` — a native messaging host registered at
 *      install time — is asked to find or start it. Native messaging is used for
 *      this and nothing else: its message-size limits make it the wrong channel
 *      for a 300MB capture, so the bytes go over HTTP.
 *
 *   2. CAPTURE. Send the URL and let Nova fetch it, which is cheap and reuses
 *      the import path Nova already has. If Nova's fetch is refused — the
 *      auth-gated case, and the reason an extension is worth building — Nova
 *      replies `needs_bytes` and we fetch it ourselves, with the cookies it
 *      doesn't have.
 *
 *   3. QUEUE. Anything that fails is kept and retried. Deliberately a short-term
 *      RETRY BUFFER holding URLs, not a store holding media: browser storage is
 *      evictable, and a queue that accumulates gigabytes for weeks is a storage
 *      system nobody designed.
 *
 * ONE file serves both browser families. Firefox MV3 runs it as an event page
 * (`background.scripts`); Chromium runs it as a service worker. Either way it
 * can be evicted between events, so nothing that matters may live in a
 * module-level variable — state goes to `storage.local`.
 */

/**
 * Firefox exposes `browser` (promise-based); Chromium exposes only `chrome`,
 * which is also promise-based under MV3. Picking `browser` first means Firefox
 * never falls back to its callback-style `chrome` alias.
 */
const ext = globalThis.browser ?? globalThis.chrome;

const NOVA_HOST = "com.nova.connector";
const PORTS = [41595, 41596, 41597, 41598];

/** Keys in storage.local. */
const K_TOKEN = "token";
const K_PORT = "port";
const K_QUEUE = "queue";
const K_FOLDER = "folderId";
const K_RECENT = "recent";

/** Retry buffer only — see the note at the top about why this stays small. */
const QUEUE_MAX = 50;
const RECENT_MAX = 10;

/**
 * Bytes we are willing to pull through the extension and push to Nova. Nova's
 * own cap is higher; this one is about the extension's memory, since the whole
 * response is held as a Blob before it is sent.
 */
const MAX_BYTES = 200 * 1024 * 1024;

// ── Storage helpers ──────────────────────────────────────────────────────────

async function get(key, fallback = null) {
  const bag = await ext.storage.local.get(key);
  return bag[key] ?? fallback;
}

async function set(key, value) {
  await ext.storage.local.set({ [key]: value });
}

// ── Reaching Nova ────────────────────────────────────────────────────────────

/**
 * Confirm a port belongs to a LIVING Nova.
 *
 * A bound port is not proof: Nova closes its listener first and tears down
 * after, so a socket can accept while the process behind it is already leaving.
 * Only a real answer counts.
 */
async function healthAt(port) {
  try {
    const res = await fetch(`http://127.0.0.1:${port}/health`, {
      method: "GET",
      cache: "no-store",
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

/**
 * Find Nova, starting it if necessary. Returns `{ port, health }`.
 *
 * Tries the remembered port first (the common case, and it costs one local
 * round trip), then the rest, and only then pays for a wake.
 */
async function ensureNova() {
  const remembered = await get(K_PORT);
  const candidates = remembered ? [remembered, ...PORTS.filter((p) => p !== remembered)] : PORTS;

  for (const port of candidates) {
    const health = await healthAt(port);
    if (health) {
      if (port !== remembered) await set(K_PORT, port);
      return { port, health };
    }
  }

  // Nothing listening: ask the connector to start it.
  let reply;
  try {
    reply = await ext.runtime.sendNativeMessage(NOVA_HOST, { cmd: "wake" });
  } catch (e) {
    // Almost always the host manifest not being registered — a setup problem
    // with a specific fix, so it must not be reported as "Nova is closed".
    throw new NovaError("no_connector", `Can't reach Nova's launcher: ${e.message}`);
  }

  if (!reply?.ok || !reply.port) {
    throw new NovaError("no_nova", reply?.error || "Nova could not be started.");
  }

  const health = await healthAt(reply.port);
  if (!health) throw new NovaError("no_nova", "Nova started but is not answering.");

  await set(K_PORT, reply.port);
  return { port: reply.port, health };
}

class NovaError extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}

async function api(port, path, { method = "GET", token, body, headers = {} } = {}) {
  const res = await fetch(`http://127.0.0.1:${port}${path}`, {
    method,
    // This header is why a web page cannot reach Nova: a custom header forces a
    // CORS preflight, and Nova answers that preflight only for a paired
    // extension origin.
    headers: { ...(token ? { "X-Nova-Token": token } : {}), ...headers },
    body,
  });
  const text = await res.text();
  let json = null;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    /* not json — fall through to the status check */
  }
  if (!res.ok) {
    throw new NovaError(json?.error || `http_${res.status}`, json?.message || `HTTP ${res.status}`);
  }
  return json;
}

// ── Pairing ──────────────────────────────────────────────────────────────────

/**
 * Ask Nova to pair. Nova parks the request until a human approves it in its own
 * window — which is the actual authentication here, and the reason a token
 * cannot be obtained by anything that merely reaches the port.
 */
async function pair() {
  const { port } = await ensureNova();
  const result = await api(port, "/pair", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: browserLabel() }),
  });
  await set(K_TOKEN, result.token);
  return result;
}

/**
 * What to call this browser in Nova's approval dialog.
 *
 * Best-effort, and it does not need to be better than that: the label is a
 * convenience for the human, while the thing actually pinned is the origin. A
 * wrong name here cannot grant anything.
 *
 * Brave deliberately does not identify itself in the user-agent — it mimics
 * Chrome — so `navigator.brave` is the only reliable tell.
 */
function browserLabel() {
  if (globalThis.navigator?.brave) return "Brave";
  const ua = navigator.userAgent ?? "";
  if (ua.includes("Zen")) return "Zen";
  if (ua.includes("Helium")) return "Helium";
  if (ua.includes("Firefox")) return "Firefox";
  if (ua.includes("Edg/")) return "Edge";
  if (ua.includes("Chrome")) return "Chrome";
  return "Browser";
}

// ── Capturing ────────────────────────────────────────────────────────────────

function newId() {
  return crypto.randomUUID();
}

/**
 * Save one item.
 *
 * The `id` travels with the capture and is the basis of retry safety: a request
 * that fails at the connection level is indistinguishable from one that
 * succeeded and lost its reply, so a resend carries the SAME id and Nova answers
 * from memory rather than importing twice.
 */
async function save(capture) {
  const token = await get(K_TOKEN);
  if (!token) throw new NovaError("not_paired", "Pair with Nova first.");

  const { port, health } = await ensureNova();
  if (!health.library) {
    throw new NovaError("no_library", "Nova is running but no library is open.");
  }

  // A folder chosen in the editor wins over the popup's standing default: it was
  // picked for THIS capture, moments ago.
  const folderId =
    capture.folderId !== undefined ? capture.folderId : await get(K_FOLDER, null);
  const result = await api(port, "/capture", {
    method: "POST",
    token,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      id: capture.id,
      url: capture.url,
      page_url: capture.pageUrl,
      title: capture.title,
      folder_id: folderId,
      // Names, not ids — Nova resolves them and creates any that are new.
      tags: capture.tags ?? [],
      link_only: capture.linkOnly === true,
      // A blob: URL means nothing outside the page that made it, so there is no
      // point asking Nova to try fetching it.
      bytes: capture.url.startsWith("blob:") || capture.url.startsWith("data:"),
    }),
  });

  if (result?.status !== "needs_bytes") return result;

  // Nova could not fetch it. That is usually not a dead link — it is content
  // behind a login, which we can reach and Nova cannot.
  return await sendBytes(port, token, capture, result.ticket);
}

/**
 * Fetch the media ourselves and hand Nova the bytes.
 *
 * `credentials: "include"` is the entire point: the request carries the session
 * that makes the content reachable. `activeTab`, granted by the context-menu
 * click, is what permits it for the page's own origin.
 */
async function sendBytes(port, token, capture, ticket) {
  if (!ticket) throw new NovaError("failed", "Nova asked for bytes but issued no ticket.");

  let blob;
  try {
    const res = await fetch(capture.url, { credentials: "include", cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    blob = await res.blob();
  } catch (e) {
    throw new NovaError("fetch_failed", `Couldn't download that media: ${e.message}`);
  }

  if (blob.size === 0) throw new NovaError("failed", "That media came back empty.");
  if (blob.size > MAX_BYTES) {
    throw new NovaError(
      "too_large",
      `That file is ${Math.round(blob.size / 1024 / 1024)}MB — too large to send this way. Download it and drag it into Nova.`,
    );
  }

  return await api(port, `/capture/${capture.id}/upload`, {
    method: "PUT",
    // The TICKET, not the token: single-use and scoped to this one capture.
    headers: {
      "X-Nova-Ticket": ticket,
      "X-Nova-Filename": safeHeader(capture.title || "capture"),
      "X-Nova-Source": safeHeader(capture.pageUrl || capture.url),
    },
    body: blob,
  });
}

/** Headers must be latin-1; page titles are frequently not. */
function safeHeader(value) {
  return value.replace(/[^\x20-\x7E]/g, "").slice(0, 300) || "capture";
}

// ── The retry queue ──────────────────────────────────────────────────────────

async function enqueue(capture) {
  const queue = await get(K_QUEUE, []);
  if (queue.some((q) => q.id === capture.id)) return;
  queue.push(capture);
  await set(K_QUEUE, queue.slice(-QUEUE_MAX));
  await refreshBadge();
}

/**
 * Drain the queue. Called on startup, after a successful save, and from the
 * popup — never on a timer, because a timer would wake Nova on its own schedule
 * and the point is that Nova sleeps.
 */
async function flush() {
  const queue = await get(K_QUEUE, []);
  if (queue.length === 0) return { sent: 0, left: 0 };

  const left = [];
  let sent = 0;
  for (const capture of queue) {
    try {
      const result = await save(capture);
      await remember(capture, result);
      sent += 1;
    } catch (e) {
      // A capture that cannot possibly succeed later is dropped rather than
      // retried forever; anything else stays.
      if (e.code === "too_large" || e.code === "not_media") continue;
      left.push(capture);
      // One unreachable Nova means the rest will fail identically.
      if (e.code === "no_nova" || e.code === "no_connector" || e.code === "not_paired") {
        left.push(...queue.slice(queue.indexOf(capture) + 1));
        break;
      }
    }
  }

  await set(K_QUEUE, left);
  await refreshBadge();
  return { sent, left: left.length };
}

async function refreshBadge() {
  const queue = await get(K_QUEUE, []);
  const text = queue.length > 0 ? String(queue.length) : "";
  await ext.action.setBadgeText({ text });
  if (text) {
    await ext.action.setBadgeBackgroundColor({ color: "#f59e0b" });
  }
}

async function remember(capture, result) {
  const recent = await get(K_RECENT, []);
  recent.unshift({
    title: result?.filename || capture.title || capture.url,
    status: result?.status || "saved",
    at: Date.now(),
  });
  await set(K_RECENT, recent.slice(0, RECENT_MAX));
}

// ── The details overlay ──────────────────────────────────────────────────────

/**
 * The editor is a SECOND verb, never the default.
 *
 * "Save to Nova" stays instant — a right-click and it is done. If filling in a
 * form were the only way, the feature would have rebuilt the very friction it
 * exists to remove. So the ellipsis item opens this instead, and the plain one
 * never asks anything.
 *
 * Rendered as an in-page overlay rather than the toolbar popup because
 * `action.openPopup()` is unreliable on Chromium and unavailable on Firefox — and
 * an overlay opens instantly, keeps focus, and can be styled like Nova.
 *
 * This function is INJECTED, so it cannot close over anything in this file:
 * everything it needs arrives in `args`, and it returns a plain object.
 */
function novaDetailsOverlay({ title, folders, host }) {
  return new Promise((resolve) => {
    const existing = document.getElementById("nova-capture-overlay");
    if (existing) existing.remove();

    const root = document.createElement("div");
    root.id = "nova-capture-overlay";
    // A very high z-index and `all: initial` on the shell: this is injected into
    // somebody else's page, and page CSS must not be able to reach in.
    root.style.cssText =
      "all: initial; position: fixed; inset: 0; z-index: 2147483647;" +
      "display: grid; place-items: center; background: rgba(0,0,0,.55);" +
      "font: 13px/1.5 system-ui, -apple-system, Segoe UI, sans-serif;";

    const panel = document.createElement("div");
    panel.style.cssText =
      "width: 360px; max-width: calc(100vw - 32px); background: #0a0a0a;" +
      "border: 1px solid #262626; border-radius: 12px; padding: 16px;" +
      "box-shadow: 0 20px 60px rgba(0,0,0,.5); color: #e5e5e5;";

    const label = (text) => {
      const el = document.createElement("label");
      el.textContent = text;
      el.style.cssText =
        "display:block; margin-bottom:4px; color:#737373; font-size:11px;";
      return el;
    };
    const field = () => {
      const el = document.createElement("input");
      el.type = "text";
      el.style.cssText =
        "width:100%; box-sizing:border-box; padding:6px 8px; margin-bottom:12px;" +
        "background:#171717; border:1px solid #262626; border-radius:6px;" +
        "color:#e5e5e5; font:inherit; font-size:12px;";
      return el;
    };

    const heading = document.createElement("div");
    heading.textContent = "Save to Nova";
    heading.style.cssText = "font-weight:600; margin-bottom:2px;";
    const sub = document.createElement("div");
    sub.textContent = host;
    sub.style.cssText =
      "color:#737373; font-size:11px; margin-bottom:14px; overflow:hidden;" +
      "text-overflow:ellipsis; white-space:nowrap;";

    const titleInput = field();
    titleInput.value = title || "";
    const tagsInput = field();
    tagsInput.placeholder = "comma, separated";

    const folderSelect = document.createElement("select");
    folderSelect.style.cssText =
      "width:100%; box-sizing:border-box; padding:6px 8px; margin-bottom:14px;" +
      "background:#171717; border:1px solid #262626; border-radius:6px;" +
      "color:#e5e5e5; font:inherit; font-size:12px;";
    for (const folder of [{ id: "", name: "Library root" }, ...folders]) {
      const option = document.createElement("option");
      option.value = folder.id ?? "";
      option.textContent = folder.name;
      folderSelect.appendChild(option);
    }

    const row = document.createElement("div");
    row.style.cssText = "display:flex; gap:8px;";
    const button = (text, primary) => {
      const el = document.createElement("button");
      el.type = "button";
      el.textContent = text;
      el.style.cssText =
        "flex:1; padding:7px 10px; border-radius:6px; font:inherit; font-size:12px;" +
        "cursor:pointer; " +
        (primary
          ? "background:#2563eb; border:1px solid transparent; color:#fff; font-weight:500;"
          : "background:#171717; border:1px solid #262626; color:#e5e5e5;");
      return el;
    };
    const cancel = button("Cancel", false);
    const link = button("Link only", false);
    const save = button("Save", true);
    row.append(cancel, link, save);

    panel.append(
      heading,
      sub,
      label("Title"),
      titleInput,
      label("Tags"),
      tagsInput,
      label("Folder"),
      folderSelect,
      row,
    );
    root.appendChild(panel);
    document.documentElement.appendChild(root);
    titleInput.focus();
    titleInput.select();

    const close = (value) => {
      window.removeEventListener("keydown", onKey, true);
      root.remove();
      resolve(value);
    };
    const collect = (linkOnly) => ({
      title: titleInput.value.trim(),
      tags: tagsInput.value
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean),
      folderId: folderSelect.value || null,
      linkOnly,
    });

    // Capture phase, so a page that swallows keydown cannot trap the user in a
    // dialog they did not ask for.
    const onKey = (e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close(null);
      } else if (e.key === "Enter" && e.target !== folderSelect) {
        e.stopPropagation();
        close(collect(false));
      }
    };
    window.addEventListener("keydown", onKey, true);

    cancel.addEventListener("click", () => close(null));
    link.addEventListener("click", () => close(collect(true)));
    save.addEventListener("click", () => close(collect(false)));
    root.addEventListener("click", (e) => {
      if (e.target === root) close(null);
    });
  });
}

/**
 * Show the editor in the page and return what the user chose, or null.
 *
 * Injected on demand rather than declared as a content script: it is needed on
 * one tab, once, at the moment of a right-click — and `activeTab` (granted by
 * that very click) is enough, so the extension never asks for blanket access to
 * every site you visit.
 */
async function askForDetails(tabId, capture) {
  let folders = [];
  try {
    const token = await get(K_TOKEN);
    const { port, health } = await ensureNova();
    if (token && health.library) {
      folders = (await api(port, "/folders", { token }))?.folders ?? [];
    }
  } catch {
    // Nova unreachable. Still worth showing the dialog: the capture will queue
    // with its title and tags intact and file itself correctly later.
  }

  let host = "";
  try {
    host = new URL(capture.pageUrl || capture.url).host;
  } catch {
    host = capture.url.slice(0, 60);
  }

  try {
    const [injected] = await ext.scripting.executeScript({
      target: { tabId },
      func: novaDetailsOverlay,
      args: [{ title: capture.title, folders, host }],
    });
    return injected?.result ?? null;
  } catch (e) {
    // Injection is refused on privileged pages (about:, the add-ons store, PDF
    // viewers). Falling back to a plain save is better than doing nothing.
    console.warn("Nova: could not show the details dialog:", e);
    return {};
  }
}

// ── Entry points ─────────────────────────────────────────────────────────────

function notify(title, message) {
  ext.notifications
    .create({ type: "basic", iconUrl: ext.runtime.getURL("icons/128.png"), title, message })
    .catch(() => {
      /* notifications are a courtesy, never a dependency */
    });
}

/**
 * Three verbs, and the ordering is the design: the plain one is instant and
 * asks nothing, because that is the whole point of the extension. The ellipsis
 * one — by long convention — is the one that opens a dialog.
 */
const MENUS = [
  { id: "nova-save", title: "Save to Nova", contexts: ["image", "video", "audio"] },
  { id: "nova-details", title: "Save to Nova...", contexts: ["image", "video", "audio"] },
  {
    id: "nova-link",
    title: "Save link to Nova (don't download)",
    contexts: ["image", "video", "audio"],
  },
];

ext.runtime.onInstalled.addListener(() => {
  ext.contextMenus.removeAll().then(() => {
    for (const menu of MENUS) ext.contextMenus.create(menu);
  });
  refreshBadge();
});

ext.runtime.onStartup.addListener(() => {
  refreshBadge();
});

ext.contextMenus.onClicked.addListener(async (info, tab) => {
  if (!MENUS.some((m) => m.id === info.menuItemId)) return;

  const capture = {
    id: newId(),
    // `srcUrl` is what the user actually right-clicked. `linkUrl` would be the
    // anchor around it, which is usually a page, not the media.
    url: info.srcUrl,
    pageUrl: info.pageUrl || tab?.url || "",
    title: tab?.title || "",
    linkOnly: info.menuItemId === "nova-link",
  };

  if (!capture.url) {
    notify("Nothing to save", "Nova couldn't work out which media you clicked.");
    return;
  }

  // The editor runs BEFORE anything is sent, so cancelling costs nothing and
  // Nova is never woken for a capture the user abandoned.
  if (info.menuItemId === "nova-details") {
    const details = await askForDetails(tab?.id, capture);
    if (details === null) return; // cancelled
    if (details.title) capture.title = details.title;
    if (details.tags?.length) capture.tags = details.tags;
    if (details.folderId !== undefined) capture.folderId = details.folderId;
    capture.linkOnly = details.linkOnly === true;
  }

  try {
    const result = await save(capture);
    await remember(capture, result);
    if (result?.status === "duplicate") {
      notify("Already in Nova", "That file is already in your library.");
    } else if (result?.status === "linked") {
      notify("Link saved", "Nova stored the link without downloading it.");
    } else {
      notify("Saved to Nova", result?.filename || capture.title || "Capture saved.");
    }
    // A successful save proves Nova is up, which is the best moment to retry
    // anything that failed while it was not.
    await flush();
  } catch (e) {
    if (e.code === "not_paired") {
      notify("Pair with Nova first", "Open the Nova extension and choose Pair.");
      return;
    }
    await enqueue(capture);
    notify(
      "Queued for Nova",
      e.code === "no_library"
        ? "Open a library in Nova and it will import."
        : "Nova isn't reachable — it'll be saved next time it is.",
    );
  }
});

// The popup is a separate document and cannot share these functions directly.
ext.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  // Chromium MV3 does NOT honour a Promise returned from a listener; it wants
  // `return true` plus a later sendResponse. Firefox accepts both forms, so
  // this is the only one that works in either.
  handleMessage(message).then(sendResponse, (e) => sendResponse({ error: e.message }));
  return true;
});

async function handleMessage(message) {
  switch (message?.cmd) {
    case "status": {
      const token = await get(K_TOKEN);
      const queue = await get(K_QUEUE, []);
      const recent = await get(K_RECENT, []);
      const folderId = await get(K_FOLDER, null);
      try {
        const { port, health } = await ensureNova();
        let folders = [];
        if (token && health.library) {
          try {
            folders = (await api(port, "/folders", { token }))?.folders || [];
          } catch {
            /* a folder list is a convenience, not a reason to report failure */
          }
        }
        return {
          state: !token ? "unpaired" : health.library ? "ready" : "no_library",
          port,
          queue: queue.length,
          recent,
          folders,
          folderId,
        };
      } catch (e) {
        return { state: e.code === "no_connector" ? "no_connector" : "no_nova", message: e.message, queue: queue.length, recent, folders: [], folderId };
      }
    }
    case "pair":
      try {
        await pair();
        return { ok: true };
      } catch (e) {
        return { ok: false, code: e.code, message: e.message };
      }
    case "flush":
      return await flush();
    case "setFolder":
      await set(K_FOLDER, message.folderId || null);
      return { ok: true };
    case "unpair":
      await ext.storage.local.remove(K_TOKEN);
      return { ok: true };
    default:
      return null;
  }
}
