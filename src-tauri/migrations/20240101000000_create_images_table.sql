CREATE TABLE IF NOT EXISTS folders (
   id TEXT PRIMARY KEY NOT NULL UNIQUE,
   name TEXT NOT NULL,
   parent_id TEXT,
   description TEXT,
   order_by TEXT DEFAULT 'imported_date',
   is_ascending INTEGER DEFAULT 1,


   FOREIGN KEY(parent_id) REFERENCES folders(id) ON DELETE CASCADE
);
