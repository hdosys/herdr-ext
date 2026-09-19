# PRODUCT.md

## Purpose

This file is the short durable user-perspective truth for Herdr Extended: the distribution's
product promise, visible fork-specific behavior, terminology, supported choices,
and acceptance outcomes. Upstream Herdr owns general product behavior; code and
tests remain the detailed implementation truth.

## Product Shape and Identity

- Herdr Extended is an unofficial, upstream-first cross-platform distribution
  for developers who need extended Herdr capabilities today. It makes practical
  bug fixes and extensions available through a small, reviewable patch set designed
  for upstream adoption. Its strongest focus areas are multi-Agent workflows,
  terminal experience, OpenCode reliability, and Windows support; it complements
  upstream Herdr rather than presenting a competing product.
- Fork identity appears in repository, release, update-feed, Windows setup, and
  Installed Apps presentation. The Windows package entry is **Herdr Extended**; the
  executable, command, configuration, state, sessions, sockets, and protocol remain
  `herdr`. Herdr v0.9.0's frozen binary protocol and negotiated endpoint compatibility
  are preserved. Optional fork capabilities use named extensions rather than a
  second wire protocol or an exact-version requirement for compatible attachment.
- A published `herdr --version` reports `herdr-ext <CalVer> (Herdr
  <upstream-version>)`. A separately built local binary instead reports
  `herdr-ext <YYYY.MM.DD.HHMMZ> (local, Herdr <upstream-version>, build
  <build-id>)` when build provenance is available. The UTC label is generated once
  for the coherent local build and leads local runtime status and Windows setup
  presentation; Welcome and Finish headings omit the build ID, which remains
  secondary exact provenance in technical metadata. Integrations can
  require the stable `herdr-ext ` prefix without changing the executable, command,
  state, or protocol identity.
- Every Herdr Extended snapshot is a cross-platform release for Windows x86_64, Linux
  amd64/arm64, and macOS amd64/arm64. Windows additionally ships the managed
  per-user setup and portable archive; Linux and macOS ship raw executables from
  the same retained candidate. Supported clients and remote endpoints therefore
  negotiate endpoint compatibility in mixed-platform development environments.
  Exact remote provisioning uses matching assets instead of requiring
  manual binary copying or independently released builds. Windows can run both the
  interactive client and an SSH-reachable Herdr server.
  General CLI, TUI, configuration, integration, and issue behavior remains documented
  and owned upstream unless a maintained fork delta explicitly changes it.
- The maintained user-visible delta covers the Windows terminal experience, SSH
  target provisioning, and managed distribution; OpenCode reliability and multi-Agent
  layouts; managed Agent launch and recovery; completion controls and metadata
  capacity; worktree lifecycle; and focused cross-platform correctness and download
  hardening. Upstream Herdr owns general behavior unless a maintained delta changes
  it.

## Installation, Update, and Uninstall

- The normal managed installation is per-user under
  `%LOCALAPPDATA%\Programs\Herdr`, places its stable `bin` directory first on user
  `PATH` when no effective equivalent is already present, never claims or rewrites
  an equivalent user-owned entry, and otherwise shadows without changing any
  existing upstream/native `herdr`. It registers **Herdr Extended** in Windows Installed
  Apps without requiring administrator
  privileges. The installer interface is English-only. Its branded,
  keyboard-operable Windows setup uses the human-facing display name **Herdr Extended**
  consistently while the repository slug is `herdr-ext`. The
  Welcome description derives and displays the current reviewed Herdr base version
  from the canonical package input. Setup identifies the
  fork as an unofficial distribution built from the latest reviewed stable Herdr
  release plus the maintained Windows
  patches. The destination remains fixed without displaying a path or offering a
  directory choice on Welcome. Setup presents the Apache-2.0 license before modifying
  files and ends with the exact first command plus separate user-invoked links to the
  fork guide and official upstream project. It never opens Herdr or a browser
  automatically. The installed payload and portable ZIP include the same license as
  `LICENSE.txt`. Their Windows executables require no separately installed Microsoft
  Visual C++ Redistributable. A portable ZIP remains a supported manual alternative.
- The managed installer copies upstream's canonical Herdr skill to
  `%USERPROFILE%\.agents\skills\herdr\SKILL.md`. When Claude Code is detected,
  it also copies the skill below `CLAUDE_CONFIG_DIR` or
  `%USERPROFILE%\.claude`. Install/update creates a missing copy and replaces only
  a `SKILL.md` whose bytes match a current or historical installer-delivered
  version. A customized or otherwise unknown copy is preserved and setup reports a
  visible warning; every sibling file and directory is always preserved.
- The managed installation has exactly one launcher at `bin\herdr.exe`. It starts
  the selected immutable runtime payload directly; runtime directories never carry
  a second launcher. Setup replaces the launcher immediately when idle or stages a
  replacement whose hash and embedded build ID are validated before publication
  after the final managed payload exits. Every retained release candidate has a
  distinct runtime build ID, including retries or independent builds from unchanged
  source, so byte-different payloads never compete for one immutable runtime path.
- The repository exposes one current locally installable setup at
  `target/x86_64-pc-windows-msvc/release/herdr-ext_local_candidate_setup.exe`.
  Successful replacement removes superseded local input bundles; topic candidate
  outputs are temporary and never become a second user-facing installer.
- Setup updates only an exact current managed installation. Any other existing
  install layout, including the former runtime-local launcher design, is preserved
  and rejected with instructions to uninstall the existing **Herdr** or **Herdr Win**
  entry from Windows Installed Apps before running setup again; setup never
  migrates, backs up, or removes an incompatible layout. Within a registered current
  installation, setup normally recreates missing installer control files and
  replaces changed regular control-file bytes. If current-format drift cannot use
  that narrow repair, setup directly rebuilds the dedicated managed root from its
  validated embedded payload after proving that no managed process or lease is
  active. Reparse points and active installed files remain preserved blockers;
  user configuration and projects remain outside this root. A current managed root
  remains updateable when its Installed Apps registration is absent or uses the
  immediately preceding current value set; setup safely defaults a missing PATH
  value-ownership fact to unowned and rewrites the complete current registration.
  Before setup adds its exact PATH entry it persists one ownership-intent marker;
  Installed Apps registration finalizes that ownership. An interrupted setup
  therefore claims the fixed literal managed path while pre-existing equivalent
  user entries remain unowned. On uninstall it removes every normalized literal
  spelling of that fixed path, including case, slash, quote, or trailing-separator
  variants, so no dead managed-path alias remains. Environment expressions and
  foreign entries remain preserved. The intent also
  records whether setup created the user `PATH` value itself, so uninstall restores
  an originally absent value as absent instead of leaving an empty registry value.
  Exact rooted literal entries may contain `%`; expandable entries such as
  `%LOCALAPPDATA%\...` remain foreign and are never removed as installer-owned.
- Installed/current after setup and absent after uninstall are the terminal product
  outcomes. Fresh/update sibling directories are private disposable staging, not a
  second ownership or recovery authority; stale or malformed staging is removed
  when safe and otherwise preserved with a warning without blocking the requested
  operation. One stable Windows named mutex serializes setup, update, and uninstall
  by acquired ownership, including recovery from an abandoned owner. Uninstall uses
  that lifecycle mutex and the existing launcher lock plus one
  `uninstall.pending` launch gate, then atomically moves `bin` and immutable runtimes
  into disposable same-parent staging before final metadata/self-cleanup. A stopped
  removal therefore leaves either the complete owned directory or no directory in
  the managed root, never a partially deleted tree. The marker remains the final
  filesystem ownership sentinel until residual enumeration succeeds. Final cleanup
  removes it before the retry executables, so an interruption either retains a
  complete retry root or leaves an exact cleanup residual that the next setup can
  finish. Any reported filesystem, PATH, or Installed Apps failure restores exact
  retry commands and installer-owned registration, so direct and silent retries may
  resume from the remaining root without parsing a separate uninstall journal.
  While the marker is present, the stable launcher refuses to start a new managed
  session.
- Direct setup and portable installs update through only the fork-owned immutable
  setup asset and verified digest through the fork-owned update feed. Existing
  managed sessions continue on their current runtime; a newer runtime may remain
  staged until old sessions exit,
  after which future launches switch atomically. The post-exit maintenance path
  then publishes any pending launcher and removes every exact unleased runtime
  except Active and optional Pending. Busy or ambiguous content is preserved and
  reported. A hard process-tree kill leaves pending state recoverable for the next
  safe launch or setup. Update never terminates active Herdr sessions.
- The installed distribution owns its update feed. A Herdr Extended binary has no
  user-selectable stable/preview channel or `update.channel` setting and cannot be
  redirected to official Herdr update sources. CalVer `YYYY.MM.DD.N` orders fork
  releases: a published binary accepts only a newer feed CalVer, while a local
  build may install the latest published release. The feed advances only from an
  immutable normal GitHub release and explicitly rejects drafts, prereleases, and
  a feed without that stable-release classification. The runtime build ID remains
  the exact immutable payload and matching remote-asset identity, upstream Cargo
  SemVer remains plugin/provenance compatibility metadata, and the wire protocol
  remains the client/server compatibility gate.
- A copy installed by WinGet updates only through
  `winget upgrade hdosys.herdr-win`; `herdr update`
  refuses to replace package-managed bytes. A newer GitHub release does not create
  an update badge or release-note action for that copy until the official WinGet
  source exposes the exact target release version; then user-facing update actions
  show the WinGet command. Running direct setup over the exact current managed root
  preserves existing WinGet ownership rather than creating competing update paths.
  Uninstall removes that ownership with the managed program.
- Uninstall first asks every running managed session to stop through Herdr's
  graceful server API under one bounded deadline. If any session remains
  reachable, uninstall preserves the managed installation and reports the
  required action. After sessions stop, it removes the managed program, only
  literal spellings of its own user `PATH` path, **Herdr Extended** Installed Apps
  registration, and installer-known `SKILL.md` copies at its managed universal
  and Claude locations. Its single skill-removal checkbox covers both locations and
  starts selected only when every existing copy is installer-known or absent; any
  unknown copy leaves it clear. Selecting it explicitly authorizes removal of the
  exact `SKILL.md` files even when customized. Silent uninstall removes known copies
  automatically, preserves unknown copies by default, and accepts `/REMOVE_SKILL`
  as explicit authorization. Other PATH entries and skill-directory content are
  preserved, so a previously shadowed upstream/native `herdr` becomes visible to
  new processes again. A `herdr` skill directory is removed only immediately after
  its authorized `SKILL.md` removal proves it empty. The separate interactive
  settings checkbox preserves configuration and session data under
  `%USERPROFILE%\.herdr` by default; silent uninstall accepts `/REMOVE_SETTINGS` as
  the explicit settings-deletion choice. If locked or unsafe content prevents that
  selected cleanup or a selected installer-managed skill removal from finishing,
  uninstall preserves and reports the residual while still removing the managed
  program, Installed Apps registration, and its installer-owned `PATH` entry.
- The executable and setup are currently unsigned. Documentation must keep the
  SmartScreen warning and digest-verification path clear until signing becomes an
  explicit release capability.

## Interaction and Status

- Keyboard-first terminal operation remains complete end to end.
- The unpublished development candidate accepts one OSC 8 HTTP(S) link around a
  command-produced tab-row status label. Plain-text producers remain unchanged.
  Clicking the underlined label or selecting it through prefix-F10, Up/Down, and
  Enter opens the attached client's browser, never the remote server's browser.
  Refresh never activates a link. Producers must not put credentials in URLs;
  unsupported schemes and credential-bearing authorities are not clickable.
  Both endpoints need this feature for activation; older compatible endpoints
  retain the plain label. The source configuration guide owns the byte contract.
- Background Agent completion alerts follow Herdr's enabled default. Settings >
  completion and `ui.notify_on_agent_completion = false` expose one immediate,
  persistent opt-out for completion popups and done sounds without suppressing
  questions, permission prompts, or errors.
- `[session] auto_start_agent` is an opt-in new-tab convenience that starts one
  supported interactive Agent in the root pane of every genuinely new tab through
  Herdr's existing managed launch path. It is unset by default and applies to the
  initial tab of a new persistent session plus later tabs created through the UI,
  CLI, or API. When live reload enables or changes the setting, Herdr also starts
  the selected Agent in each eligible existing tab root that is waiting at its
  shell. Restore or reattach at startup, an unchanged reload, live handoff,
  split panes, and restored tabs do not trigger it. A genuinely new default workspace
  replacing the last closed workspace uses the same opt-in setting. Exiting or interrupting the Agent
  returns to the tab's underlying shell, and Herdr never writes the setting.
- Managed Agent launch stays in the selected interactive shell and preserves
  exact arguments. Initial start, auto-start, and native session resume share
  the same selected-shell renderer. PowerShell, cmd, Nushell, and Git Bash each
  use their native command syntax on Windows; unknown Windows shells fail
  closed. Linux and macOS use shell-native argument quoting, including explicit
  external-command invocation for Nushell.
- The development candidate binds `prefix+o` to immediate managed preferred-agent start
  in the selected existing shell pane and its current directory. Client-owned
  `[agent]` selects `kind` (default `opencode`) and an `args` list, independent of
  server keybinding profiles. The selected agent must be installed on the server.
  Arguments are rendered through the existing shell-native launch owner, not
  interpreted as arbitrary shell command text. OpenCode retains its ephemeral
  loopback HTTP flags with extra arguments; conflicting network options are rejected.
  The start refuses managed
  agents, non-shell processes, and shells with running children. No shell profile,
  alias, new pane, or separate server supervisor is required. `keys.start_agent`
  is configurable through the existing client/remote keybinding machinery;
  client-local bindings apply to the selected remote pane. The notification-target
  default moves to `prefix+shift+o`. Explicit user overrides remain authoritative.
  Selecting a preferred client agent does not enable or change server-side new-tab
  auto-start, which remains separately configured through `session.auto_start_agent`.
- OpenCode roots started through Herdr's managed Agent path expose one ephemeral
  loopback attach endpoint so direct child sessions can open in their own panes.
  Those panes remain bound to the root session selected locally in that pane;
  server-global creation events from another attached client cannot take over that
  selection, while child prompts still report blocked and working state through the
  root authority. A bare `opencode` process has no attach endpoint and is not a
  child-pane root.
- A still-running full-lifecycle Agent that was temporarily superseded by another
  recognized foreground Agent regains hook authority when it returns. A real
  process exit or explicit hook clear still prevents the stale session from
  reclaiming authority.
- One pane or workspace metadata report atomically updates or clears at most 64
  custom tokens, and each pane or workspace retains at most 64. Existing key,
  value, source, sequence, and TTL validation remains fail-closed.
- Every supported client can attach to an x86_64 or ARM64 Windows SSH host. Herdr
  first uses an endpoint-compatible `herdr.exe` from that SSH
  user's `PATH` or the stable per-user remote runtime at
  `%USERPROFILE%\.herdr\remote\herdr.exe`. Version differences alone do not require
  replacing a compatible server. Saved-machine reconnect never installs, stops,
  replaces, or starts a missing server. If neither candidate is compatible, an
  interactive attach offers to transfer the complete digest-verified Windows
  portable payload into that user's profile without running the managed installer
  or changing `PATH`; `--yes` explicitly approves that installation and any required
  server restart for one normal attach, while an ordinary non-interactive attach
  without it never modifies the host. Interactive progress reports connection and
  candidate inspection, preparation, the one payload transfer, staging validation,
  any approved server stop, activation, deployed-client verification, and opening.
  The first Windows probe owns candidate and server inspection for the whole attach;
  an update never repeats those remote queries. Herdr validates remote configuration
  and the complete staged payload before an approved stop. One bounded remote
  operation then stops the server when required, confirms shutdown, requires the
  exclusive runtime lease, removes the old stable payload, promotes staging, and
  verifies the deployed binary, version, and protocol. There is no rollback, legacy
  discovery, migration, compatibility layout, or parallel runtime version.
  The Windows host's OpenSSH default shell must be `cmd.exe` or PowerShell 7
  (`pwsh.exe`) so the interactive binary stream remains byte-exact. Herdr rejects
  Windows PowerShell 5.1 and unrecognized default shells with a corrective action
  instead of opening a buffered or corrupted terminal.
  Persistent server launch targets exactly one active desktop session owned by the
  SSH user and verifies the launched process session. It fails closed when that
  session is absent or ambiguous and leaves no persistent scheduled task. The
  server releases its transient interactive console before serving, and the server
  and pane processes remain independent of the initiating SSH channel.
- `herdr --remote <target> --provision` is the explicit configuration-management
  path. Unattended use requires `--yes` and may add `--json`. It deploys a matching
  runtime, rejects invalid remote configuration before server activation, starts a
  missing server, reloads configuration when the running binary already matches,
  and saves then stops, replaces, and restarts only when binary activation requires
  it. A non-package-managed local Windows x86_64 build can provision itself to
  x86_64 or ARM64 Windows through Windows emulation only when its executable is
  adjacent to the complete portable runtime. Herdr projects that runtime into the
  same validated ZIP layout used by published builds. Windows remote hosts do not
  support live handoff.
- User-visible state distinguishes waiting, active, mixed, complete, failed,
  cancelled, stopped, and no-op outcomes whenever they require different user
  understanding or action.
- Status inspection is observational: viewing status never starts, retries, or
  changes work.
- Windows remote downloads and polling-peer connections preserve forward progress
  without extending stall timeouts. Matching Tab/Escape reports and nested mouse
  input remain usable without weakening input validation.
- Worktree collapse preferences belong to their machine, and creating a worktree
  focuses it only in the requesting client. Navigate can preview workspaces across
  connected machines while refusing actions on stale selections. Explicit public
  Agent focus reaches attached clients; activating a background view preserves the
  focused terminal's geometry.
- Session deletion requires the exact recorded name and refuses a live target.
  A Local startup failure remains visible when saved remote machines are present,
  without interrupting healthy remote views or changing restored selection.

## Release Promise

- Published snapshot assets are the exact retained outputs of one successful manual
  candidate build of the selected upstream source plus the maintained queue.
  Promotion never rebuilds or repackages them. The portable ZIP, managed installer,
  and manifest digest identify the same source.
- Each snapshot includes the managed Windows x86_64 setup and portable ZIP plus raw
  Linux and macOS executables for amd64 and arm64, all built from the same retained
  candidate. Every asset is a supported client or remote endpoint on the matching
  wire protocol; only Windows currently has the managed installer and update
  lifecycle.
- Each release has a manually selected Herdr Extended CalVer `YYYY.MM.DD.N` and is based
  on the exact latest upstream stable release selected during the most recent
  explicit refresh. Current assets use `herdr-ext_v<CalVer>_<os>_<arch>.<ext>`
  and `_setup.exe`. The package ID remains `hdosys.herdr-win`; existing WinGet
  installations upgrade through that ID when the new manifest is in the catalog.
  Direct clients that reject the renamed assets use the current installer to
  update the same managed installation in place without uninstalling it. There
  is no waiting period or duplicate release asset. The GitHub release title,
  notes, and installer metadata visibly pair that CalVer with `Herdr
  v<upstream-version>`. Source/control hashes remain exact provenance.
- Candidate builds and release promotion are separate manual operations. A build
  requires the intended CalVer, uses the reviewed stable commit recorded in `BASE`,
  and retains its candidate artifacts for 14 days without publishing a release.
  Promotion requires that successful build's workflow run ID and publishes only its
  validated artifacts. An explicit manual refresh separately selects the latest
  non-draft, non-prerelease upstream release and replays the complete queue; no
  scheduled workflow queries, rebases, builds, or publishes current upstream.
- Ordinary pushes do not publish binaries. A replay, build, package, immutability,
  digest, or feed-verification failure prevents or visibly fails the corresponding
  release stage rather than silently publishing a different build.
