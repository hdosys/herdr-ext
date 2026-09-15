# Herdr Extended repository overlay

The global OpenCode working agreement owns reusable personal workflow. This file
owns only Herdr Extended repository invariants and must not restate or silently
override generic delivery procedure. Project-specific commands select exact
owners and unsafe boundaries while the global interactive ordering remains
authoritative. Do not copy global workflow or create project `.opencode/`
configuration.

## Canonical owners and precedence

- Code and tests own detailed implementation behavior.
- `PRODUCT.md` owns stable user-visible fork behavior, terminology, supported
  choices, and acceptance outcomes.
- `ARCHITECTURE.md` owns stable technical boundaries, source/replay topology,
  Windows distribution design, state ownership, and verification architecture.
- `CONTRIBUTING.md` owns change classification, mailbox maintenance, replay,
  verification, documentation projection, commit, and upstream-engagement
  procedure.
- `BACKLOG.md` owns only current-user-selected future product outcomes.
- `AGENT_IMPROVEMENTS.md` owns evidence-backed Herdr Extended-specific workflow,
  tooling, test, and skill improvement proposals.

After system and current-user instructions, local precedence is this file,
`PRODUCT.md`, `ARCHITECTURE.md`, `CONTRIBUTING.md`, then `BACKLOG.md`.
Improvement proposals are not active rules until accepted into an owning file.

Route each durable decision to exactly one owner above. Cross-project OpenCode
behavior belongs in the global configuration repository. Do not invent another
active memory owner.

## Identity and source invariants

- Official upstream: `herdrdev/herdr`.
- This repository: `hdosys/herdr-ext`, an unofficial extended
  distribution fork.
- Release source is replayed upstream source plus the maintained delta, not an
  independently developed product line.
- Runtime identity remains `herdr`: package, executable, command, config, state,
  sockets, and protocol. Fork identity belongs only in repository/release/update
  presentation.
- Keep the delta small, explicit, replayable, and upstreamable.
- Choose changed behavior against recorded upstream and the simplest long-lived
  design. Keep one implementation path for each current contract.
- `patches/delta/` owns maintained product behavior; `series` owns order and
  `BASE` records the exact commit behind the latest non-draft, non-prerelease
  upstream stable release selected during the last explicit manual refresh. It
  never tracks upstream `master` or a preview tag.
- Patch generation and patch-queue writes require explicit current-user
  authorization in the current task. Unless the user explicitly requests an
  update, regeneration, or finalization of the maintained patches, or explicitly
  requests creation or publication of a release, never invoke a patch-generation
  or mailbox-finalization command and never create, modify, reorder, rename,
  stage, commit, or push anything under `patches/`, including `patches/delta/BASE`
  and `patches/delta/series`. A source fix, completed topic, development-branch
  integration, matching replay tree, installer build or acceptance, clean-slate
  request, improvement task, or repository invariant does not imply this
  authorization. Work remains on the topic branch or `candidate/development`.
  When the current request does not already name the patch update or release,
  stop immediately before the first patch-generation or patch-writing action and
  ask the user explicitly. Authorization from an earlier task never carries
  forward.
- Never fetch, clone, query, download, check out, replay, test, or otherwise obtain
  anything from official upstream `herdrdev/herdr` unless the user explicitly
  requests that exact upstream operation in the current task. This prohibition also
  covers changing `BASE` or refreshing maintained mailboxes; use only already-local
  objects and the commit recorded in `BASE`, and report a blocked gate instead of
  reaching upstream. When the user explicitly requests an upstream refresh, query
  and fetch the latest stable release, peel its exact commit, and update `BASE` only
  after the complete queue is replayed, reviewed, and verified there.
  Synchronizing this fork's configured `origin` for normal collaboration and
  delivery remains allowed because it is not official upstream. A manual
  `workflow_dispatch` release uses recorded `BASE`.
- `patches/upstream/` is a frozen patch archive. Never regenerate, rename, or
  delete it; external links may depend on exact paths.
- `scripts/test_delta_patches.py` and `scripts/test_upstream_patches.py` own queue
  control invariants.
- `.github/workflows/ci.yml` owns cheap PR/manual replay validation.
- `.github/workflows/release.yml` owns the manually dispatched candidate build and
  exact-artifact promotion path. It has no scheduled trigger. A build requires an
  explicit Herdr Extended CalVer and always replays recorded `BASE`; promotion requires
  the successful build run ID and never rebuilds or repackages its retained
  candidates.
- `website/preview.json` is generated channel state; the release workflow's
  promotion operation is its only writer.
- `src/distribution.rs` in replayed source owns the fork channel and source URLs.
- Root `README.md` and `docs/next/README.md` are one mirrored public-documentation
  projection of relevant `PRODUCT.md` behavior.

## Repository and session boundaries

- Treat this directory as the root and `master` as the control branch; stop for
  direction on another branch.
- Preserve recovery stashes and unrelated shared-worktree changes.
- Never commit generated replay/build evidence, logs, binaries, credentials,
  private data, or temporary worktrees.
- Maintained product work defaults to the one shared
  `candidate/development` source worktree. The local branch and its pushed
  `origin/candidate/development` target use the same name and form the one
  cumulative cross-session development state. Sessions serialize and coordinate
  there. Create a topic worktree only for a concrete parallel collision or risky
  isolation boundary.
- The agent that creates a topic worktree owns its complete internal lifecycle:
  focused verification, integration into `candidate/development`, remote
  durability, and removal of its task worktree, local branch, and temporary remote
  ref after integration. This cleanup has standing user authorization when exact
  integration and ownership are proven. Never ask the user to manage these items.
- The canonical local user-testable artifact is
  `target/x86_64-pc-windows-msvc/release/herdr-win_local_candidate_setup.exe`.
  Only the exact `candidate/development` source stack may replace it, and it
  contains the current replay plus every completed development change. Topic
  worktrees keep any required package output temporary and remove it after their
  focused check; they never produce or replace the canonical installer.
- User-facing handoffs add only this repository's included outcomes and result to
  the global artifact evidence. Internal worktrees, branches, candidate refs, and
  queues stay internal unless an unresolved safety blocker requires a decision.
- Every committed topic-worktree head must be contained in
  `candidate/development` before its fixed installer can build. Uncommitted topic
  work remains in progress and is never represented as completed user-testable
  behavior.

## Product and implementation constraints

- Preserve the user-visible promises in `PRODUCT.md` and technical boundaries in
  `ARCHITECTURE.md`; do not hide a changed product or architecture decision inside
  a mailbox refresh or procedural edit.
- The Windows installer accepts only the current managed install layout and exact
  product-owned roots. Reject every other root with the documented uninstall-first
  action. Preserve current managed locked-binary safety through immutable runtimes,
  per-build leases, pending activation, and process-safe uninstall.
- Status inspection is pure and never starts or retries work.
- Keyboard-first terminal interaction remains complete end to end.
- Windows PTY integration owns process-tree cleanup, handle inheritance, resize,
  and byte-stream behavior explicitly. Compile Windows-only code only for Windows.
- Do not add `unsafe` unless unavoidable at a reviewed FFI boundary.
- Avoid production `unwrap`/`expect`; propagate contextual errors.
- Use `tracing` instead of production `eprintln!`/`dbg!`.
- Keep async cancellation-safe and avoid holding non-async locks across `.await`.
- Keep `#[allow(...)]` narrow, local, and justified.

## Fork verification policy

- Leave tests that are unchanged from recorded `BASE` alone. Checks added or
  changed by this fork follow the global Lean test policy.
- A release or CI workflow must not install or download software used only by a
  test. External programs are allowed only when they are current product or build
  dependencies and the workflow validates the product-owned integration boundary.
- Broad native and installer matrices remain release or explicitly assigned
  unattended work, never ordinary iteration gates.
