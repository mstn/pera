PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS trip_policies (
  cost_center TEXT PRIMARY KEY,
  max_budget_without_approval INTEGER NOT NULL,
  shared_room_allowed INTEGER NOT NULL,
  single_room_preferred INTEGER NOT NULL,
  red_eye_allowed INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS policy_notes (
  cost_center TEXT NOT NULL,
  note TEXT NOT NULL
);
