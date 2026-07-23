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
