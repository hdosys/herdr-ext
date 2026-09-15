# Changelog

This changelog records only user-visible changes released by the `herdr-win` fork. For Herdr's general release history, see the [official upstream changelog](https://github.com/herdrdev/herdr/blob/master/CHANGELOG.md).

## Unreleased

## [2026.09.15.1] - 2026-09-15

Unofficial herdr-win snapshot based on Herdr v0.9.0 plus the maintained delta.

### Fixed
- OpenCode root panes stay busy while direct subagents run, while attached
  subagent panes report their own lifecycle instead of appearing as nested work.

## [2026.09.10.2] - 2026-09-10

Unofficial herdr-win snapshot based on Herdr v0.9.0 plus the maintained delta.

### Added
- One client can monitor and navigate saved SSH machines, with endpoint-scoped
  workspace state and a direct machine-setup guide in the client menu.

### Changed
- Windows remote image and input forwarding uses matching pipe and writer buffers,
  making large pastes substantially faster while preserving bounded backpressure.

### Fixed
- Host terminal appearance is applied when the client starts and remains current
  without allowing late palette replies to reach pane input.
- Sidebar resizing remains stable instead of briefly rendering the wrong view
  (`herdrdev/herdr#3817`).
- Windows SSH bridge downloads, endpoint writes, control-key text, and nested mouse
  reports remain responsive and correctly framed.
- Cross-machine workspace navigation, focus, collapse state, and geometry stay
  scoped to the requesting client and selected machine.
- Session deletion requires an exact recorded name, and Local startup failures stay
  visible without freezing healthy remote views.

## [2026.08.31.4] - 2026-08-31

Unofficial herdr-win snapshot based on Herdr v0.8.2 plus the maintained delta.

### Changed
- Managed Agent startup and native session restore now share the selected
  interactive shell's command renderer. PowerShell, cmd, Nushell, Git Bash, and
  `sh` use native command syntax instead of a PowerShell transport or a separate
  restore quoting path.

### Fixed
- Closing the final shell no longer lets its automatically created replacement
  workspace queue the configured Agent again.
- Repeated terminal rows that scroll out of the alternate screen remain visible
  in history (`herdrdev/herdr#2893`).
- Explicit relative plugin commands resolve from the linked plugin root instead
  of the current process directory (`herdrdev/herdr#3024`).
- Muted sidebar and inactive tab labels retain readable contrast
  (`herdrdev/herdr#2692`).
- Integration settings show only controls supported by that integration
  (`herdrdev/herdr#2880`).
- Devin's native configuration is found in roaming AppData
  (`herdrdev/herdr#2724`).
- Malformed Windows environment entries are omitted before process creation
  (`herdrdev/herdr#3430`).
- Uninstall now preserves the managed installation and fails closed when private
  pending-launcher state is malformed.

## [2026.08.27.5] - 2026-08-27

Unofficial herdr-win snapshot based on Herdr v0.8.2 plus the maintained delta.

### Added
- OpenCode direct subagents from Herdr-managed roots now open in adaptive readable splits beside Main, stay bound to the pane-selected root session after resume, and close when the child becomes idle or is deleted.
- New persistent sessions can start one configured interactive Agent in each new tab with `[session] auto_start_agent`, including eligible shell roots after a live configuration reload.
- Managed Agent launches preserve configured arguments through PowerShell, cmd, Nushell, Git Bash, and `sh` on Windows, with explicit Nushell external-command support on Linux and macOS.
- Interactive Windows remote updates report download, validation, activation, and restart progress.

### Changed
- herdr-win releases now publish as normal stable GitHub releases. Direct updates and Windows remote provisioning reject prerelease feeds.
- Pane and workspace metadata reports can atomically retain up to 64 custom tokens while preserving existing bounds.
- Agent completion alerts follow the upstream enabled default and can be disabled from **Settings > completion** or with `ui.notify_on_agent_completion = false` without suppressing questions, permission prompts, or errors.

### Fixed
- Windows SSH provisioning now starts the persistent server reliably in the authenticated user's active desktop session and keeps it independent of the transient SSH channel.
- Windows worktree removal waits for terminal sessions to release the checkout before unregistering it, and background removal preserves the currently focused workspace.
- Cancelling an OpenCode response no longer reports an error that asks for attention.
- A full-lifecycle Agent can regain hook authority after a temporary recognized foreground takeover ends.

## [2026.08.21.2] - 2026-08-21

Unofficial herdr-win snapshot based on Herdr v0.8.2 plus the maintained delta.

### Fixed
- Windows setup and portable archives now run on clean Windows systems without requiring a separately installed Microsoft Visual C++ Redistributable.

## [2026.08.21.1] - 2026-08-21

Unofficial herdr-win snapshot based on Herdr v0.8.2 plus the maintained delta.

### Fixed
- Windows SSH attach and provisioning no longer hang while preparing the remote PowerShell bootstrap when SSH also needs standard input.

## [2026.08.20.5] - 2026-08-20

Unofficial herdr-win snapshot based on Herdr v0.8.2 plus the maintained delta.

### Fixed
- Herdr worktree commands can now manage explicitly selected mounted repositories across Windows account boundaries without persistent or wildcard Git trust (`herdrdev/herdr#3044`).

## [2026.08.20.4] - 2026-08-20

Unofficial herdr-win snapshot based on Herdr v0.8.2 plus the maintained delta.

### Added
- Windows, Linux, and macOS clients can now provision and attach to x86_64 or ARM64 Windows SSH targets. Provisioning validates one exact portable payload and starts the persistent server in the SSH user's active desktop session.

### Changed
- Shared remote attach and clipboard/file image transport now come directly from Herdr v0.8.2 (`herdrdev/herdr#2329`); the maintained remote mailbox is limited to the remaining Windows target-host contract.
- Windows terminal synchronization keeps host foreground, background, and cursor colors while using Herdr's built-in indexed palette, preventing unframed OSC 4 replies from reaching pane input (`herdrdev/herdr#2786`).
- Runtime `curl` transfers ignore ambient configuration, require HTTPS with TLS 1.2 or newer, disable URL globbing, and bound redirects.

### Fixed
- OpenCode retry and error events no longer publish a terminal failure while the same turn is still actively retrying.
