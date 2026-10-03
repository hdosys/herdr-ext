# BACKLOG.md

Current-user-selected future Herdr Extended product outcomes only. User-visible product
rules belong in `PRODUCT.md`; stable technical design belongs in
`ARCHITECTURE.md`; workflow/tooling/test/skill proposals belong in
`AGENT_IMPROVEMENTS.md`.

## Rules

- Keep items actionable and current.
- Remove completed or obsolete items instead of preserving history.
- Do not use this file for untriaged findings, verification assignments, evidence,
  test reminders, task logs, accepted product rules, architecture, or agent
  process notes.

## Items

- Make automated messages to OpenCode arrive independently of the human's
  unfinished input. Preserve the draft while agent communication continues,
  without manual input-ownership modes or a release step. Scope this outcome to
  OpenCode; do not claim a generic fix for other agents. The selected technical
  design and boundaries are in
  [Draft-safe OpenCode messages](ARCHITECTURE.md#draft-safe-opencode-messages).
