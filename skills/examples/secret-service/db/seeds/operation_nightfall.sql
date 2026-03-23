INSERT OR REPLACE INTO agents (id, codename, skills_json, loyalty, cover_identity, status, created_at) VALUES
  ('agent_bond007', 'Bond', '["infiltration", "languages", "marksmanship"]', 94, 'International trade consultant', 'active', datetime('now')),
  ('agent_vesper', 'Vesper', '["finance", "counterintelligence", "analysis"]', 86, 'Private banking advisor', 'active', datetime('now')),
  ('agent_qbranch', 'Q', '["engineering", "cryptography", "field-support"]', 99, 'University robotics lecturer', 'active', datetime('now'));

INSERT OR REPLACE INTO missions (id, objective, difficulty, region, required_skills_json, status, assigned_agent_id, result_notes, created_at, resolved_at) VALUES
  ('mission_orchid', 'Recover encrypted dossier from offshore data vault', 'high', 'Mediterranean', '["infiltration", "cryptography"]', 'assigned', 'agent_bond007', NULL, datetime('now'), NULL),
  ('mission_tundra', 'Locate defector and verify authenticity', 'medium', 'Nordics', '["analysis", "languages"]', 'planned', NULL, NULL, datetime('now'), NULL);

INSERT OR REPLACE INTO inventory_items (id, name, category, quantity, status, assigned_agent_id, mission_id, specs_json, created_at) VALUES
  ('gadget_omega_laser', 'Wristwatch Laser', 'wearable', 2, 'available', NULL, NULL, '{"power":"low-yield","battery_hours":4}', datetime('now')),
  ('gadget_pen_dart', 'Pen Tranquilizer', 'disguise', 5, 'assigned', 'agent_bond007', 'mission_orchid', '{"range_m":8,"payload":"sedative"}', datetime('now')),
  ('gadget_bug_swarm', 'Micro Bug Swarm', 'surveillance', 12, 'available', NULL, NULL, '{"uptime_minutes":45,"encrypted":true}', datetime('now'));

INSERT OR REPLACE INTO mission_events (id, mission_id, event_type, payload_json, happened_at) VALUES
  ('evt_orchid_briefing', 'mission_orchid', 'briefing', '{"location":"safehouse-12"}', datetime('now')),
  ('evt_orchid_loadout', 'mission_orchid', 'gear-assigned', '{"items":["gadget_pen_dart"]}', datetime('now'));
