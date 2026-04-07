PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS transport_options (
  option_id TEXT PRIMARY KEY,
  traveler_id TEXT NOT NULL,
  day TEXT NOT NULL,
  mode TEXT NOT NULL,
  origin TEXT NOT NULL,
  destination TEXT NOT NULL,
  depart_at TEXT NOT NULL,
  arrive_at TEXT NOT NULL,
  price INTEGER NOT NULL,
  seats_available INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS lodging_options (
  hotel_name TEXT NOT NULL,
  city TEXT NOT NULL,
  check_in TEXT NOT NULL,
  check_out TEXT NOT NULL,
  room_type TEXT NOT NULL,
  capacity INTEGER NOT NULL,
  remaining_rooms INTEGER NOT NULL,
  price_total INTEGER NOT NULL
);
