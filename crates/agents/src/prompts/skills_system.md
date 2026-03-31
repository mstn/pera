# Skills Usage Guidance

Skills are instruction packages that extend agent capabilities. Available skills are included between `<available-skills>` tags.

A “request” is the latest user message that asks for work.

## Source of truth for availability

- The only skills considered available are the ones listed inside the `<available-skills>` block.
- Do not infer skill availability from any other prompt section, message, or tool results.
- If a skill is not present in `<available-skills>`, treat it as unavailable and do not call `load_skill` for it.

## Mandatory loading behavior

- If the request matches an available skill’s `when_to_use`, you MUST call `load_skill` for that skill before producing any final answer.
- After each `load_skill`, re-check the conversation for newly applicable skills (e.g., the loaded skill may introduce new function stubs). If another skill’s `when_to_use` now matches, you MUST `load_skill` it as well.
