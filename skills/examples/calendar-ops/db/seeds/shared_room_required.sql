INSERT INTO meetings (project_code, week_of, title, city, start_at, end_at) VALUES
  ('DELTA', '2026-04-06', 'Delta Review', 'Berlin', '2026-04-09T10:00:00', '2026-04-09T12:00:00');

INSERT INTO meeting_attendees (project_code, week_of, title, traveler_id) VALUES
  ('DELTA', '2026-04-06', 'Delta Review', 'alice'),
  ('DELTA', '2026-04-06', 'Delta Review', 'bruno');

INSERT INTO travelers (traveler_id, display_name) VALUES
  ('alice', 'Alice'),
  ('bruno', 'Bruno');

INSERT INTO traveler_constraints (traveler_id, week_of, origin_city) VALUES
  ('alice', '2026-04-06', 'Rome'),
  ('bruno', '2026-04-06', 'Milan');

INSERT INTO blocked_intervals (traveler_id, week_of, start_at, end_at, reason) VALUES
  ('alice', '2026-04-06', '2026-04-08T16:00:00', '2026-04-08T18:00:00', 'staff-check-in'),
  ('bruno', '2026-04-06', '2026-04-10T15:00:00', '2026-04-10T17:00:00', 'regional-briefing');
