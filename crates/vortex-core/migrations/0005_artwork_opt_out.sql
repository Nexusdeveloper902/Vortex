-- M28: source 'none' set by the user ("Don't look up artwork", the wizard's NDA switch, Remove
-- artwork), as opposed to an automatic lookup without a match. Rows from before M28 read as 0.
ALTER TABLE artwork ADD COLUMN opted_out INTEGER NOT NULL DEFAULT 0;
