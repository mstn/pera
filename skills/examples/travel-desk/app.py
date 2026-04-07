import json

from wit_world import exports
from wit_world.imports import sqlite, travel_desk_types


def _rows(result) -> list[dict]:
    if isinstance(result, str):
        try:
            result = json.loads(result)
        except Exception:
            return []
    if isinstance(result, list):
        return [row for row in result if isinstance(row, dict)]
    if isinstance(result, dict):
        for key in ("rows", "items", "results", "data"):
            value = result.get(key)
            if isinstance(value, list):
                return [row for row in value if isinstance(row, dict)]
        return [result]
    return []


def _query(sql: str, params: dict | None = None) -> list[dict]:
    return _rows(sqlite.execute(sql, None if params is None else json.dumps(params)))


class TravelDeskExports(exports.TravelDeskExports):
    def search_transport(self, origin, destination, day):
        rows = _query(
            """
            SELECT option_id, traveler_id, mode, origin, destination, depart_at, arrive_at, price, seats_available
            FROM transport_options
            WHERE origin = :origin
              AND destination = :destination
              AND day = :day
            ORDER BY depart_at ASC, price ASC
            """,
            {"origin": origin, "destination": destination, "day": day},
        )
        return [
            travel_desk_types.TransportOption(
                option_id=str(row.get("option_id", "")),
                traveler_id=str(row.get("traveler_id", "")),
                mode=str(row.get("mode", "")),
                origin=str(row.get("origin", "")),
                destination=str(row.get("destination", "")),
                depart_at=str(row.get("depart_at", "")),
                arrive_at=str(row.get("arrive_at", "")),
                price=int(row.get("price", 0) or 0),
                seats_available=int(row.get("seats_available", 0) or 0),
            )
            for row in rows
        ]

    def search_lodging(self, city, check_in, check_out):
        rows = _query(
            """
            SELECT hotel_name, city, check_in, check_out, room_type, capacity, remaining_rooms, price_total
            FROM lodging_options
            WHERE city = :city
              AND check_in = :check_in
              AND check_out = :check_out
            ORDER BY price_total ASC, hotel_name ASC
            """,
            {"city": city, "check_in": check_in, "check_out": check_out},
        )
        return [
            travel_desk_types.LodgingOption(
                hotel_name=str(row.get("hotel_name", "")),
                city=str(row.get("city", "")),
                check_in=str(row.get("check_in", "")),
                check_out=str(row.get("check_out", "")),
                room_type=str(row.get("room_type", "")),
                capacity=int(row.get("capacity", 0) or 0),
                remaining_rooms=int(row.get("remaining_rooms", 0) or 0),
                price_total=int(row.get("price_total", 0) or 0),
            )
            for row in rows
        ]
