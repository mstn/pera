CREATE INDEX IF NOT EXISTS idx_agents_loyalty ON agents(loyalty DESC);
CREATE INDEX IF NOT EXISTS idx_missions_status_region ON missions(status, region);
CREATE INDEX IF NOT EXISTS idx_inventory_status ON inventory_items(status);
CREATE INDEX IF NOT EXISTS idx_inventory_mission_id ON inventory_items(mission_id);
CREATE INDEX IF NOT EXISTS idx_mission_events_mission_id ON mission_events(mission_id, happened_at DESC);
