CREATE TABLE IF NOT EXISTS folders (
   id TEXT PRIMARY KEY NOT NULL UNIQUE,
   name TEXT NOT NULL,
   folder_parent_id TEXT,
   description TEXT,
   order_by TEXT DEFAULT 'imported_date', -- 'name', 'date', 'size'
   is_ascending INTEGER DEFAULT 1, -- 0 = false, 1 = true


   FOREIGN KEY(folder_parent_id) REFERENCES folders(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS assets (
    id TEXT PRIMARY KEY NOT NULL UNIQUE,
    asset_type TEXT NOT NULL, -- 'image', 'video', 'audio', "etc."
    path TEXT NOT NULL,


    filename TEXT NOT NULL,
    extension TEXT NOT NULL,
    imported_date TEXT NOT NULL,
    modified_date TEXT NOT NULL,
    creation_date TEXT NOT NULL
)

CREATE TABLE IF NOT EXISTS folder_assets (
    folder_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,

    PRIMARY KEY (folder_id, asset_id),
    FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE CASCADE,
    FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE
);

CREATE INDEX idx_folder_contents ON asset_folders(folder_id, added_at);
