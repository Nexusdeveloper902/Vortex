-- M22: how many possible Steam matches an automatic lookup found but did not attach
-- (source 'none', steam_app NULL). 0 = no candidates, or not applicable.
ALTER TABLE artwork ADD COLUMN candidates INTEGER NOT NULL DEFAULT 0;
