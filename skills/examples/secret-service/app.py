from wit_world import exports
from wit_world.imports import secret_service_types, sqlite

import json
from datetime import datetime, timezone
from uuid import uuid4


def _now_iso() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def _rows(result) -> list:
    if isinstance(result, str):
        try:
            result = json.loads(result)
        except Exception:
            return []
    if isinstance(result, list):
        return result
    if isinstance(result, dict):
        for key in ("rows", "items", "results", "data"):
            value = result.get(key)
            if isinstance(value, list):
                return value
        return [result]
    return []


def _json_loads(value, default):
    if value is None:
        return default
    if isinstance(value, (list, dict)):
        return value
    if isinstance(value, str):
        try:
            return json.loads(value)
        except Exception:
            return default
    return default


def _enum_value(value, fallback: str) -> str:
    if value is None:
        return fallback
    if isinstance(value, str):
        return value
    return str(getattr(value, "name", fallback)).lower().replace("_", "-")


def _difficulty(raw):
    key = str(raw or "medium").replace("-", "_").upper()
    return secret_service_types.Difficulty.__members__.get(key, secret_service_types.Difficulty.MEDIUM)


def _mission_status(raw):
    key = str(raw or "planned").replace("-", "_").upper()
    return secret_service_types.MissionStatus.__members__.get(key, secret_service_types.MissionStatus.PLANNED)


def _inventory_status(raw):
    key = str(raw or "available").replace("-", "_").upper()
    return secret_service_types.InventoryStatus.__members__.get(key, secret_service_types.InventoryStatus.AVAILABLE)


def _mission_from_row(row: dict) -> secret_service_types.Mission:
    return secret_service_types.Mission(
        id=str(row.get("id", "")),
        objective=str(row.get("objective", "")),
        difficulty=_difficulty(row.get("difficulty")),
        region=str(row.get("region", "")),
        required_skills=[str(item) for item in _json_loads(row.get("required_skills_json"), [])],
        status=_mission_status(row.get("status")),
        assigned_agent_id=(None if row.get("assigned_agent_id") is None else str(row.get("assigned_agent_id"))),
        result_notes=(None if row.get("result_notes") is None else str(row.get("result_notes"))),
    )


def _agent_from_row(row: dict) -> secret_service_types.Agent:
    return secret_service_types.Agent(
        id=str(row.get("id", "")),
        codename=str(row.get("codename", "")),
        skills=[str(item) for item in _json_loads(row.get("skills_json"), [])],
        loyalty=int(row.get("loyalty", 0) or 0),
        cover_identity=str(row.get("cover_identity", "")),
        status=str(row.get("status", "active")),
    )


def _inventory_from_row(row: dict) -> secret_service_types.InventoryItem:
    specs_json = row.get("specs_json")
    return secret_service_types.InventoryItem(
        id=str(row.get("id", "")),
        name=str(row.get("name", "")),
        category=str(row.get("category", "")),
        quantity=int(row.get("quantity", 0) or 0),
        status=_inventory_status(row.get("status")),
        assigned_agent_id=(None if row.get("assigned_agent_id") is None else str(row.get("assigned_agent_id"))),
        mission_id=(None if row.get("mission_id") is None else str(row.get("mission_id"))),
        specs=(None if specs_json is None else str(specs_json)),
    )


class SecretServiceExports(exports.SecretServiceExports):
    def register_agent(self, codename, skills, loyalty, cover_identity):
        agent_id = f"agent_{uuid4().hex[:10]}"
        sqlite.execute(
            """
            INSERT INTO agents (
              id, codename, skills_json, loyalty, cover_identity, status, created_at
            ) VALUES (
              :id, :codename, :skills_json, :loyalty, :cover_identity, 'active', :created_at
            )
            """,
            json.dumps(
                {
                    "id": agent_id,
                    "codename": codename,
                    "skills_json": json.dumps([str(skill) for skill in (skills or [])]),
                    "loyalty": max(0, min(100, int(loyalty))),
                    "cover_identity": cover_identity,
                    "created_at": _now_iso(),
                }
            ),
        )
        return self._get_agent(agent_id)

    def list_agents(self, loyalty_min, required_skill):
        raw = sqlite.execute(
            """
            SELECT id, codename, skills_json, loyalty, cover_identity, status
            FROM agents
            WHERE (:loyalty_min IS NULL OR loyalty >= :loyalty_min)
              AND (
                :required_skill IS NULL
                OR EXISTS (
                  SELECT 1
                  FROM json_each(agents.skills_json)
                  WHERE json_each.value = :required_skill
                )
              )
            ORDER BY loyalty DESC, codename ASC
            """,
            json.dumps(
                {
                    "loyalty_min": (None if loyalty_min is None else int(loyalty_min)),
                    "required_skill": required_skill,
                }
            ),
        )
        return [_agent_from_row(row) for row in _rows(raw) if isinstance(row, dict)]

    def create_mission(self, objective, difficulty, region, required_skills):
        mission_id = f"mission_{uuid4().hex[:10]}"
        sqlite.execute(
            """
            INSERT INTO missions (
              id, objective, difficulty, region, required_skills_json,
              status, assigned_agent_id, result_notes, created_at
            ) VALUES (
              :id, :objective, :difficulty, :region, :required_skills_json,
              'planned', NULL, NULL, :created_at
            )
            """,
            json.dumps(
                {
                    "id": mission_id,
                    "objective": objective,
                    "difficulty": _enum_value(difficulty, "medium"),
                    "region": region,
                    "required_skills_json": json.dumps([str(skill) for skill in (required_skills or [])]),
                    "created_at": _now_iso(),
                }
            ),
        )
        return self._get_mission(mission_id)

    def list_missions(self, status, region):
        raw = sqlite.execute(
            """
            SELECT id, objective, difficulty, region, required_skills_json,
                   status, assigned_agent_id, result_notes
            FROM missions
            WHERE (:status IS NULL OR status = :status)
              AND (:region IS NULL OR region = :region)
            ORDER BY created_at DESC
            """,
            json.dumps(
                {
                    "status": (None if status is None else _enum_value(status, "planned")),
                    "region": region,
                }
            ),
        )
        return [_mission_from_row(row) for row in _rows(raw) if isinstance(row, dict)]

    def add_inventory_item(self, name, category, quantity, specs):
        item_id = f"gadget_{uuid4().hex[:10]}"
        sqlite.execute(
            """
            INSERT INTO inventory_items (
              id, name, category, quantity, status, assigned_agent_id, mission_id, specs_json, created_at
            ) VALUES (
              :id, :name, :category, :quantity, 'available', NULL, NULL, :specs_json, :created_at
            )
            """,
            json.dumps(
                {
                    "id": item_id,
                    "name": name,
                    "category": category,
                    "quantity": max(0, int(quantity)),
                    "specs_json": specs,
                    "created_at": _now_iso(),
                }
            ),
        )
        return self._get_inventory_item(item_id)

    def list_inventory(self, status, only_available):
        raw = sqlite.execute(
            """
            SELECT id, name, category, quantity, status, assigned_agent_id, mission_id, specs_json
            FROM inventory_items
            WHERE (:status IS NULL OR status = :status)
              AND (
                :only_available IS NULL
                OR :only_available = 0
                OR (status = 'available' AND quantity > 0)
              )
            ORDER BY name ASC
            """,
            json.dumps(
                {
                    "status": (None if status is None else _enum_value(status, "available")),
                    "only_available": (None if only_available is None else (1 if only_available else 0)),
                }
            ),
        )
        return [_inventory_from_row(row) for row in _rows(raw) if isinstance(row, dict)]

    def assign_mission(self, mission_id, agent_id, gadget_ids):
        _ = self._get_agent(agent_id)
        sqlite.execute(
            """
            UPDATE missions
            SET assigned_agent_id = :agent_id,
                status = 'assigned'
            WHERE id = :mission_id
            """,
            json.dumps({"mission_id": mission_id, "agent_id": agent_id}),
        )

        assigned: list[secret_service_types.InventoryItem] = []
        for gadget_id in gadget_ids or []:
            sqlite.execute(
                """
                UPDATE inventory_items
                SET assigned_agent_id = :agent_id,
                    mission_id = :mission_id,
                    status = 'assigned'
                WHERE id = :gadget_id
                  AND quantity > 0
                  AND status = 'available'
                """,
                json.dumps(
                    {
                        "mission_id": mission_id,
                        "agent_id": agent_id,
                        "gadget_id": gadget_id,
                    }
                ),
            )
            assigned.append(self._get_inventory_item(gadget_id))

        return secret_service_types.AssignMissionOutput(
            mission=self._get_mission(mission_id),
            assigned_gadgets=assigned,
        )

    def resolve_mission(self, mission_id, outcome, notes):
        released_raw = sqlite.execute(
            """
            SELECT id, name, category, quantity, status, assigned_agent_id, mission_id, specs_json
            FROM inventory_items
            WHERE mission_id = :mission_id
            ORDER BY name ASC
            """,
            json.dumps({"mission_id": mission_id}),
        )
        released = [_inventory_from_row(row) for row in _rows(released_raw) if isinstance(row, dict)]

        status = "aborted"
        outcome_raw = _enum_value(outcome, "success")
        if outcome_raw == "success":
            status = "resolved"
        elif outcome_raw == "failure":
            status = "failed"

        sqlite.execute(
            """
            UPDATE missions
            SET status = :status,
                result_notes = :notes,
                resolved_at = :resolved_at
            WHERE id = :mission_id
            """,
            json.dumps(
                {
                    "mission_id": mission_id,
                    "status": status,
                    "notes": notes,
                    "resolved_at": _now_iso(),
                }
            ),
        )

        sqlite.execute(
            """
            UPDATE inventory_items
            SET mission_id = NULL,
                assigned_agent_id = NULL,
                status = 'available'
            WHERE mission_id = :mission_id
            """,
            json.dumps({"mission_id": mission_id}),
        )

        return secret_service_types.ResolveMissionOutput(
            mission=self._get_mission(mission_id),
            released_gadgets=released,
        )

    def _get_agent(self, agent_id: str) -> secret_service_types.Agent:
        raw = sqlite.execute(
            """
            SELECT id, codename, skills_json, loyalty, cover_identity, status
            FROM agents
            WHERE id = :agent_id
            """,
            json.dumps({"agent_id": agent_id}),
        )
        rows = _rows(raw)
        if not rows or not isinstance(rows[0], dict):
            raise ValueError(f"agent not found: {agent_id}")
        return _agent_from_row(rows[0])

    def _get_mission(self, mission_id: str) -> secret_service_types.Mission:
        raw = sqlite.execute(
            """
            SELECT id, objective, difficulty, region, required_skills_json,
                   status, assigned_agent_id, result_notes
            FROM missions
            WHERE id = :mission_id
            """,
            json.dumps({"mission_id": mission_id}),
        )
        rows = _rows(raw)
        if not rows or not isinstance(rows[0], dict):
            raise ValueError(f"mission not found: {mission_id}")
        return _mission_from_row(rows[0])

    def _get_inventory_item(self, item_id: str) -> secret_service_types.InventoryItem:
        raw = sqlite.execute(
            """
            SELECT id, name, category, quantity, status, assigned_agent_id, mission_id, specs_json
            FROM inventory_items
            WHERE id = :item_id
            """,
            json.dumps({"item_id": item_id}),
        )
        rows = _rows(raw)
        if not rows or not isinstance(rows[0], dict):
            raise ValueError(f"inventory item not found: {item_id}")
        return _inventory_from_row(rows[0])


exports.SecretServiceExports = SecretServiceExports
