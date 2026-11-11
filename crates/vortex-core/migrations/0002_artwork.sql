-- One row per game once artwork was handled. No row = never looked up.
-- source 'none' = looked up without a match, or removed by the user: do not query again
-- automatically. The image itself lives at Paths::artwork_file(game_id).
CREATE TABLE artwork (
  game_id    INTEGER PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
  source     TEXT NOT NULL CHECK (source IN ('auto','custom','none')),
  kind       TEXT CHECK (kind IN ('portrait','landscape')),
  steam_app  INTEGER,
  updated_at INTEGER NOT NULL
);
