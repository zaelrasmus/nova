CREATE TABLE IF NOT EXISTS folders (
   id TEXT PRIMARY KEY NOT NULL UNIQUE,
   name TEXT NOT NULL,
   folder_parent_id TEXT,
   description TEXT,
   order_by TEXT DEFAULT 'imported_date',
   is_ascending INTEGER DEFAULT 1, -- 0 = false, 1 = true


   FOREIGN KEY(folder_parent_id) REFERENCES folders(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS assets (
    id TEXT PRIMARY KEY NOT NULL UNIQUE,
    asset_type TEXT NOT NULL, -- 'image', 'video', 'audio', "etc."
    path TEXT NOT NULL,

    width INTEGER NOT NULL DEFAULT 0,
    height INTEGER NOT NULL DEFAULT 0,


    filename TEXT NOT NULL,
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



-- Membership indexes
CREATE INDEX IF NOT EXISTS idx_folder_contents ON assets_folders(folder_id, added_at);
CREATE INDEX IF NOT EXISTS idx_folders_position ON assets_folders (folder_id, position); -- Serve a folder's assets already ordered by manual position


-- ── Sort indexes: one composite (sortcol, id) per sort mode you expose.
-- The trailing id makes the keyset cursor unique so deep pages stay O(log n).
-- SQLite can scan an index backwards, so one index per column serves both
-- ascending and descending (is_ascending) without a second index.
CREATE INDEX IF NOT EXISTS idx_assets_imported ON assets (imported_date, id);
CREATE INDEX IF NOT EXISTS idx_assets_filename ON assets (filename,      id);
CREATE INDEX IF NOT EXISTS idx_assets_created  ON assets (creation_date, id);
