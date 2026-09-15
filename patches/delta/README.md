# Herdr Extended maintained delta

This is the canonical product delta applied by the manually dispatched Herdr Extended
release workflow on top of the reviewed [`herdrdev/herdr`](https://github.com/herdrdev/herdr)
stable-release commit recorded in `BASE`.

The queue contains responsibility-owned feature patches and temporary reviewed
backports, not a monolith or a patch for every development commit.

The current queue targets Herdr v0.9.0 and retains these logical slots:

- 0001: client terminal appearance, negotiated cursor color, and Windows VTI input.
- 0003: Windows SSH adapter, compatible attach, exact provisioning, and bounded desktop launch.
- 0004: managed distribution, installer lifecycle, and fork update handling.
- 0005: OpenCode lifecycle, pane-local selection, and direct-child layouts.
- 0006: shared runtime `curl` policy.
- 0008: selected-path Git trust and bounded Windows worktree terminal shutdown.
- 0009: persistent-session Agent auto-start and shared shell-native launch.
- 0010: transient foreground takeover recovery.
- 0011: 64-token metadata capacity.
- 0012: client-local completion controls and cancellation suppression.
- 0013: repeated terminal-history rows.
- 0015: muted-label contrast in client presentation.
- 0016: section-aware integration settings hints.
- 0032: Herdr Extended runtime, installer, update-source, and documentation identity.

Slots 0007, 0014, 0017, and 0018 are absent because v0.9.0 already owns the
equivalent native-path docs assertion, plugin-root resolution, Devin configuration,
and Windows environment validation. Partial upstream adoption does not retire a
remaining responsibility.

## Reviewed v0.9.0 corrections

The following 13 logical mailboxes extend the reviewed 13-mailbox foundation
without advancing `BASE`. They are included in Herdr Extended v2026.09.10.2. An
upstream PR reference records provenance, not that its changes have shipped in an
upstream stable release.

| Slot | Responsibility | Reviewed source |
| --- | --- | --- |
| [0019](0019-windows-bridge-blocking-download.patch) | Blocking Windows SSH bridge downloads and native socket fixture | [PR 3661](https://github.com/herdrdev/herdr/pull/3661) |
| [0020](0020-windows-endpoint-writer-progress.patch) | Windows polling-peer write progress and stall detection | [PR 3721](https://github.com/herdrdev/herdr/pull/3721) |
| [0021](0021-associated-control-key-text.patch) | Matching associated text for control keys, including Tab and Escape | [PR 3592](https://github.com/herdrdev/herdr/pull/3592) |
| [0022](0022-nested-windows-mouse-input.patch) | Transactional nested Windows mouse reports | [PR 3742](https://github.com/herdrdev/herdr/pull/3742) |
| [0023](0023-endpoint-scoped-worktree-collapse.patch) | Endpoint-scoped collapsed worktree groups | [PR 3781](https://github.com/herdrdev/herdr/pull/3781) |
| [0024](0024-client-local-worktree-autofocus.patch) | New-worktree focus in the requesting client | [PR 3787](https://github.com/herdrdev/herdr/pull/3787) |
| [0025](0025-multi-machine-workspace-navigation.patch) | Guarded workspace navigation across connected machines | [PR 3755](https://github.com/herdrdev/herdr/pull/3755) |
| [0026](0026-public-agent-focus-projection.patch) | Successful public Agent focus on attached clients | [PR 3764](https://github.com/herdrdev/herdr/pull/3764) |
| [0027](0027-background-activation-geometry.patch) | Focused geometry during background activation | [PR 3746](https://github.com/herdrdev/herdr/pull/3746) |
| [0028](0028-exact-recorded-session-deletion.patch) | Exact recorded session names and live-session deletion refusal | Local correction for [issue 3819](https://github.com/herdrdev/herdr/issues/3819) |
| [0029](0029-local-startup-diagnostics.patch) | Visible Local startup diagnostics without freezing healthy remote views | Local correction for [issue 3759](https://github.com/herdrdev/herdr/issues/3759) |
| [0030](0030-client-sidebar-resize.patch) | Stable client-owned sidebar resizing | [PR 3862](https://github.com/herdrdev/herdr/pull/3862) |
| [0031](0031-machine-setup-discovery.patch) | Machine setup guidance in the client menu | Local discoverability improvement |

Keep each temporary correction with its original author credit, immutable source
references, and necessary fork adaptations. Retire it only during an explicitly
authorized stable refresh when equivalent stable behavior includes those
adaptations. Never retire a mailbox merely because its upstream PR was merged.
Retired identifiers remain unused. `series` remains the only application order;
no separate backport manifest or unreleased catch-all mailbox is maintained.

When a feature evolves, refresh its existing mailbox in place. Add a new patch
only when the change has a genuinely independent owner, verification plan, and
upstream integration path. This keeps replay conflicts localized without
turning the queue into task history.

## Files

- `BASE` records the exact commit behind the latest non-draft, non-prerelease
  upstream stable release selected during the latest explicit manual refresh.
- `series` is the only release application order.
- `*.patch` files are full-index, binary-safe `git format-patch` mailboxes.

Repository branding, GitHub Actions, and release orchestration are control-plane
files and do not belong in this product patch queue.

## Refreshing the queue

Run this procedure only for a current user-authorized official-upstream refresh. Ordinary fork work must use the commit already recorded in `BASE` without querying or fetching newer upstream source.

1. Query the official latest stable release, fetch and peel its `v<version>` tag,
   verify it is neither draft nor prerelease, and start a clean branch at that
   exact commit.
2. Apply `series` in order with `git am --3way`.
3. Resolve upstream drift in the patch that owns the behavior.
4. Keep one reviewed commit per logical patch, preserving its metadata and stable
   filename. Do not flatten merged development history into a single mailbox.
5. Use `delta_workflow.py refresh` as documented in `CONTRIBUTING.md`. It stages
   full-index binary mailboxes privately and proves the accepted tree before
   replacing the queue and `BASE`. Every ordered prefix must compile before control
   publication; evidence for unchanged staged prefix trees remains valid.

Validate the control-plane inventory with:

```powershell
python -m unittest scripts.test_delta_patches scripts.test_upstream_patches
```

Release replay never resolves conflicts automatically. A conflict means the
owning patch must be refreshed and reviewed.
