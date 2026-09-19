# Herdr Extended

**An upstream-friendly [Herdr](https://github.com/herdrdev/herdr) distribution for developers who need extended capabilities today: multi-agent extensions, terminal experience improvements, better OpenCode integration, and first-class Windows support.**

[![Latest stable release](https://img.shields.io/github/v/release/hdosys/herdr-ext?display_name=tag&sort=semver)](https://github.com/hdosys/herdr-ext/releases/latest) [![Patch replay](https://github.com/hdosys/herdr-ext/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/hdosys/herdr-ext/actions/workflows/ci.yml) [![Release pipeline](https://github.com/hdosys/herdr-ext/actions/workflows/release.yml/badge.svg?branch=master)](https://github.com/hdosys/herdr-ext/actions/workflows/release.yml) [![Rust 1.96.1](https://img.shields.io/badge/Rust-1.96.1-000000?logo=rust&logoColor=white)](https://github.com/hdosys/herdr-ext/blob/master/rust-toolchain.toml) [![Built with Herdr Sandbox](https://img.shields.io/badge/built%20with-Herdr%20Sandbox-0078D4?logo=windows11&logoColor=white)](https://github.com/hdosys/herdr-sandbox) [![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/hdosys/herdr-ext/blob/master/LICENSE)

Herdr Extended is an unofficial, upstream-first distribution that makes practical bug fixes and extensions available today through a small, reviewable patch queue designed for upstream adoption. Its strongest focus areas are multi-agent workflows, terminal experience, OpenCode reliability, and Windows support. It complements upstream Herdr while the executable, command, configuration, state, sessions, sockets, and protocol remain `herdr`.

Every published release contains matching Windows, Linux, and macOS binaries built from one reviewed stable Herdr release and one ordered patch queue. Releases are normal stable GitHub releases, and the integrated update paths reject prerelease feeds.

> [!IMPORTANT]
> GitHub's **ahead/behind** banner compares commit ancestry, not release-source freshness. This repository's `master` is a control branch for the patch queue and release automation, not a mirror of upstream `master`. Each build starts from the stable commit recorded in [`BASE`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/BASE) and applies the maintained patches. GitHub's **Sync fork** action is not this project's refresh mechanism.

[What differs from upstream](#what-differs-from-upstream) · [Install](#install) · [First use](#first-use) · [Everyday use](#everyday-use) · [Troubleshooting](#troubleshooting) · [Project reference](#project-reference)

## See it in action

https://github.com/user-attachments/assets/b6c02367-683b-4a1f-94e6-b662149d89d9

Detach from a Windows-hosted Herdr session, reconnect from another terminal, and continue the same OpenCode session without RDP.

## Engineering approach

- **Upstream-first and contribution-oriented:** each behavior has one responsibility-owned mailbox designed for focused upstream review and leaves the queue when equivalent support ships upstream.
- **One coherent distribution:** release assets share one source tree and build identity. Compatible attachment negotiates the upstream endpoint protocol; provisioning verifies the exact payload.
- **Real boundary evidence:** Windows setup, ConPTY packaging, SSH provisioning, updates, uninstall, and cross-platform artifacts are exercised at their product-owned boundaries before publication.
- **No parallel product:** fork identity stays in repository, release, update-feed, setup, and Installed Apps presentation while normal Herdr commands and state remain unchanged.

> [!NOTE]
> Herdr Extended is developed and validated with [**Herdr Sandbox**](https://github.com/hdosys/herdr-sandbox), a sister project that provides disposable native Windows environments for coding agents. It is not a runtime dependency.

## How it works

```mermaid
flowchart TB
    S["Reviewed stable source<br/>Upstream Herdr v0.9.0 / BASE b99002ac99b0"]
    Q["Control branch: patches/delta/series<br/>0001 Terminal experience / 0003 Windows SSH / 0004 Managed distribution<br/>0005 OpenCode / 0006 Downloads / 0008 Worktree lifecycle<br/>0009 Agent start / 0010 Hook recovery / 0011 Metadata<br/>0012 Completion / 0013 History / 0015 Contrast / 0016 Integration hints"]
    B["Corrections after the foundation<br/>0019-0027, 0030 Reviewed upstream backports<br/>0028-0029, 0031 Local fixes and discoverability"]
    D["Current maintained source<br/>Finalized queue reproduces the accepted source tree"]
    V["Validated distribution<br/>Fresh replay → native + cross-platform gates<br/>→ Windows setup + ZIP, Linux/macOS binaries + digests"]
    S --> Q --> B --> D --> V
```

[`patches/delta/BASE`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/BASE) records the exact reviewed upstream stable commit. [`series`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/series) is the only patch order. A manual build replays that source and retains one complete candidate; promotion publishes those exact bytes without rebuilding or repackaging them.

## What differs from upstream

The table is intentionally capability-level. ✅ marks complete capabilities now provided by upstream. 🟡 **Partly upstream** marks a foundation already in v0.9.0 whose linked mailbox carries only the remaining extensions.

| Area | Status | What this repository contributes |
| --- | --- | --- |
| Native ConPTY foundation | ✅ **Upstreamed in Herdr v0.6.9** | Reuses Herdr's modern app-local ConPTY packaging instead of carrying a duplicate foundation. |
| Terminal experience | 🟡 **Partly upstream in Herdr v0.9.0; extended here** · [`0001`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0001-windows-terminal-appearance.patch) | Builds on upstream appearance handling; follows the host by default when unconfigured, preserves host and child cursor colors, supports Windows VTI observations, and avoids automatic OSC 4 palette queries. |
| Windows SSH target support | 🟡 **Partly upstream in Herdr v0.9.0; extended here** · [#2329](https://github.com/herdrdev/herdr/pull/2329) · [`0003`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0003-windows-remote-attach.patch) | Reuses upstream Windows-client SSH transport; adds Windows x86_64/ARM64 targets, exact provisioning and activation, visible progress, and fail-closed launch in the SSH user's active desktop session. |
| Managed Windows releases | 🟡 **Partly upstream in Herdr v0.9.0; extended here** · [`0004`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0004-windows-managed-distribution.patch) | Builds on upstream portable ZIP and per-user installation; adds branded setup, immutable leased runtimes, fork-owned stable updates and WinGet ownership, and process-safe uninstall. |
| OpenCode and multi-Agent workflows | 🟡 **Partly upstream in Herdr v0.9.0; extended here** · [#3052](https://github.com/herdrdev/herdr/issues/3052) · [#2450](https://github.com/herdrdev/herdr/issues/2450) · [`0005`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0005-opencode-retry-notifications.patch) | v0.9.0 fixes pane-selected session tracking and child-prompt recovery. This fork adds retry/error correlation, stricter pane-local root ownership, and adaptive panes for concurrent direct subagents. |
| Runtime downloads | **Maintained here** · [`0006`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0006-harden-curl-transfers.patch) | Ignores user `curl` configuration and bounds runtime downloads to TLS 1.2+ HTTPS with limited redirects. |
| Cross-platform docs checks | ✅ **Upstreamed in Herdr v0.9.0** · [#3041](https://github.com/herdrdev/herdr/issues/3041) | Upstream's documentation-parity assertion now uses native path separators on Windows and POSIX systems. |
| Worktree lifecycle | 🟡 **Partly upstream in Herdr v0.9.0; extended here** · [#3044](https://github.com/herdrdev/herdr/issues/3044) · [`0008`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0008-worktree-scoped-git-trust.patch) | Builds on upstream request-scoped trust and safer Windows removal; automatically scopes trust to the exact checkout and waits for PTY shutdown before unregistering it. |
| Managed Agent start | 🟡 **Partly upstream in Herdr v0.9.0; extended here** · [#321](https://github.com/herdrdev/herdr/issues/321) · [#2685](https://github.com/herdrdev/herdr/issues/2685) · [`0009`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0009-session-agent-autostart.patch) | Builds on upstream PowerShell npm-shim launching; adds optional new-tab Agent start, live-reload catch-up, and selected-shell rendering shared with session restore. |
| Agent hook recovery | **Maintained here** · [#1033](https://github.com/herdrdev/herdr/issues/1033) · [`0010`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0010-agent-transient-hook-takeover.patch) | Lets a still-running full-lifecycle Agent regain hook authority after a temporary foreground takeover without reviving a session after a real exit. |
| Metadata capacity | **Maintained here** · [`0011`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0011-metadata-token-capacity.patch) | Atomically updates and retains up to 64 pane or workspace metadata tokens while preserving existing validation bounds. |
| Completion alerts | 🟡 **Partly upstream in Herdr v0.9.0; controls maintained here** · [`0012`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0012-agent-completion-controls.patch) | Adds a persistent completion-only popup and sound opt-out plus cancellation suppression without disabling questions, permission prompts, or errors. |
| Terminal history | **Maintained here** · [#2893](https://github.com/herdrdev/herdr/issues/2893) · [`0013`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0013-terminal-history-scroll-aliasing.patch) | Retains repeated rows that scroll into alternate-screen history. |
| Plugin command resolution | ✅ **Upstreamed in Herdr v0.9.0** · [#3024](https://github.com/herdrdev/herdr/issues/3024) | Upstream resolves explicit relative pane commands from the linked plugin root, including Windows plugin-local executables. |
| Muted-label contrast | **Maintained here** · [#2692](https://github.com/herdrdev/herdr/issues/2692) · [`0015`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0015-muted-label-contrast.patch) | Keeps muted sidebar and inactive tab labels readable. |
| Integration settings | **Maintained here** · [#2880](https://github.com/herdrdev/herdr/issues/2880) · [`0016`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/0016-settings-integration-hints.patch) | Shows only controls that the selected integration supports. |
| Devin configuration | ✅ **Upstreamed in Herdr v0.9.0** · [#2724](https://github.com/herdrdev/herdr/issues/2724) | Upstream finds Devin's native configuration in roaming AppData while respecting an explicit XDG override. |
| Windows process environment | ✅ **Upstreamed in Herdr v0.9.0** · [#3430](https://github.com/herdrdev/herdr/issues/3430) | Upstream rejects malformed Windows environment entries and validates registry values before process creation. |
| Remote transport and input corrections | **Included in v2026.09.10.2** · [Inventory](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/README.md#reviewed-v090-corrections) | Preserves Windows download and endpoint-write progress, matching control-key text, and nested mouse input. |
| Multi-machine workspace views | **Included in v2026.09.10.2** · [Inventory](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/README.md#reviewed-v090-corrections) | Scopes collapsed groups to each machine, focuses new worktrees in the requesting client, supports guarded cross-machine navigation, and preserves public focus and focused geometry. |
| Session safety and startup diagnostics | **Included in v2026.09.10.2** · [Inventory](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/README.md#reviewed-v090-corrections) | Requires exact recorded names for session deletion and keeps Local startup errors visible without freezing healthy remote views. |

These corrections are included in Herdr Extended v2026.09.10.2. They keep the reviewed
v0.9.0 base unchanged. An upstream PR reference is provenance, not a claim that the
fix has shipped in upstream stable; temporary mailboxes remain until equivalent
stable behavior includes the necessary adaptations.

**Unpublished development candidate:** script-generated status labels can include
multiple OSC 8 web links in one command output. Click an underlined text section or use prefix-F10, Up/Down, and Enter
to open it in the client's browser, even when the producer runs remotely. Existing
plain-text commands still work. See the [producer contract](https://github.com/hdosys/herdr-ext/blob/candidate/development/docs/next/website/src/content/docs/configuration.mdx)
before adapting scripts. This capability is not yet in the maintained release queue.
The candidate also starts your preferred agent in the current available shell with
**prefix+o**, including selected remote panes. Configure `[agent]` on the client;
OpenCode is the default. **prefix+Shift+O** opens the notification target.
No shell alias is needed; [keybinding configuration](https://github.com/hdosys/herdr-ext/blob/candidate/development/docs/next/website/src/content/docs/configuration.mdx#keybindings)
remains client-owned by default.

## Install

### Choose an installation method

Every release provides Windows, Linux, and macOS builds as one coherent distribution. Choose one installation method for each machine:

- **Windows:** download the direct setup or use the portable ZIP.
- **Linux and macOS:** download the executable for your platform and architecture.

Windows managed installations are per-user, require no administrator access, and need no separately installed Microsoft Visual C++ Redistributable. The Windows portable ZIP is a standalone alternative.

### Windows direct setup

Download the Windows setup from the [latest Herdr Extended release](https://github.com/hdosys/herdr-ext/releases/latest), verify its GitHub SHA-256 digest, and run it.

<p align="center">
  <img src="https://raw.githubusercontent.com/hdosys/herdr-ext/master/docs/assets/herdr-extended-setup-welcome.png?v=2026.09.15.4" alt="Herdr Extended setup welcome page">
</p>

The managed install lives under `%LOCALAPPDATA%\Programs\Herdr`, registers **Herdr Extended** in Installed Apps, installs Herdr's canonical agent skill, and preserves customized skill copies.

### Windows portable

The Windows release also includes a matching portable ZIP. Extract the complete archive into one directory and run `herdr.exe`; keep its ConPTY payload beside it.

### Linux and macOS

Linux and macOS releases are raw `linux_amd64`, `linux_arm64`, `macos_amd64`, and `macos_arm64` executables. Clients negotiate endpoint generation and required codecs. Optional cursor-color support uses a named extension; exact provisioning still uses matching Herdr Extended release assets.

After downloading a Linux or macOS asset, mark it executable, rename it to `herdr`, and place it in a directory on `PATH`.

> [!WARNING]
> The executable and setup are currently unsigned, so Windows may show a SmartScreen warning. Download only from this repository and verify the SHA-256 digest before running the artifact.

## First use

Open a new terminal after installation and run:

```powershell
herdr --version
herdr
```

A published build reports `herdr-ext <CalVer> (Herdr <upstream-version>)`. The second command opens Herdr's normal keyboard-first terminal interface. General commands, configuration, keybindings, and integrations remain documented by the [official Herdr guide](https://herdr.dev/docs/).

## Everyday use

### Mixed-platform sessions

Run the client and server on Windows, or use an endpoint-compatible Linux or macOS client to control a Windows workstation or VM. Windows can also connect to compatible Linux and macOS endpoints. A compatible version difference alone does not restart a running server; saved-machine reconnects never install, replace, or start a missing server.

Every supported client can attach to or provision an x86_64 or ARM64 Windows SSH host. Use `--yes` to approve a required install or restart for one normal attach; unattended provisioning remains explicit:

```powershell
herdr --remote workbox --yes
herdr --remote workbox --provision --yes --json
```

The Windows SSH user's OpenSSH default shell must be `cmd.exe` or PowerShell 7 (`pwsh.exe`), and persistent server launch requires exactly one active desktop session owned by that user. The first probe is reused for the complete decision, the portable payload transfers once, and visible progress reports every real preparation, validation, stop, activation, verification, and opening phase. Provisioning validates the complete payload before stopping or replacing a server and verifies the exact binary, version, and protocol afterward.

### Fork-specific options

Start OpenCode automatically in the root pane of each genuinely new persistent-session tab:

```toml
[session]
auto_start_agent = "opencode"
```

This managed launch also supplies the ephemeral loopback endpoint used by direct
OpenCode subagent panes. Typing a bare `opencode` command starts OpenCode's internal
worker transport without that attach endpoint, so use the managed path for this
integration.

Use **Settings > completion**, or disable completion popups and done sounds without suppressing questions, permission prompts, or errors:

```toml
[ui]
notify_on_agent_completion = false
```

### Updates

**Herdr Win has been renamed to Herdr Extended.** Release downloads now start
with `herdr-ext`; the command remains `herdr` and the WinGet ID remains
`hdosys.herdr-win`.

- WinGet installation: `winget upgrade --id hdosys.herdr-win --exact --source winget`.
  New releases appear there after the WinGet manifest is accepted into the catalog.
- Manual Windows installation: download and run the current installer from the
  [release page](https://github.com/hdosys/herdr-ext/releases/latest). It updates an
  existing managed installation without uninstalling it or removing your settings.
- Portable, Linux, or macOS installation: replace it with the matching release
  download, or use `herdr update` on clients that support the renamed assets.

If an older client's built-in update rejects the new filename, use the package
manager or manual download above instead.

Direct updates accept only a newer stable CalVer from an immutable normal GitHub release. Active sessions continue on their current immutable runtime and the replacement activates safely afterward; update never terminates running work. `herdr update` refuses to replace a WinGet-managed installation.

## Uninstall

Uninstall from **Windows Settings > Apps > Installed apps**. Herdr first asks running managed sessions to stop through their graceful server API. If a session remains active, uninstall preserves the installation and reports the required action instead of force-terminating work.

Settings under `%USERPROFILE%\.herdr` are preserved unless you explicitly choose to remove them. Installer-owned skill files can also be removed explicitly; customized copies and unrelated directory content remain preserved.

## Troubleshooting

| Symptom | Action |
| --- | --- |
| `herdr --version` does not start with `herdr-ext` | Open a new terminal, run `where.exe herdr`, and inspect an earlier upstream or user-owned executable on `PATH`. Setup does not overwrite foreign PATH ownership. |
| Setup rejects an existing Herdr layout | Uninstall the existing **Herdr** entry from Installed Apps, then run setup again. The installer preserves and rejects incompatible layouts instead of migrating them. |
| SmartScreen warns about the download | Confirm that the file came from this repository's release page and verify its GitHub SHA-256 digest before choosing to run it. |
| Windows SSH provisioning fails before session start | Confirm that the default OpenSSH shell is `cmd.exe` or `pwsh.exe` and that exactly one active desktop session belongs to the SSH user. Windows PowerShell 5.1 is unsupported for this byte-stream path. |
| Update remains pending, or uninstall reports running sessions | Let active work finish or stop the reported Herdr sessions, then launch or retry. The managed lifecycle never force-terminates active work. |

For exact changes in downloadable releases, see the [Herdr Extended changelog](https://github.com/hdosys/herdr-ext/blob/master/CHANGELOG.md). For general Herdr behavior, use the [upstream documentation](https://herdr.dev/docs/) and [upstream changelog](https://github.com/herdrdev/herdr/blob/master/CHANGELOG.md).

## Project reference

This README describes the maintained queue on `master`. The distribution [changelog](https://github.com/hdosys/herdr-ext/blob/master/CHANGELOG.md) is the exact user-facing history for tagged Herdr Extended releases. Upstream Herdr owns the general CLI, TUI, configuration, integrations, and product documentation.

<details>
<summary><strong>Patch queue and upstream review</strong></summary>

Upstream PR [#2329](https://github.com/herdrdev/herdr/pull/2329) ships in Herdr v0.8.2. Mailbox `0003` therefore contains only the remaining Windows target-host boundary. Shared client attach, image transport, and SSH bridge behavior come directly from upstream.

The original Windows-host work builds on [nsxdavid's `feat/windows-remote-attach` branch](https://github.com/nsxdavid/herdr/tree/feat/windows-remote-attach).

The files in `patches/delta/series` are the complete maintained product delta:

1. Start at the exact commit in [`BASE`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/BASE).
2. Apply [`series`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/series) in order with `git am --3way`.
3. Review each mailbox as one responsibility with its implementation, tests, and documentation.
4. Follow [`CONTRIBUTING.md`](https://github.com/hdosys/herdr-ext/blob/master/CONTRIBUTING.md) for replay and verification.

The mailboxes are focused evidence, not an all-or-nothing merge request. Fork branding, release workflows, and publication state stay outside the product queue.

</details>

<details>
<summary><strong>Maintaining the project</strong></summary>

Refresh and release are separate manual operations:

- **Refresh:** select and review a stable upstream release, then replay and minimize the queue.
- **Build:** replay recorded `BASE`, run the complete gates, and retain one candidate with provenance and checksums.
- **Promote:** publish those exact retained bytes without rebuilding or repackaging them.

Ordinary pushes do not publish binaries.

| Need | Canonical owner |
| --- | --- |
| User-visible fork behavior | [`PRODUCT.md`](https://github.com/hdosys/herdr-ext/blob/master/PRODUCT.md) |
| Technical boundaries | [`ARCHITECTURE.md`](https://github.com/hdosys/herdr-ext/blob/master/ARCHITECTURE.md) |
| Patch ownership and refresh policy | [`patches/delta/README.md`](https://github.com/hdosys/herdr-ext/blob/master/patches/delta/README.md) |
| Replay, verification, and release procedure | [`CONTRIBUTING.md`](https://github.com/hdosys/herdr-ext/blob/master/CONTRIBUTING.md) |
| Selected future product work | [`BACKLOG.md`](https://github.com/hdosys/herdr-ext/blob/master/BACKLOG.md) |

</details>

### Issues and contributions

- Use [upstream Herdr](https://github.com/herdrdev/herdr) for general behavior that reproduces with an official upstream build.
- Use [Herdr Extended issues](https://github.com/hdosys/herdr-ext/issues) for this distribution's artifacts, update feed, workflows, or maintained patches.
- Read [`CONTRIBUTING.md`](https://github.com/hdosys/herdr-ext/blob/master/CONTRIBUTING.md) before changing the queue or release automation.

## Credits and license

Herdr is created and maintained upstream by [Can Çelik](https://github.com/ogulcancelik). Herdr Extended is distributed under the [Apache License 2.0](https://github.com/hdosys/herdr-ext/blob/master/LICENSE).
