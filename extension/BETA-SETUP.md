# Save to Nova - beta setup

Right-click any image, video or audio on the web and it lands in your Nova
library, tagged, without a download folder in between.

This is a prototype. It is not signed and not in any extension store, so setup
has a few more steps than a normal add-on, and your browser will warn you at
least once. That is expected - the warnings are about *who published this*, not
about what it does.

**Supported for this beta: Zen and Helium.** Firefox and Chrome will not accept
an unsigned extension permanently; see "Why the warnings" at the end.

---

## 1. Install Nova

Run `nova_0.1.0_x64-setup.exe`.

Windows will show **"Windows protected your PC"**. Click **More info** ->
**Run anyway**. This appears because the installer is not code-signed yet.

Open Nova once and open a library before continuing. The extension saves *into*
a library, so there has to be one.

## 2. Install the extension

### Zen (or any Firefox-based browser)

1. Go to `about:config`, accept the warning.
2. Search for `xpinstall.signatures.required` and set it to **false**
   (double-click the row to flip it).
3. Go to `about:addons`.
4. Click the **gear icon** at the top right -> **Install Add-on From File...**
5. Pick `save-to-nova.xpi`.
6. Accept the permission prompt.

The add-on stays installed across restarts. You only do this once.

### Helium (or any Chromium-based browser)

1. Unzip `save-to-nova-chromium.zip` somewhere you will not delete - the
   browser loads it from that folder every launch, so the Downloads folder is
   a bad choice.
2. Go to `chrome://extensions`.
3. Turn on **Developer mode** (top right).
4. Click **Load unpacked** and pick the folder you unzipped.

## 3. Pair it

Pairing is what authorises the connection, and it needs Nova's window open so
there is somewhere to approve it.

1. Make sure Nova is open with a library loaded.
2. Click the extension's toolbar icon -> **Pair with Nova**.
3. Switch to Nova. Under **Settings > Browser extension** a request appears
   showing the extension's name and origin.
4. Click **Allow**.

One time only. After this Nova can be closed and the extension will start it
again by itself.

---

## Using it

Right-click an image, video or audio element:

- **Save to Nova** - saves immediately into your chosen folder.
- **Save to Nova...** - opens a small panel first so you can set the title,
  tags and destination folder before saving.
- **Save link to Nova (don't download)** - stores the URL only. Nothing is
  downloaded. Useful for a large video you want recorded but not copied.

The toolbar popup lets you pick the destination folder, see recent saves, and
retry anything that queued while Nova was closed.

**Nothing is ever downloaded without you asking.** Pasting or opening a URL
does not trigger a save.

## Nova closing on its own

After you close Nova's window it keeps running quietly so saves are instant,
then exits by itself. While waiting it uses about 30MB and no browser engine.

You can change how long it waits, or turn it off, in
**Settings > Browser extension > Stay in the background for**. Options run from
1 minute to "Stay open". It never exits while the window is open or while work
is in progress.

If Nova is fully closed when you save, the capture is queued and the extension
starts Nova to deliver it.

---

## What to report

Useful things to tell us:

- A site where the right-click menu does not appear, or saves the wrong image.
- Anything that saves with the wrong filename or extension.
- A capture that stays stuck in the queue.
- Nova exiting while you were still using it, or not exiting at all.
- Anything in **Settings > Browser extension** that reads as confusing.

Please include the site and the browser.

## Known limits

- Some sites serve images as `.webp`. That is what the site actually sent and
  Nova stores it as-is; it is not an error. Nova reads WebP everywhere.
- Sites that require a login may serve a placeholder to the extension instead
  of the real file.
- Link-only saves record the URL. If that URL later expires, the link stops
  resolving - the file was never copied.

## Why the warnings

Nothing here is signed yet. Windows SmartScreen warns about the installer, and
Zen needs `xpinstall.signatures.required=false` before it will accept the XPI.

Stock **Firefox** ignores that setting and refuses unsigned add-ons outright,
and **Chrome** disables developer-mode extensions on every launch. That is why
this beta asks for Zen and Helium specifically. Signing through addons.mozilla.org
and the Chrome Web Store removes all of it, and is on the list before release.

The connection itself is local only: Nova listens on 127.0.0.1, no other machine
can reach it, and only a browser you approved by hand can save to it. You can
revoke that at any time in **Settings > Browser extension**.
