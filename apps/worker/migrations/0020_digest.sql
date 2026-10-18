-- Per-day rollup. `seconds` columns are integer seconds (overlap-shared), NOT minutes.
-- time_zone is the duration-weighted IANA zone of that local_date (see digest::tz).
-- algo_version mismatch means the row is stale and must be rebuilt.
CREATE TABLE daily_meta (
  user_id TEXT NOT NULL,
  local_date TEXT NOT NULL, -- YYYY-MM-DD
  time_zone TEXT NOT NULL,
  tracked_seconds INTEGER NOT NULL,
  sleep_seconds INTEGER NOT NULL, -- post-share seconds of SYNTHETIC sleep intervals only
  algo_version INTEGER NOT NULL,
  computed_at TEXT NOT NULL,
  PRIMARY KEY (user_id, local_date)
);

-- '' category_id = unclassified real tracked time (not the untracked fill).
-- '*sleep' = synthetic sleep when the user has no sleep category (see SYNTHETIC_SLEEP_CATEGORY_ID).
CREATE TABLE daily_category_minutes (
  user_id TEXT NOT NULL,
  local_date TEXT NOT NULL,
  category_id TEXT NOT NULL DEFAULT '',
  seconds INTEGER NOT NULL,
  PRIMARY KEY (user_id, local_date, category_id)
);

-- Hour-of-day in the day's time_zone, for the productive heatmap (slice 5).
-- Same category_id sentinel rules. Sum of seconds for a date == daily_meta.tracked_seconds.
CREATE TABLE daily_hour_seconds (
  user_id TEXT NOT NULL,
  local_date TEXT NOT NULL,
  hour INTEGER NOT NULL, -- 0..23
  category_id TEXT NOT NULL DEFAULT '',
  seconds INTEGER NOT NULL,
  PRIMARY KEY (user_id, local_date, hour, category_id)
);

-- One row per paired or estimated night. Multiple nights may share a wake_date
-- (a nap and a night) — scorer.py keeps every pair, so wake_date is NOT unique.
CREATE TABLE sleep_nights (
  user_id TEXT NOT NULL,
  wake_date TEXT NOT NULL,
  asleep_at TEXT NOT NULL, -- RFC3339 UTC Z
  woke_at TEXT NOT NULL,
  seconds INTEGER NOT NULL, -- full marker delta, NOT overlap-shared
  bed_min_past_noon INTEGER NOT NULL,
  wake_min_past_midnight INTEGER NOT NULL,
  source TEXT NOT NULL, -- paired | est_no_wake | est_no_asleep | est_none
  PRIMARY KEY (user_id, asleep_at)
);
CREATE INDEX IF NOT EXISTS idx_sleep_nights_wake ON sleep_nights(user_id, wake_date);

CREATE TABLE dirty_local_days (
  user_id TEXT NOT NULL,
  local_date TEXT NOT NULL,
  enqueued_at TEXT NOT NULL,
  PRIMARY KEY (user_id, local_date)
);

-- kind: reclassify | backfill. status: idle | running | done.
-- reclassify cursor is the last calendar_events.id processed ('' = start).
-- backfill cursor is the next YYYY-MM-DD to rebuild ('' = not started).
CREATE TABLE report_job_status (
  user_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  cursor TEXT NOT NULL DEFAULT '',
  updated_at TEXT NOT NULL,
  PRIMARY KEY (user_id, kind)
);
