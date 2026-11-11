-- M26: installer arguments, a JSON array of strings (argv), kept for retries.
ALTER TABLE installs ADD COLUMN installer_args TEXT NOT NULL DEFAULT '[]';
