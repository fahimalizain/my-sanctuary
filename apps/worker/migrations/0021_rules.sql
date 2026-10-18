-- Consistency rules (reports slice 3).
--
-- A rule is scored on read from the digest tables; nothing here stores a
-- verdict. `kind` is immutable after create, `status` is one of
-- active | paused | archived (no soft-delete column — archiving is a status),
-- and `starts_on` / `ends_on` are inclusive civil dates. Days before
-- `starts_on` are not in play (strip `off`, no streak) but their digest rows
-- still feed the window of the first in-play days.
CREATE TABLE rules (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  name TEXT NOT NULL,
  icon TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL, -- category_total | category_days | category_daily_avg | sleep_duration | sleep_timing
  status TEXT NOT NULL DEFAULT 'active', -- active | paused | archived
  starts_on TEXT NOT NULL, -- YYYY-MM-DD inclusive; the rule is "in play" from this date
  ends_on TEXT, -- inclusive challenge end; NULL = open-ended
  paused_on TEXT, -- set iff status = 'paused': open-ended pause began this date
  sort_order INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_rules_user ON rules(user_id, sort_order);

-- Scoring params only. Cosmetic edits (name, icon, sort_order, status, starts/ends) do NOT add a row.
-- The row active on date D is the one with the greatest effective_from <= D;
-- ties broken by created_at then id (latest wins).
CREATE TABLE rule_revisions (
  id TEXT PRIMARY KEY,
  rule_id TEXT NOT NULL,
  effective_from TEXT NOT NULL,
  params_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (rule_id) REFERENCES rules(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_rule_revisions_rule ON rule_revisions(rule_id, effective_from);

-- Closed pause intervals. rule_id NULL = global (every rule).
-- Open-ended "pause this rule" is rules.paused_on + status, NOT a row here,
-- until resume closes it.
CREATE TABLE pause_periods (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  rule_id TEXT, -- NULL = global
  from_date TEXT NOT NULL, -- inclusive
  to_date TEXT NOT NULL, -- inclusive
  reason TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
  FOREIGN KEY (rule_id) REFERENCES rules(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_pause_periods_user ON pause_periods(user_id, from_date, to_date);
