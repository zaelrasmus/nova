CREATE TABLE IF NOT EXISTS folders (
   id TEXT PRIMARY KEY NOT NULL UNIQUE,
   name TEXT NOT NULL,
   parent_id TEXT,
   position REAL NOT NULL DEFAULT 0,
   -- Free-text user notes. Named to match assets.notes: it's the same field for
   -- the same purpose, and the inspector edits both through one control.
   notes TEXT,
   -- Same format as the `stamp()` writer in assets.rs (RFC 3339, millis, Z), so
   -- folder timestamps sort and compare against asset timestamps as plain text.
   created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
   order_by TEXT NOT NULL DEFAULT 'manual',
   is_ascending INTEGER NOT NULL DEFAULT 1, -- 0 = false, 1 = true

   -- Sidebar pin accent. Stores a palette TOKEN NAME ('blue', 'emerald', …),
   -- never a hex value, so a theme change retints every pinned folder without
   -- rewriting the database. Validated against assets::PIN_COLORS on write.
   color TEXT,
   -- Rank among pinned folders (fractional, same scheme as `position`).
   -- NULL = not pinned. One nullable column rather than a boolean + a rank,
   -- because that makes "pinned without a place in the order" unrepresentable.
   pin_position REAL,

   FOREIGN KEY(parent_id) REFERENCES folders(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS assets (
    id TEXT PRIMARY KEY NOT NULL UNIQUE,
    asset_type TEXT NOT NULL, -- 'image', 'video', 'audio', "etc."
    path TEXT NOT NULL,

    width INTEGER NOT NULL DEFAULT 0,
    height INTEGER NOT NULL DEFAULT 0,
    pixel_count INTEGER GENERATED ALWAYS AS (width * height) VIRTUAL,

    manual_position REAL NOT NULL DEFAULT 0,

    filename TEXT NOT NULL,
    file_size INTEGER NOT NULL DEFAULT 0,
    extension TEXT NOT NULL,
    imported_date TEXT NOT NULL,
    modified_date TEXT NOT NULL,
    creation_date TEXT NOT NULL,

    -- Free-text user notes.
    notes TEXT,

    -- Where the asset came from, if the user downloaded it. Purely a record the
    -- user keeps by hand today; the download-from-URL path will populate it
    -- automatically when that lands. Stored permissively (any text), but only
    -- ever OPENED when the scheme is http/https — it's user data being handed to
    -- the OS shell, and file:// or a custom protocol handler is a real vector.
    source_url TEXT,

    -- BLAKE3 fingerprint of the file's bytes, hex-encoded. Import checks this
    -- before copying: re-importing bytes the library already holds links the
    -- EXISTING asset to wherever the duplicate was headed instead of writing a
    -- second copy to disk. Matters most for drag & drop, where re-dropping the
    -- same folder is a weekly accident rather than a deliberate act.
    --
    -- Nullable on purpose. Hashing is best-effort — a file we could not read
    -- still imports (never drop a user's file), it just never participates in
    -- dedup. Rows written before this column existed are NULL for the same
    -- reason, which is why the index below is PARTIAL: a plain UNIQUE index
    -- treats NULLs as distinct in SQLite, but being explicit documents that
    -- "unhashed" is a real state rather than an oversight.
    content_hash TEXT,

    -- TESTING Columns
    thumb_hash TEXT, -- base64 ThumbHash (NULL until generated)
    thumb_config TEXT, -- recipe tag, e.g. "webp:auto" (staleness marker)
    is_animated INTEGER NOT NULL DEFAULT 0 -- 1 if multi-frame (GIF/animated) source
);

CREATE TABLE IF NOT EXISTS assets_folders (
    folder_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    added_at TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    position REAL NOT NULL DEFAULT 0,

    PRIMARY KEY (folder_id, asset_id),
    FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE CASCADE,
    FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS view_settings (
    view_key TEXT PRIMARY KEY NOT NULL,
    order_by TEXT NOT NULL DEFAULT 'imported_date',
    is_ascending INTEGER NOT NULL DEFAULT 0

);

INSERT OR IGNORE INTO view_settings (view_key, order_by, is_ascending) VALUES ('all', 'imported_date', 0),
     ('uncategorized', 'imported_date', 0);

-- Named, reusable filter combinations. A saved filter is a LENS (it narrows
-- whatever scope you're in), not a place — it has no parent, no sort and no
-- position in the folder tree. A smart folder, when that lands, is the opposite:
-- a scope that owns its own sort and sits in the tree, so it gets its own table.
--
-- One table with a JSON document rather than a filters/filter_conditions pair.
-- The Rust `FilterSet` already IS the filter language and round-trips through
-- serde for free, so it stays the single source of truth; the normalized form
-- would need a hand-written encode/decode/validate layer kept in sync with it by
-- discipline alone, and its `value TEXT` column is untyped either way. SQLite's
-- json_each() covers querying across saved filters if that's ever needed.
-- Stored rule sets: ONE table behind two products.
--
-- A rule set is a tree of all/any/none groups over conditions (see rules.rs).
-- The same document is used two ways, and `kind` is the only thing that differs:
--
--   'smart'  — a Smart Folder. A PLACE in the sidebar: its tree becomes the
--              scope predicate, and it owns a persisted sort like any folder.
--   'filter' — a Saved Filter. A LENS: its tree narrows whatever scope you're
--              already in, and it owns nothing.
--
-- They stay two concepts in the UI on purpose — a smart folder is for an ongoing
-- workflow, a saved filter is a repeatable query you don't want cluttering the
-- sidebar — but they share the storage, the compiler and the editor, because
-- underneath they are the same sentence about assets.
CREATE TABLE IF NOT EXISTS rule_sets (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('smart', 'filter')),

    -- Deliberately NOT unique: two groups may each hold an "Approved", exactly
    -- as two folders may. (SQLite's UNIQUE on TEXT is also case-sensitive, so it
    -- would block the legitimate collision and permit the confusing one.)
    name TEXT NOT NULL,
    -- Named to match folders.notes and assets.notes: same field, same purpose,
    -- edited through the same inspector control.
    notes TEXT,

    -- Ungroup on delete, never cascade: removing a group must not destroy the
    -- user's saved queries.
    group_id TEXT REFERENCES rule_set_groups(id) ON DELETE SET NULL,

    -- List ordering (fractional rank, same idea as folders.position).
    position REAL NOT NULL DEFAULT 0,

    -- Schema version of query_json. When the rule language changes, old
    -- documents get migrated deliberately instead of silently mis-parsed, and a
    -- row written by a NEWER build is skipped rather than misread.
    -- v1 = the flat FilterSet; v2 = the rule tree.
    version INTEGER NOT NULL DEFAULT 2,

    -- Serialized RuleNode.
    query_json TEXT NOT NULL,

    -- Sidebar pin, same semantics as folders: NULL = unpinned, and the accent
    -- survives unpinning so re-pinning restores the look. Only meaningful for
    -- kind = 'smart' — a lens has nowhere to be pinned to.
    color TEXT,
    pin_position REAL,

    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_rule_sets_order ON rule_sets (kind, position, name);
CREATE INDEX IF NOT EXISTS idx_rule_sets_pinned
    ON rule_sets (pin_position) WHERE pin_position IS NOT NULL;

-- Manual order INSIDE a smart folder.
--
-- Folders get this free: `assets_folders` physically exists per (folder, asset)
-- and carries a position. A smart folder's membership is computed, so there is
-- no row to hang a rank on — hence this table.
--
-- It is SPARSE on purpose. A row exists only for an asset someone has actually
-- placed; everything else sorts after the ranked block (see the ORDER BY in
-- build_manifest_query). That gives append-on-arrival semantics for free: an
-- asset that starts matching after an import lands at the bottom, exactly where
-- a folder would put it, with nothing having to notice it arrived.
--
-- Rows for assets that stop matching are pruned when the folder is next opened
-- in manual order — removing a tag is an intentional act, and a returning asset
-- should come back at the bottom rather than resurrect its old slot.
CREATE TABLE IF NOT EXISTS smart_folder_order (
    smart_folder_id TEXT NOT NULL REFERENCES rule_sets(id) ON DELETE CASCADE,
    asset_id        TEXT NOT NULL REFERENCES assets(id)    ON DELETE CASCADE,
    position        REAL NOT NULL,

    PRIMARY KEY (smart_folder_id, asset_id)
) WITHOUT ROWID;

-- Sidebar containers for smart folders. A group is browsable — clicking one
-- shows the UNION of its members — so it owns a sort, which lives in
-- view_settings under 'smartgroup:<id>' rather than here.
CREATE TABLE IF NOT EXISTS rule_set_groups (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    notes TEXT,
    position REAL NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

-- Dominant colors per asset, in CIELAB, with the share of the image each covers.
--
-- A palette rather than one dominant color: an image that is 60% sky and 30%
-- sunset has a single dominant color (blue) and would be unfindable by searching
-- orange, even though a third of it is orange. Matching tests EVERY entry.
--
-- Populated during thumbnail generation (the pixels are already decoded there,
-- so it's nearly free) and backfilled by the "Analyze colors" pass. An asset with
-- no rows here simply hasn't been analyzed yet — the UI reports that count rather
-- than letting a color filter quietly under-report.
CREATE TABLE IF NOT EXISTS asset_colors (
    asset_id TEXT NOT NULL,
    l REAL NOT NULL,
    a REAL NOT NULL,
    b REAL NOT NULL,
    -- Share of the sampled pixels, 0.0-1.0.
    ratio REAL NOT NULL,

    FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE
);

-- Distance matching is an expression over three columns, so no index can serve
-- it; this one exists for the asset_id lookup inside the EXISTS subquery.
CREATE INDEX IF NOT EXISTS idx_asset_colors ON asset_colors (asset_id);

-- ── Tags ──────────────────────────────────────────────────────────────────────
--
-- A tag is a LENS on assets, never a place: applying one narrows the current
-- scope through the FilterSet, exactly like shape or size. Tags never apply to
-- folders. Groups are pure organization — a tag belongs to at most one — and are
-- flat (no nesting), matching the Tag Manager's own sidebar.

-- Optional grouping of tags (the manager's "Groups"). Deleting a group must NOT
-- delete its tags: SET NULL leaves them ungrouped. Cascading here would turn one
-- click into silent mass tag loss (and, through assets_tags, lost assignments).
CREATE TABLE IF NOT EXISTS tag_groups (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    color TEXT,
    -- Sibling ordering in the manager (fractional rank, same idea as folders).
    position REAL NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS tags (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,

    -- Own color overrides the group's; NULL inherits it, then a neutral default.
    color TEXT,

    -- At most one group. SET NULL, never CASCADE — see tag_groups above.
    group_id TEXT REFERENCES tag_groups(id) ON DELETE SET NULL,

    -- Pinned in the manager's "Starred" view. Column present from the start so
    -- the T4 feature needs no migration; unused until then.
    is_starred INTEGER NOT NULL DEFAULT 0,

    position REAL NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Global name uniqueness, case-INSENSITIVE: "Red" and "red" are the same tag, so
-- create-on-the-fly resolves to the existing row instead of spawning a near-dupe.
-- A UNIQUE INDEX (not a column constraint) so the collation is explicit.
CREATE UNIQUE INDEX IF NOT EXISTS idx_tags_name ON tags (name COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_tags_group ON tags (group_id, position);

CREATE TABLE IF NOT EXISTS assets_tags (
    asset_id TEXT NOT NULL,
    tag_id TEXT NOT NULL,
    -- When the tag was applied, for the "recently used" suggestion. Same RFC 3339
    -- shape as every other timestamp so it compares as plain text.
    added_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    PRIMARY KEY (asset_id, tag_id),
    FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE,
    FOREIGN KEY (tag_id) REFERENCES tags(id) ON DELETE CASCADE
);

-- The composite PK already serves asset -> tags (the inspector). EVERY other tag
-- operation — usage counts, filtering, merge, the whole manager — is tag ->
-- assets, so it needs the reverse index or each one is a full scan.
CREATE INDEX IF NOT EXISTS idx_assets_tags_reverse ON assets_tags (tag_id, asset_id);

-- Dedup lookup, and the guarantee behind it: two rows can never claim the same
-- bytes. PARTIAL so the unhashed rows described on the column are exempt rather
-- than colliding with each other.
CREATE UNIQUE INDEX IF NOT EXISTS idx_assets_hash
    ON assets (content_hash) WHERE content_hash IS NOT NULL;

-- Membership indexes
CREATE INDEX IF NOT EXISTS idx_folder_contents ON assets_folders(folder_id, added_at);
CREATE INDEX IF NOT EXISTS idx_folders_position ON assets_folders (folder_id, position); -- Serve a folder's assets already ordered by manual position
CREATE INDEX IF NOT EXISTS idx_folders_tree ON folders(parent_id, position);
-- Partial: the sidebar reads the pinned folders on every load, and pins are a
-- handful of rows out of potentially thousands. Indexing only the non-NULLs
-- keeps it tiny and makes the ordered read an index scan.
CREATE INDEX IF NOT EXISTS idx_folders_pinned
    ON folders(pin_position) WHERE pin_position IS NOT NULL;


-- Sort indexes: one composite (sortcol, id) per exposed sort mode. The trailing
-- id matches the query's tie-breaker so deep scans stay ordered by the index.
-- SQLite scans an index backwards, so one index serves both directions.
CREATE INDEX IF NOT EXISTS idx_assets_imported ON assets (imported_date, id);
CREATE INDEX IF NOT EXISTS idx_assets_created  ON assets (creation_date, id);
CREATE INDEX IF NOT EXISTS idx_assets_modified ON assets (modified_date, id);
CREATE INDEX IF NOT EXISTS idx_assets_size     ON assets (file_size, id);
CREATE INDEX IF NOT EXISTS idx_assets_pixels   ON assets (pixel_count, id);
CREATE INDEX IF NOT EXISTS idx_assets_manual   ON assets (manual_position, id);
-- COLLATE NOCASE must match the ORDER BY's collation or SQLite silently ignores
-- this index and full-sorts instead.
CREATE INDEX IF NOT EXISTS idx_assets_filename ON assets (filename COLLATE NOCASE, id);

-- ── Full-text search ─────────────────────────────────────────────────────────
--
-- One denormalised row per asset holding every searchable field, so a search
-- across name/note/url/folders/tags is ONE MATCH instead of a cross-table join
-- (the "Option A" decision). The trigram tokenizer gives fast SUBSTRING/infix
-- matching (not typo tolerance — proven in the fts5_probe tests). Scope toggles
-- map onto FTS5 column filters (`{name note} : term`).
--
-- Kept in sync by `search::reindex_assets`, called from every Rust function that
-- mutates searchable text; it's a derived cache, always rebuildable from the
-- source tables via `rebuild_search_index`. `asset_id` is UNINDEXED — stored so
-- a MATCH can return it, but not itself tokenised.
--
-- folder_text is the DIRECT folders' names only (not the ancestor chain), so a
-- rename reindexes just that folder's members, not the whole subtree.
CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
    asset_id UNINDEXED,
    name,
    extension,
    note,
    url,
    folder_text,  -- names of the asset's direct folders
    folder_note,  -- notes of those folders (the "Folder description" scope)
    tag_text,
    tokenize = 'trigram'
);

-- ── Quick Actions ────────────────────────────────────────────────────────────
--
-- A quick action is a MACRO: a named, ordered pipeline of mutation steps applied
-- to a snapshotted selection.
--
-- Where a smart folder is a PLACE and a saved filter is a LENS, an action is a
-- VERB — it changes assets rather than describing them. That's why it lives in
-- the grid toolbar (with search, filter and sort, which also act on the current
-- view) and never in the sidebar, which is navigation.
--
-- `steps_json` is a document for the same reason `rule_sets.query_json` is: the
-- Rust enum IS the step language, serde round-trips it for free, and the wire
-- shape is pinned by tests. A normalised steps/step_values pair would need a
-- hand-written encode/decode/validate layer kept in sync by discipline alone,
-- and its `value TEXT` column would be untyped either way.
CREATE TABLE IF NOT EXISTS quick_actions (
    id TEXT PRIMARY KEY NOT NULL UNIQUE,
    name TEXT NOT NULL,
    -- Lucide icon name, so the menu shows intent before the label is read.
    icon TEXT,
    -- Palette TOKEN ('blue', 'emerald', …), never a hex value — same convention
    -- as folders.color, so a theme change retints without rewriting rows.
    color TEXT,
    -- 1..9, bound to Ctrl+Shift+<n>. NULL = no shortcut.
    shortcut INTEGER,
    position REAL NOT NULL DEFAULT 0,
    steps_json TEXT NOT NULL,
    -- Bumped when the step language changes shape. An action written by a newer
    -- Nova must refuse to run rather than silently drop a step it cannot parse:
    -- a macro that quietly does less than it says is worse than one that stops.
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Two actions can never claim one chord. The conflict rule is "you can't",
-- enforced here rather than left to whichever keydown handler registers first.
-- PARTIAL because SQLite treats NULLs as distinct: unbound actions don't collide.
CREATE UNIQUE INDEX IF NOT EXISTS idx_quick_actions_shortcut
    ON quick_actions (shortcut) WHERE shortcut IS NOT NULL;

-- One row per EXECUTION. Gives undo something to name in the toast ("Tagged
-- 4,231 assets · Undo") and lets the log be pruned by age without parsing
-- payloads.
CREATE TABLE IF NOT EXISTS action_runs (
    id TEXT PRIMARY KEY NOT NULL UNIQUE,
    -- ON DELETE SET NULL, not CASCADE: deleting an action must not destroy the
    -- ability to undo a run of it. The run already happened and its inverse is
    -- self-contained, so it outlives the definition that produced it.
    action_id TEXT REFERENCES quick_actions(id) ON DELETE SET NULL,
    -- The name AS IT WAS when it ran. Denormalised on purpose — renaming an
    -- action must not retitle history.
    name TEXT NOT NULL,
    ran_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    asset_count INTEGER NOT NULL DEFAULT 0,
    -- 0 when the inverse exceeded the payload budget. Stored rather than derived
    -- from "has no undo rows", because that would be ambiguous: a run where every
    -- asset already carried the tag has an empty inverse and IS undoable (as a
    -- no-op). This distinguishes "nothing to undo" from "we declined to record".
    is_undoable INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_action_runs_recent ON action_runs (ran_at DESC);

-- The inverse of ONE step, as data.
--
-- Deliberately not a before/after snapshot of each asset. The inverse of "add
-- tag t to 10,000 assets" is "remove t from the ids that did not already have
-- it" — an id list, not a copy of 10,000 rows. Cost is proportional to the
-- DELTA, which is what keeps a large run measured in hundreds of kilobytes and
-- lets the log live on disk instead of in memory.
CREATE TABLE IF NOT EXISTS action_undo (
    run_id TEXT NOT NULL REFERENCES action_runs(id) ON DELETE CASCADE,
    -- Position in the pipeline. Undo walks these DESCENDING, because the inverse
    -- of (A then B) is (B⁻¹ then A⁻¹).
    seq INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (run_id, seq)
) WITHOUT ROWID;
