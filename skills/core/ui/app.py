from __future__ import annotations

import json

from wit_world import exports
from wit_world.imports import ui_builder, ui_types


def _json(value) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True)


def _prop(name: str, value) -> str:
    return ui_builder.prop(name=name, encoded_value=_json(value))


def _event(action_name: str, result_path: str | None = None, args: dict | None = None) -> str:
    return ui_builder.handler(
        event="click",
        action_name=action_name,
        encoded_action_args=_json(args or {}),
        result_path=result_path,
    )


def _node(component: str, props: list[str] | None = None, children: list[str] | None = None, events: list[str] | None = None) -> str:
    return ui_builder.element(
        component=component,
        props=props or [],
        children=children or [],
        handlers=events or [],
    )


def _value(value) -> object:
    if hasattr(value, "value"):
        tag = value.value
    else:
        tag = value

    if isinstance(tag, tuple) and len(tag) == 2:
        case, payload = tag
    else:
        case = getattr(value, "name", None) or str(value)
        payload = None

    case = str(case).lower().replace("-", "_")

    if case in {"text", "string"}:
        return {"kind": "string", "value": payload}
    if case == "integer":
        return {"kind": "integer", "value": payload}
    if case == "float":
        return {"kind": "float", "value": payload}
    if case in {"boolean", "bool"}:
        return {"kind": "bool", "value": payload}
    if case in {"empty", "null"}:
        return {"kind": "null"}
    if case == "bind":
        return {"kind": "binding", "binding": {"path": payload}}
    raise ValueError(f"unsupported ui value case: {case}")


def _action_args(args) -> dict:
    return {arg.name: _value(arg.value) for arg in args}


class UiExports(exports.UiExports):
    def stack(self, children):
        return _node("column", children=list(children))

    def text(self, value, style):
        return _node(
            "text",
            props=[
                _prop("text", {"kind": "string", "value": value}),
                _prop(
                    "style",
                    {
                        "kind": "string",
                        "value": getattr(style, "name", str(style)).lower(),
                    },
                ),
            ],
        )

    def markdown(self, value):
        return _node(
            "markdown",
            props=[_prop("value", {"kind": "string", "value": value})],
        )

    def code(self, source, language):
        props = [_prop("source", {"kind": "string", "value": source})]
        if language is not None:
            props.append(_prop("language", {"kind": "string", "value": language}))
        return _node("code", props=props)

    def notice(self, kind, title, body):
        props = [
            _prop(
                "kind",
                {
                    "kind": "string",
                    "value": getattr(kind, "name", str(kind)).lower(),
                },
            ),
            _prop("body", {"kind": "string", "value": body}),
        ]
        if title is not None:
            props.append(_prop("title", {"kind": "string", "value": title}))
        return _node("notice", props=props)

    def text_input(self, label, bind_path):
        return _node(
            "text_field",
            props=[
                _prop("label", {"kind": "string", "value": label}),
                _prop("value", {"kind": "binding", "binding": {"path": bind_path}}),
            ],
        )

    def button(self, label, action_name, action_args, result_path):
        return _node(
            "button",
            props=[_prop("label", {"kind": "string", "value": label})],
            events=[_event(action_name, result_path, _action_args(action_args))],
        )

    def list(self, label, items_bind_path):
        props = [
            _prop("items", {"kind": "binding", "binding": {"path": items_bind_path}}),
        ]
        if label is not None:
            props.append(_prop("label", {"kind": "string", "value": label}))
        return _node("list", props=props)

    def table(self, title, columns, rows):
        props = [
            _prop(
                "columns",
                {
                    "kind": "string",
                    "value": _json(
                        [
                            {
                                "key": column.key,
                                "label": column.label,
                            }
                            for column in columns
                        ]
                    ),
                },
            ),
            _prop(
                "rows",
                {
                    "kind": "string",
                    "value": _json(
                        [
                            {
                                cell.column_key: cell.value
                                for cell in row.cells
                            }
                            for row in rows
                        ]
                    ),
                },
            ),
        ]
        if title is not None:
            props.append(_prop("title", {"kind": "string", "value": title}))
        return _node("table", props=props)

    def map(self, title, features):
        props = [
            _prop(
                "features",
                {
                    "kind": "string",
                    "value": _json(
                        [
                            {
                                "latitude": feature.latitude,
                                "longitude": feature.longitude,
                                "label": feature.label,
                                "details": feature.details,
                            }
                            for feature in features
                        ]
                    ),
                },
            ),
        ]
        if title is not None:
            props.append(_prop("title", {"kind": "string", "value": title}))
        return _node("map", props=props)

    def screen(self, id, title, root):
        return ui_builder.screen(id=id, title=title, root=root)
