PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meetings (
  project_code TEXT NOT NULL,
  week_of TEXT NOT NULL,
  title TEXT NOT NULL,
  city TEXT NOT NULL,
  start_at TEXT NOT NULL,
  end_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS meeting_attendees (
  project_code TEXT NOT NULL,
  week_of TEXT NOT NULL,
  title TEXT NOT NULL,
  traveler_id TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS traveler_constraints (
  traveler_id TEXT NOT NULL,
  week_of TEXT NOT NULL,
  origin_city TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS travelers (
  traveler_id TEXT NOT NULL PRIMARY KEY,
  display_name TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS blocked_intervals (
  traveler_id TEXT NOT NULL,
  week_of TEXT NOT NULL,
  start_at TEXT NOT NULL,
  end_at TEXT NOT NULL,
  reason TEXT NOT NULL
);
