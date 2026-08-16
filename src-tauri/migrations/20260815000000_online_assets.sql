-- Online assets, and media duration.
--
-- The FIRST migration after v1.0, and it carries two features on purpose. The
-- online-asset columns and `duration_ms` were designed together and shipped a
-- fortnight apart; three sequential migrations for what is one schema evolution
-- is fine at ten users and unpleasant at ten thousand.
--
-- SAME RULE AS THE FIRST FILE: once this has shipped, it is FROZEN. `sqlx`
-- checksums every byte, comments included, so editing this later makes every
-- library that already ran it refuse to open. Changes go in a NEW file.
--
-- ── The model ───────────────────────────────────────────────────────────────
--
-- An online asset is an ordinary asset whose BYTES ARE ELSEWHERE. Everything
-- else about it is normal: it has a row, a filename, tags, folders, a thumbnail,
-- a place in the manifest. That is what keeps the grid, the filters, search and
-- the viewer working on it without knowing anything about the network.
--
-- `origin` is the whole state machine, and it has exactly two values:
--
--   'local'   the bytes are at `path`. Everything Nova has ever done applies.
--   'remote'  there are no bytes; `remote_url` is where they live.
--
-- There is deliberately NO "remote but cached" third state. Bytes only ever
-- arrive through an explicit "Keep offline", which is permanent and flips the
-- row to 'local' — so "can I open this without a network?" is one column, not a
-- pair that can disagree.
--
-- `path` stays NOT NULL for a remote asset and holds the location the file WOULD
-- occupy. That keeps the original invariant intact and means promoting to local
-- already knows where to write.

-- 'local' | 'remote'. Defaulted, so every existing row is correct as written.
ALTER TABLE assets ADD COLUMN origin TEXT NOT NULL DEFAULT 'local';

-- Where the bytes live. DELIVERY, and deliberately not the same thing as
-- `source_url`, which is PROVENANCE ("I got this from here") and can be set on a
-- perfectly ordinary local file. Conflating them would make "which assets
-- actually need the network?" unanswerable.
ALTER TABLE assets ADD COLUMN remote_url TEXT;

-- NULL for local assets. For remote ones:
--   'ok'           the last check reached it
--   'unverified'   never checked, or not since `last_verified_at` aged out
--   'unavailable'  the last check failed — the link has rotted
-- Persisted rather than derived so a dead asset can be shown as dead without
-- re-hitting the network on every render.
ALTER TABLE assets ADD COLUMN remote_state TEXT;

ALTER TABLE assets ADD COLUMN last_verified_at TEXT;

-- Whether the origin honours HTTP range requests, learned once at add time.
-- Without it, seeking cannot work and the player must say so instead of
-- offering a scrubber that does nothing.
ALTER TABLE assets ADD COLUMN supports_range INTEGER;

-- Media length in milliseconds. NULL until something measures it: the webview
-- reports it alongside the captured keyframe, and the URL probe cannot know it.
ALTER TABLE assets ADD COLUMN duration_ms INTEGER;

-- PARTIAL, matching the shape of `idx_assets_hash` above it: online assets are
-- the rare case by design, so the index only carries them. It serves both the
-- "Online" filter and the bulk actions (re-check, download all) that operate on
-- exactly this set.
CREATE INDEX IF NOT EXISTS idx_assets_origin
    ON assets (origin) WHERE origin <> 'local';
