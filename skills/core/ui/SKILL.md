---
name: ui
when_to_use: Use when you need to render structured assistant-side visual content in chat using basic UI elements such as text, markdown, code, notices, tables, and maps.
---

# UI

Build user interfaces.

Use:

- `stack(children)` for vertical layout.
- `text(value, style)` for labels, headings, captions, and narrative text.
- `markdown(value)` for rich prose authored in markdown.
- `code(source, language)` for code blocks or structured snippets.
- `notice(kind, title, body)` for alerts, warnings, errors, and success messages.
- `text_input(label, bind_path)` for editable text fields bound to UI state.
- `button(label, action_name, action_args, result_path)` for actions that call the environment.
- `list(label, items_bind_path)` for repeated collections.
- `table(title, columns, rows)` for structured tabular data.
- `map(title, features)` for maps with feature overlays.

Compose the UI from component-returning functions and finish with `screen(id, title, root)`.

Use clear semantic structure:

- headings and narrative text before controls
- notices for important status or warnings
- tables for structured records
- lists for simple repeated items
- maps only when location or geometry is central to the task
