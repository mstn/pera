import json

from wit_world import exports
from wit_world.imports import sqlite, travel_policy_types


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


class TravelPolicyExports(exports.TravelPolicyExports):
    def get_trip_policy(self, cost_center, traveler_ids):
        _ = traveler_ids
        policies = _query(
            """
            SELECT cost_center, max_budget_without_approval, shared_room_allowed,
                   single_room_preferred, red_eye_allowed
            FROM trip_policies
            WHERE cost_center = :cost_center
            """,
            {"cost_center": cost_center},
        )
        notes = _query(
            """
            SELECT note
            FROM policy_notes
            WHERE cost_center = :cost_center
            ORDER BY note ASC
            """,
            {"cost_center": cost_center},
        )
        row = policies[0] if policies else {}
        return travel_policy_types.TripPolicy(
            cost_center=str(row.get("cost_center", cost_center)),
            max_budget_without_approval=int(row.get("max_budget_without_approval", 0) or 0),
            shared_room_allowed=bool(int(row.get("shared_room_allowed", 0) or 0)),
            single_room_preferred=bool(int(row.get("single_room_preferred", 0) or 0)),
            red_eye_allowed=bool(int(row.get("red_eye_allowed", 0) or 0)),
            notes=[str(item.get("note", "")) for item in notes],
        )

    def requires_manager_approval(self, total_cost, cost_center):
        policies = _query(
            """
            SELECT max_budget_without_approval
            FROM trip_policies
            WHERE cost_center = :cost_center
            """,
            {"cost_center": cost_center},
        )
        if not policies:
            return False
        limit = int(policies[0].get("max_budget_without_approval", 0) or 0)
        return int(total_cost) > limit
