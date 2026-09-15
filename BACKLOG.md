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

- Rename the repository and distribution presentation to **Herdr X** (**Herdr
  Extended**) when the migration can be scheduled, while preserving the runtime
  command, state, and protocol identity as `herdr`.
- Make OpenCode activity reporting keep the sidebar indicator busy whenever the
  visible root or a direct child session is still working; currently a working child
  can transiently appear idle until a later OpenCode event corrects the state.
