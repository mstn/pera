PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS agents (
  id TEXT PRIMARY KEY,
  codename TEXT NOT NULL UNIQUE,
  skills_json TEXT NOT NULL DEFAULT '[]',
  loyalty INTEGER NOT NULL CHECK (loyalty >= 0 AND loyalty <= 100),
  cover_identity TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'active',
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS missions (
  id TEXT PRIMARY KEY,
  objective TEXT NOT NULL,
  difficulty TEXT NOT NULL CHECK (difficulty IN ('low', 'medium', 'high', 'extreme')),
  region TEXT NOT NULL,
  required_skills_json TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL CHECK (status IN ('planned', 'assigned', 'in-progress', 'resolved', 'failed', 'aborted')),
  assigned_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
  result_notes TEXT,
  created_at TEXT NOT NULL,
  resolved_at TEXT
);

CREATE TABLE IF NOT EXISTS inventory_items (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  category TEXT NOT NULL,
  quantity INTEGER NOT NULL CHECK (quantity >= 0),
  status TEXT NOT NULL CHECK (status IN ('available', 'assigned', 'used', 'retired')),
  assigned_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
  mission_id TEXT REFERENCES missions(id) ON DELETE SET NULL,
  specs_json TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS mission_events (
  id TEXT PRIMARY KEY,
  mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL DEFAULT '{}',
  happened_at TEXT NOT NULL
);
