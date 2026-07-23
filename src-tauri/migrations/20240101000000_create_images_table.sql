CREATE TABLE IF NOT EXISTS folders (
   id TEXT PRIMARY KEY NOT NULL UNIQUE,
   name TEXT NOT NULL,
   parent_id TEXT,
   position REAL NOT NULL DEFAULT 0,
   description TEXT,
   order_by TEXT NOT NULL DEFAULT 'manual',
   is_ascending INTEGER NOT NULL DEFAULT 1, -- 0 = false, 1 = true


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
CREATE TABLE IF NOT EXISTS saved_filters (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,

    -- List ordering (fractional rank, same idea as folders.position). No reorder
    -- command yet; the column exists so adding one later needs no migration.
    position REAL NOT NULL DEFAULT 0,

    -- Schema version of query_json. When the filter language changes (tags), old
    -- documents get migrated deliberately instead of silently mis-parsed, and a
    -- row written by a NEWER build is skipped rather than misread.
    version INTEGER NOT NULL DEFAULT 1,

    -- Serialized FilterSet.
    query_json TEXT NOT NULL,

    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_saved_filters_order ON saved_filters (position, name);

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

-- Membership indexes
CREATE INDEX IF NOT EXISTS idx_folder_contents ON assets_folders(folder_id, added_at);
CREATE INDEX IF NOT EXISTS idx_folders_position ON assets_folders (folder_id, position); -- Serve a folder's assets already ordered by manual position
CREATE INDEX IF NOT EXISTS idx_folders_tree ON folders(parent_id, position);


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
