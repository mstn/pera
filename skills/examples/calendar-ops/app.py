import json

from wit_world import exports
from wit_world.imports import calendar_ops_types, sqlite


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


class CalendarOpsExports(exports.CalendarOpsExports):
    def list_required_meetings(self, project_code, week_of):
        meetings = _query(
            """
            SELECT title, city, start_at, end_at
            FROM meetings
            WHERE project_code = :project_code
              AND week_of = :week_of
            ORDER BY start_at ASC
            """,
            {"project_code": project_code, "week_of": week_of},
        )
        attendees = _query(
            """
            SELECT title, traveler_id
            FROM meeting_attendees
            WHERE project_code = :project_code
              AND week_of = :week_of
            ORDER BY title ASC, traveler_id ASC
            """,
            {"project_code": project_code, "week_of": week_of},
        )
        attendees_by_title: dict[str, list[str]] = {}
        for row in attendees:
            attendees_by_title.setdefault(str(row.get("title", "")), []).append(
                str(row.get("traveler_id", ""))
            )

        return [
            calendar_ops_types.Meeting(
                title=str(row.get("title", "")),
                city=str(row.get("city", "")),
                start_at=str(row.get("start_at", "")),
                end_at=str(row.get("end_at", "")),
                required_attendees=attendees_by_title.get(str(row.get("title", "")), []),
            )
            for row in meetings
        ]

    def list_traveler_constraints(self, traveler_ids, week_of):
        constraints = _query(
            """
            SELECT traveler_id, origin_city
            FROM traveler_constraints
            WHERE week_of = :week_of
            ORDER BY traveler_id ASC
            """,
            {"week_of": week_of},
        )
        blocked = _query(
            """
            SELECT traveler_id, start_at, end_at
            FROM blocked_intervals
            WHERE week_of = :week_of
            ORDER BY traveler_id ASC, start_at ASC
            """,
            {"week_of": week_of},
        )

        requested_ids = {str(traveler_id) for traveler_id in (traveler_ids or [])}
        blocked_by_traveler: dict[str, list[str]] = {}
        for row in blocked:
            traveler_id = str(row.get("traveler_id", ""))
            blocked_by_traveler.setdefault(traveler_id, []).append(
                f"{row.get('start_at', '')} -> {row.get('end_at', '')}"
            )

        result: list[calendar_ops_types.TravelerConstraint] = []
        for row in constraints:
            traveler_id = str(row.get("traveler_id", ""))
            if requested_ids and traveler_id not in requested_ids:
                continue
            result.append(
                calendar_ops_types.TravelerConstraint(
                    traveler_id=traveler_id,
                    origin_city=str(row.get("origin_city", "")),
                    blocked_intervals=blocked_by_traveler.get(traveler_id, []),
                )
            )
        return result

    def search_travelers(self, names):
        requested_names = [str(name).strip() for name in (names or []) if str(name).strip()]
        if not requested_names:
            return []

        rows = _query(
            """
            SELECT traveler_id, display_name
            FROM travelers
            ORDER BY display_name ASC, traveler_id ASC
            """
        )

        matches: list[calendar_ops_types.TravelerRef] = []
        seen_ids: set[str] = set()
        for requested_name in requested_names:
            needle = requested_name.casefold()
            for row in rows:
                traveler_id = str(row.get("traveler_id", ""))
                display_name = str(row.get("display_name", ""))
                if traveler_id in seen_ids:
                    continue
                if needle not in {traveler_id.casefold(), display_name.casefold()}:
                    continue
                seen_ids.add(traveler_id)
                matches.append(
                    calendar_ops_types.TravelerRef(
                        traveler_id=traveler_id,
                        display_name=display_name,
                    )
                )
        return matches
