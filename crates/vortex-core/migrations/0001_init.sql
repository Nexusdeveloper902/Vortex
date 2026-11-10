CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE environments (
  id             INTEGER PRIMARY KEY,
  prefix_path    TEXT NOT NULL UNIQUE,
  prefix_managed INTEGER NOT NULL CHECK (prefix_managed IN (0,1)),
  runtime_path   TEXT NOT NULL,
  created_at     INTEGER NOT NULL
);

CREATE TABLE games (
  id                  INTEGER PRIMARY KEY,
  name                TEXT NOT NULL,
  environment_id      INTEGER NOT NULL REFERENCES environments(id) ON DELETE RESTRICT,
  status              TEXT NOT NULL CHECK (status IN ('installing','installed','failed','interrupted')),
  install_dir         TEXT,
  install_dir_managed INTEGER NOT NULL DEFAULT 0 CHECK (install_dir_managed IN (0,1)),
  exe_path            TEXT,
  work_dir            TEXT,
  launch_args         TEXT NOT NULL DEFAULT '[]',
  env_vars            TEXT NOT NULL DEFAULT '{}',
  created_at          INTEGER NOT NULL,
  updated_at          INTEGER NOT NULL,
  CHECK (status <> 'installed' OR exe_path IS NOT NULL)
);

CREATE TABLE installs (
  id             INTEGER PRIMARY KEY,
  game_id        INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
  source_path    TEXT NOT NULL,
  source_kind    TEXT NOT NULL CHECK (source_kind IN ('file','dir')),
  installer_path TEXT NOT NULL,
  state          TEXT NOT NULL CHECK (state IN ('created','prefix_init','running','waiting',
                   'select_exe','done','failed','cancelled','interrupted')),
  failure        TEXT,
  started_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);

CREATE TABLE runs (
  id             INTEGER PRIMARY KEY,
  marker         TEXT NOT NULL UNIQUE,
  kind           TEXT NOT NULL CHECK (kind IN ('prefix_init','install','launch','exec','winetricks')),
  environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
  game_id        INTEGER REFERENCES games(id) ON DELETE SET NULL,
  install_id     INTEGER REFERENCES installs(id) ON DELETE SET NULL,
  argv           TEXT NOT NULL,
  pid            INTEGER,
  pid_start      INTEGER,
  state          TEXT NOT NULL CHECK (state IN ('running','waiting','finished','stopped','interrupted')),
  exit_code      INTEGER,
  user_forced    INTEGER NOT NULL DEFAULT 0,
  findings       TEXT,
  log_path       TEXT NOT NULL,
  started_at     INTEGER NOT NULL,
  ended_at       INTEGER
);
CREATE INDEX runs_game ON runs(game_id, started_at);

CREATE TABLE dependencies (
  environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
  verb           TEXT NOT NULL,
  run_id         INTEGER REFERENCES runs(id) ON DELETE SET NULL,
  state          TEXT NOT NULL CHECK (state IN ('installed','failed')),
  at             INTEGER NOT NULL,
  PRIMARY KEY (environment_id, verb)
);

CREATE TABLE backups (
  id             INTEGER PRIMARY KEY,
  environment_id INTEGER REFERENCES environments(id) ON DELETE SET NULL,
  game_name      TEXT NOT NULL,
  archive_path   TEXT NOT NULL UNIQUE,
  runtime_path   TEXT NOT NULL,
  prefix_version TEXT,
  size_bytes     INTEGER NOT NULL,
  created_at     INTEGER NOT NULL
);
