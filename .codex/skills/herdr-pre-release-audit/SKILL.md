---
name: herdr-pre-release-audit
description: Audit Herdr Extended candidate and release readiness against recorded BASE, the maintained queue, public docs, and the manual build/promote workflow.
---

# Herdr Extended pre-release audit

Use this skill only inside the `hdosys/herdr-ext` repository.

Read `references/pre-release-audit.md` and follow its workflow. Treat it as the source of truth for:

- using recorded `patches/delta/BASE` and `series`
- replaying and verifying the maintained source
- auditing the mirrored README and project-owned Markdown
- validating the six-platform retained candidate
- promoting only the exact successful build run
- producing the final release-readiness report

Do not edit files during the audit unless the user explicitly asks to apply fixes. When applying fixes, keep changes scoped to the files named in the reference workflow.
