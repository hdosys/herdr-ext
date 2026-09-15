---
description: Audit Herdr Extended candidate and release readiness
---

# Herdr Extended pre-release audit

Audit this fork against its recorded stable source and manual build/promote model. `CONTRIBUTING.md` owns the procedure; this prompt is a concise operator checklist.

Optional context: `$1 ${@:2}`

## 1. Establish the release boundary

- Require `master`, inspect the complete status/diff, and compare `HEAD` with `origin/master` without discarding unrelated work.
- Read `patches/delta/BASE` and `patches/delta/series`. `BASE` is the release source; never substitute upstream `master` or the latest tag.
- Do not query or fetch official upstream during this audit unless the user separately requested an upstream refresh.
- Read `PRODUCT.md`, `ARCHITECTURE.md`, and `CONTRIBUTING.md` before judging readiness.

## 2. Verify the maintained source

- Run the control-plane inventory tests.
- Replay every entry in `series` with `git am --3way` on a fresh checkout of recorded `BASE`.
- Require the replayed tree to match the reviewed logical source and keep one responsibility per mailbox.
- Run the relevant formatting, Clippy, Rust, package, native, and installer gates from `CONTRIBUTING.md` on one frozen snapshot.
- Treat a replay conflict, source drift, wrong protocol/build identity, or failed native/package boundary as blocking.

## 3. Audit public documentation

- Require root `README.md` and `docs/next/README.md` to be byte-identical.
- Check README, `PRODUCT.md`, `ARCHITECTURE.md`, `CONTRIBUTING.md`, `BACKLOG.md`, and both changelogs against current code and workflow behavior.
- Ensure the changelog contains the stable version represented by `BASE`; qualify new upstream issue references as `herdrdev/herdr#N` while preserving imported historical qualifiers.
- Keep general Herdr documentation and upstream community material clearly attributed to official upstream.
- Claim platform availability only when a successful candidate or published release provides that exact artifact.

## 4. Build and promote

- Choose one unused Herdr Extended CalVer `YYYY.MM.DD.N`.
- Push the verified control commit, then dispatch `release.yml` with `operation=build` and that CalVer.
- Require the successful retained candidate to contain the Windows ZIP/setup plus matching Linux and macOS amd64/arm64 binaries, coherent source/control identities, and verified digests.
- Promote only that successful build run ID with `operation=promote`. Promotion must publish the retained bytes without replaying, rebuilding, or repackaging them.
- Do not run upstream `just release`, create an ad hoc release tag, or publish from an ordinary push.

## 5. Report

```text
Release readiness: READY | NOT READY
Control commit: <sha>
BASE: <sha and Herdr version>
Replay tree: <sha>
Verification: <passed gates and exact blockers>
Documentation: CURRENT | NEEDS CHANGES
Candidate: NOT STARTED | <run ID and result>
Promotion: NOT STARTED | <run ID and result>
Required next action: <one exact action>
```

Keep the report decision-first. Separate required defects from optional polish and never broaden a failed gate into unrelated cleanup.
