INSERT INTO transport_options (option_id, traveler_id, day, mode, origin, destination, depart_at, arrive_at, price, seats_available) VALUES
  ('alice_flight_1', 'alice', '2026-04-08', 'flight', 'Rome', 'Berlin', '2026-04-08T07:20:00', '2026-04-08T09:15:00', 240, 3),
  ('bruno_flight_1', 'bruno', '2026-04-08', 'flight', 'Milan', 'Berlin', '2026-04-08T07:45:00', '2026-04-08T09:05:00', 210, 2),
  ('alice_train_1', 'alice', '2026-04-08', 'train', 'Rome', 'Berlin', '2026-04-08T06:00:00', '2026-04-08T15:30:00', 180, 4),
  ('bruno_train_1', 'bruno', '2026-04-08', 'train', 'Milan', 'Berlin', '2026-04-08T06:10:00', '2026-04-08T13:40:00', 160, 4);

INSERT INTO lodging_options (hotel_name, city, check_in, check_out, room_type, capacity, remaining_rooms, price_total) VALUES
  ('Spree Central', 'Berlin', '2026-04-08', '2026-04-09', 'single', 1, 2, 160),
  ('Spree Central', 'Berlin', '2026-04-08', '2026-04-09', 'double', 2, 1, 210),
  ('Checkpoint Suites', 'Berlin', '2026-04-08', '2026-04-09', 'single', 1, 3, 190);
