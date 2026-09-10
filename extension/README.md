# Save to Nova — browser extension

Right-click any image, video or audio on the web and it lands in your local Nova
library. Nova does not need to be open: the extension starts it in the
background, and if that fails the capture is queued and imported next time Nova
is reachable.

Works on **Zen and Firefox**, and on **Brave, Helium, Chrome and Chromium**. One
codebase: every line of JS, CSS and HTML is shared, and only the manifest differs
— how the background is declared, and how the extension identifies itself to the
native messaging host.

## How it fits together

```
  right-click ──► extension ──► nova-connector ──► starts nova.exe --tray
                      │           (native messaging, wake only)
                      └────────► http://127.0.0.1:41595 ──► the library
                                 (loopback HTTP, all data)
```

Two channels on purpose. Native messaging is the only way a browser will start a
process for you, but it is JSON over stdio with message-size limits — wrong for
a 300MB capture. So it is used to *wake* Nova and nothing else; the bytes go
over a loopback socket.

The extension never touches `library.db`. It cannot — extensions have no SQLite —
and it must not: writing rows behind Nova's back would leave the search index
unsynced, skip thumbnail generation and bypass folder auto-tags. Nova stays the
only writer.

## Install (development)

Nova ships the connector as a Tauri sidecar and registers it as a native
messaging host on every start (`src-tauri/src/bridge/host.rs`), so there is no
longer a build step or a registration script to run first. Just have a Nova
build that exists.

For testers, see `BETA-SETUP.md` and `.\package-beta.ps1`, which assembles the
whole handout.

**Load the extension (Zen / Firefox)**

Temporarily, for a quick iteration:

1. `about:debugging#/runtime/this-firefox`
2. **Load Temporary Add-on...** -> pick `extension/manifest.json`

Removed when the browser closes. For day-to-day use build a real package
instead, which survives restarts:

```powershell
cd extension
.\build-xpi.ps1
```

Then `about:config` -> `xpinstall.signatures.required` -> **false**, and
`about:addons` -> gear -> **Install Add-on From File...** -> `dist\save-to-nova.xpi`.

Firefox randomises the `moz-extension://` origin per *install*, so reinstalling
the XPI produces a new origin and Nova will ask you to pair again. That is the
origin pinning working, not a bug.

### Chromium instead (Helium · Brave · Chrome)

```powershell
cd extension
node build-chromium.mjs        # assembles dist\chromium
```

Then `chrome://extensions` (or `brave://extensions`) → **Developer mode** →
**Load unpacked** → pick `extension\dist\chromium`.

Unlike Firefox, Chromium extensions can pin their id: `manifest.chromium.json`
carries a public `key`, so the id is always
`helhjhnfgjfdkbbmojccpckkffafkamm` and the host manifest can name a stable
`chrome-extension://` origin. That means Chromium gets real origin pinning,
where Firefox has to fall back to the token alone.

Each Chromium vendor reads its own registry path — `BraveSoftware\Brave-Browser`,
`Helium\Helium`, `Google\Chrome`, `Chromium` — so all four are written. A key for
a browser you have not installed is harmless.

`register-host.ps1` and `register-host-chromium.ps1` are kept for the case where
you want to point the browser at a connector Nova did not register itself, such
as a debug build sitting somewhere else. They are not part of normal setup.

**Pair**

Open Nova, then click the extension's toolbar icon and choose **Pair with Nova**.
The request appears in Nova under **Settings › Browser extension** for you to
allow.

Pairing needs Nova's *window* open, because approving is what authenticates the
connection and there has to be somewhere to approve it. It is a one-time step —
afterwards Nova can be started windowless and never shown.

## Using it

- **Save to Nova** — downloads the media into your library.
- **Save link to Nova** — records the URL without downloading, for something
  large you want catalogued but not stored.

The toolbar icon shows a badge when captures are waiting, and its popup reports
which of the four states you are in: connected, not paired, no library open, or
Nova not running. Each has a different fix, so they are never collapsed into a
single error.

## What it does about awkward media

**Content behind a login** is the reason an extension is worth having. Nova
fetches most URLs itself, cheaply. When a fetch is refused — a 403 from a
hotlink-protected CDN, or anything gated by a session — Nova asks the extension
for the bytes instead, and the extension refetches with the page's cookies.

Not handled, deliberately: **HLS and DASH streams** (YouTube, most Twitter
video). Those are not files, and reassembling segments is a different feature
from saving one. **DRM and canvas-rendered** content cannot be captured at all.
`blob:` URLs skip straight to the bytes path, since the URL means nothing
outside the page that made it.

## Security

A loopback port is reachable by any page in any browser, so there are two
independent barriers.

Every endpoint requires the `X-Nova-Token` header. A custom header makes the
request non-simple, which forces a CORS preflight — and Nova answers preflights
only for a paired extension origin, so a web page's request is never sent. This
matters more than it sounds: a *simple* cross-origin POST is delivered and
executed with only its response hidden, which is exactly how CSRF works.

Behind that is a 256-bit token, minted only after a human approves the request
in Nova's own window. That covers everything that is not a browser.

Then: the `Host` header must be the `127.0.0.1` literal (`localhost` is rejected
— a name can be rebound), the socket binds loopback only, and the API is
capture-shaped. There is no endpoint that lists a library, reads an asset or
runs a query, so the worst case of a total auth bypass is an unwanted image
arriving *in* the library rather than anything leaving it.

Firefox randomises `moz-extension://` origins per installation, so there is no
stable origin to allowlist in advance. Nova records whichever origin paired and
pins it; if the browser sends none, the token carries the weight alone.

## Files

| | |
|---|---|
| `manifest.json` | MV3, Firefox flavour — event page, `gecko.id` = `nova@local`. Load this directly. |
| `manifest.chromium.json` | MV3, Chromium flavour — service worker, `key` pinning the id |
| `background.js` | wake, capture, bytes fallback, retry queue — shared |
| `popup.html/js/css` | status, pairing, folder picker — shared |
| `build-chromium.mjs` | assembles `dist/chromium` (copy step; there is nothing to bundle) |
| `build-xpi.ps1` | packs a real `.xpi` that survives a browser restart |
| `package-beta.ps1` | rebuilds everything and assembles `dist/beta` for testers |
| `BETA-SETUP.md` | the tester-facing guide; ships inside that bundle |
| `register-host*.ps1` | manual host registration — only for pointing at a connector Nova did not register itself |
| `../src-tauri/connector/` | the launcher stub, its own crate |
| `../src-tauri/src/bridge/` | the server it talks to |
| `../src-tauri/src/bridge/host.rs` | the self-registration that replaced the scripts |

Two things are gitignored because they are per-machine rather than per-repo: the
generated host manifests (they contain an absolute path to your build) and
`.chromium-key.pem`. That key is only needed to publish a CRX with the *same*
id later; the public half is in `manifest.chromium.json` and is not secret.

### Writing PowerShell for this

Both `.ps1` files are deliberately **pure ASCII**. Windows PowerShell 5.1 reads
script files as ANSI unless they carry a BOM, so a UTF-8 em dash inside a string
literal decodes to a byte that ends the string early and breaks the parser — a
genuinely baffling failure, and one this repo hit. For the same family of reason,
both scripts write their JSON manifests without a BOM: `Set-Content -Encoding utf8`
adds one on 5.1, and a BOM makes the browser report the host as not existing at
all, which sends you hunting through the registry rather than the file.
