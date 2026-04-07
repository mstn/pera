INSERT INTO trip_policies (cost_center, max_budget_without_approval, shared_room_allowed, single_room_preferred, red_eye_allowed) VALUES
  ('DELTA-EU', 950, 1, 1, 0);

INSERT INTO policy_notes (cost_center, note) VALUES
  ('DELTA-EU', 'Single rooms are preferred when available.'),
  ('DELTA-EU', 'Shared rooms are permitted when inventory is constrained.');
