# AGENT_IMPROVEMENTS.md

Evidence-backed proposals for making future Herdr Extended agent work faster, cheaper,
safer, or more reliable.

This is not product backlog or task history. Product work belongs in `BACKLOG.md`;
user-visible behavior in `PRODUCT.md`; repository-specific technical design in
`ARCHITECTURE.md`; accepted Herdr Extended-specific agent rules in `AGENTS.md` or
procedure in `CONTRIBUTING.md`; cross-project workflow in the global OpenCode
configuration repository.

## Rules

- Add only concrete proposals likely to help future work.
- Keep entries short and evidence-based.
- Merge duplicates instead of appending repeats.
- Do not include secrets, credentials, private data, transient process IDs, logs,
  transcripts, generated evidence, or product feature requests.
- Status values: proposed, accepted, blocked, declined, done.

## Proposals

- **Status: done. Scope Git trust inside the delta-worktree helper.** Every helper
  Git command now trusts only the resolved control checkout and selected worktree
  through command-scoped `safe.directory` values. Evidence: materialization had
  failed on a Sandbox-owned linked checkout until the same bounded values were
  supplied manually; the focused command-construction test now owns that contract.
  Owner: `scripts/delta_workflow.py`, its focused tests, and `CONTRIBUTING.md`.

- **Status: done. Compile each mailbox prefix during stable-base refreshes.** The
  refresh-only `compile-prefixes` command replays and compiles every ordered prefix
  with all detected logical processors, disabled incremental state, and one shared
  temporary Cargo target.
  Evidence: the v0.8.2 review found cross-mailbox owner leaks that a complete-queue
  compile hid. Moving the leaked deferred auto-start call from 0003 to its 0009
  owner made the queue compile prefix by prefix; after appending the metadata owner,
  all ten prefixes passed in 416.595 seconds. Disabling incremental output also
  removed a measured 5.4 GiB disposable-cache failure. Owner:
  `scripts/delta_workflow.py`, its focused tests, and the refresh procedure in
  `CONTRIBUTING.md`.

- **Status: done. Use one validated local installer input bundle and artifact
  entrypoint.** `scripts/local_windows_installer.py` now records exact bundle
  hashes and executable identity below ignored `target/`, then delegates repeated
  builds to the materialized source packager without Cargo or 7-Zip. Evidence: a
  cached bundle was revalidated in 1.098 seconds and produced the next atomically
  replaced setup in 23.840 seconds. Owner: the script, its focused tests, and the
  Candidate procedure in `CONTRIBUTING.md`.

- **Status: done. Use one all-core source-candidate installer command.**
  `scripts/local_windows_installer.py candidate` now derives local identity before
  compilation, overrides inherited single-job limits with every logical processor
  available to the current process, builds only the three packaged binaries, and
  always finishes at the validated installer. Evidence: the same incremental Herdr
  rebuild fell from more than 20 minutes to 99.284 seconds, followed by a 22.122
  second installer build and 125.719 seconds total. Owner: the script, its focused
  tests, and the candidate procedure in `CONTRIBUTING.md`.

- **Status: done. Bound incremental reuse for local Candidate release builds.**
  Candidate enables optimized incremental reuse in its existing target, keeps 16
  release codegen units and all available build jobs, and leaves public releases
  unchanged. Evidence: an identity-changing warm build completed in 68.325 seconds
  after a 186.010-second seed; the earlier non-incremental measurement took 207.310
  seconds. Different cache warmness prevents treating that comparison as a universal
  speedup. The warm incremental cache occupied 953,296,663 bytes. A prior run failed
  on a full target volume, so Candidate now requires 2 GiB free and refuses a cache
  already above its 2 GiB iteration budget before compiling. Cleanup is explicit
  and requires quiescent ownership; no automatic pruning or fallback was added.
  The actual canonical Candidate subsequently completed in 131.332 seconds,
  including 72.841 seconds compiling, 8.561 seconds for its native launch probe,
  and 35.198 seconds packaging. Empty compiler lock files are accepted by the cache
  budget owner rather than mistaken for invalid installer payloads.
  Owner: `scripts/local_windows_installer.py`, its focused test, and
  `CONTRIBUTING.md`.

- **Status: declined. Reuse validated binaries for package-excluded candidate
  changes.** The current candidate identity intentionally changes for every tracked
  diff and untracked source input, and that identity owns the managed runtime path.
  A package-exclusion classifier would introduce a second source truth and permit a
  runtime to claim provenance from a snapshot it did not compile. The 140.782-second
  rebuild did not justify weakening that fail-closed contract; normal-test retiering
  removes the demonstrated serial test compile without changing identity ownership.

- **Status: done. Make one development stack the only local user installer
  owner.** Product sessions default to the shared `candidate/development`
  worktree. Topic worktrees exist only for concrete internal isolation, default to
  build-ID-isolated outputs, and must be integrated and removed by their creator.
  Only the exact development branch writes the fixed setup, so every reported
  installer is the cumulative current state. User acceptance promotes that complete
  tree rather than individual topics. Owner:
  `scripts/local_windows_installer.py`, its focused tests, `AGENTS.md`, and the
  development procedure in `CONTRIBUTING.md`.

- **Status: done. Share the release-profile Cargo cache for release-boundary
  checks.** `local_windows_installer.py candidate --release-test-filter` runs one
  exact release-profile test with the packaged binaries' target, build identity,
  and detected logical-processor job count. Evidence: the implemented path passed
  its focused release test in 186.361 seconds, then reused that target for the
  packaged-binary build in 0.826 seconds. Owner: the script, its focused tests, and
  the Candidate procedure in `CONTRIBUTING.md`.

- **Status: done. Retier optimization-insensitive unit tests out of the local
  installer release-build path.** `--test-filter` now uses the existing normal
  `just test-one` gate, while explicit `--release-test-filter` retains the optimized
  boundary path. Evidence: the real cumulative candidate ran one exact normal test
  in 65.849 seconds, reused its validated release binaries in 0.982 seconds, and
  produced the setup in 27.243 seconds with 16 jobs. Owner:
  `scripts/local_windows_installer.py`, its focused tests, and the Candidate
  procedure in `CONTRIBUTING.md`.

- **Status: done. Make `just test-one` select Windows-valid Rust targets.**
  The Windows recipe now runs through native PowerShell and constrains nextest to
  the `herdr` binary while the existing non-Windows path remains unchanged.
  Evidence: the focused AutoStart filter passed all three tests and the remote
  server detach filter passed its native Windows test through the shared recipe.
  Owner: `justfile`.

- **Status: done. Retier public preview-manifest convergence to its observed cache
  window.** The real fixed updater URL now receives 60 bounded attempts, reports the
  successful convergence attempt, and identifies a terminal miss as delayed
  post-publication visibility rather than a failed publication. `CONTRIBUTING.md`
  routes that exact failure through `gh run rerun <promotion-run-id> --failed` and
  forbids another promotion or CalVer. Evidence: all former 18 ten-second attempts
  expired for immutable release `2026.08.31.4`; the same URL exposed the correct
  build later, and the isolated updater returned `already up to date
  (2026.08.31.4)`. Owner: `.github/workflows/release.yml` and `CONTRIBUTING.md`.

- **Status: done. Document one clean sparse checkout for WinGet PR updates.** The
  contribution procedure now starts sparse mode in the initial single-branch clone,
  selects only the package cone, and requires a clean status before edits. Evidence:
  the exact path produced the clean PR head in 84.050 seconds and selected the cone
  in 3.143 seconds, replacing a full checkout that exceeded 600 seconds. Owner:
  `CONTRIBUTING.md`.

- **Status: done. Make Windows worktree removal terminal before dropping its
  recovery metadata.** The Windows lifecycle now waits for the pane process and
  ConPTY master to exit, and fails before Git mutation if bounded shutdown does not
  complete. Evidence: a real Windows Terminal client created and removed a
  non-forced worktree in 5.460 seconds, leaving neither its directory nor Git
  registration; cumulative installer build `9eb521456ac0.1f6cc92d2972` validated
  and packaged. Owner: the Windows `herdr worktree remove` lifecycle.

- **Status: done. Reuse an exact current installer bundle before compiling a
  build-only candidate.** `candidate` now derives source identity first and, when
  no focused test is requested, validates and packages an existing exact bundle
  through the shared `build` path before creating a Cargo target. Evidence: the
  real cumulative candidate reused build `9eb521456ac0.6747139eceac` and produced
  the validated setup in 29.415 seconds, including 27.094 seconds of packaging,
  instead of repeating 331.237 seconds of test and release compilation. Owner:
  `scripts/local_windows_installer.py`, its focused tests, and the Candidate
  procedure in `CONTRIBUTING.md`.

- **Status: done. Retier non-Rust focused checks before candidate
  packaging.** Let the Candidate procedure run an exact repository-owned Bun or
  Python behavior check in the same stop-on-failure command chain, then invoke
  `candidate` without a Rust `--test-filter`; reserve that option for Rust-owned
  behavior. Evidence: the OpenCode asset suite passed 36 tests in 0.434 seconds,
  while the exact Rust embedding filter consumed 118.444 seconds before a separate
  134.220-second release compile, making the validated candidate take 281.669
  seconds. Earlier, the nearest Rust embedding filter spent 38.361 seconds before
  failing on an unrelated existing Copilot assertion. This removes false blockers
  and one unnecessary test compile without adding a generic command runner. The
  procedure now runs the exact Bun or Python owner first and invokes Candidate
  without a Rust filter in the same stop-on-failure sequence. Owner: the Candidate
  procedure in `CONTRIBUTING.md`.

- **Status: done. Add one native Windows interactive-server launch probe.**
  Exercise the built product from an actual `cmd.exe` parent through the existing
  Task Scheduler launch owner so Windows hidden `=X:` drive state is present, then
  assert bootstrap consumption, exact session and user ownership, process cleanup,
  and zero task residue. Evidence: two installers passed probes whose parent lacked
  the real `cmd.exe` environment and then failed the configured SSH path. The exact
  `cmd.exe` to Task Scheduler launch passed in 3.253 seconds after the root fix. A
  reusable gate would expose the cross-session failure before installer handoff
  without reconstructing one-off test code. Candidate now validates the built bundle,
  places one real `cmd.exe` parent in a kill-on-close job, and requires bootstrap
  consumption, exact user and session ownership, process shutdown, and zero task
  residue before packaging. The Windows PowerShell 5.1 path passed in 3.883 seconds.
  Owner: `scripts/windows_interactive_server_launch_probe.ps1`, its native job helper,
  and the Candidate procedure in `CONTRIBUTING.md`.

- **Status: done. Propagate Windows `test-one` failures.** Make the Windows
  PowerShell recipe return Cargo Nextest's exit status to Just. Evidence: an
  intentionally failing focused test printed Nextest's failure summary, but
  `just test-one interactive_server` returned status 0; invoking the same Cargo
  Nextest path directly returned the failure correctly. The recipe now returns
  `$LASTEXITCODE`; direct and wrapped no-match runs both returned status 4. This
  prevents false-green focused gates. Owner: `justfile`.

- **Status: done. Gate changed integration assets on a higher migration version
  advance.** Extend the existing delta or Candidate source check to compare each
  changed managed integration asset with the accepted queue baseline, then require
  a higher embedded marker and matching Rust constant before packaging. Evidence:
  three OpenCode layout commits changed the managed plugin while both old and new
  bytes still claimed version 12, so the installed stale plugin was reported as
  current. Version 13 immediately made the real installed copy detectable and
  replaceable. Candidate now compares embedded assets with the accepted replay tree,
  requires a strictly higher marker, and checks the matching Rust constant before
  bundle reuse or compilation. This preserves multiple cumulative Candidate
  generations before patch finalization; the former exact-one rule rejected valid
  OpenCode v17 after v16 was already cumulative while the accepted queue remained at
  v15. Owner:
  `scripts/delta_workflow.py`, `scripts/local_windows_installer.py`, and their focused
  tests, without a second integration-version registry.

- **Status: done. Share the Candidate normal-test Cargo target with the
  pre-push focused Rust gate.** Expose one repository-owned command that runs the
  required pre-push `just test-one` boundary against Candidate's stable Cargo
  target, so Candidate can rerun the same test after push without recompiling its
  binary. Evidence: the pre-push four-test gate compiled in 30.430 seconds, then
  Candidate compiled the same Windows test binary in a separate target for 46.750
  seconds before its 6.804-second test. A later eight-test gate reused its cache and
  finished in 7.401 seconds, while Candidate recompiled the same normal-profile
  binary for 63 seconds and spent 71.711 seconds on that boundary. The shared
  `test-one` owner now clears build-identity variables and supplies Candidate's
  stable target to the existing Just recipe. The first exact Windows test compiled
  in 123.190 seconds; the final unchanged integrated rerun completed in 3.936
  seconds, with Cargo itself taking 0.91 seconds. Owner:
  `scripts/local_windows_installer.py`, the
  existing `justfile` recipe, and the Candidate procedure in `CONTRIBUTING.md`.

- **Status: blocked. Exercise restored OpenCode roots in the native split
  probe.** Start the existing adaptive-split acceptance from a persisted OpenCode
  session after a Herdr restart, then create several direct children concurrently
  and assert the native panes. Evidence: the fresh managed-launch contract and all
  32 sequential asset tests first passed while a restored process exposed no
  attachable endpoint. After that fix, seven simultaneous child events all read the
  pre-split layout and repeatedly divided the Main pane because no prior child pane
  ID had been recorded. A later user-run four-child probe showed only two visible
  panes, but `herdr integration status` proved that the running OpenCode process had
  loaded integration v15 while source owned v16. OpenCode plugins are startup-loaded,
  so that run could not test the current implementation. The exact check requires a
  current integration status, reinstalling any outdated integration, fully restarting
  OpenCode, then using provider-authenticated concurrent direct-child creation. The
  repository has no credential-free native child trigger, and a synthetic event
  driver would duplicate the Bun harness without proving restore or native pane
  placement. Resume only in a user-authorized provider session; the existing Bun suite
  remains the deterministic split-logic owner. Owner: the OpenCode native Candidate
  acceptance boundary, reusing its current split probe rather than a second harness.

- **Status: done. Fail local Candidate builds before Cargo when Windows
  commit headroom is exhausted.** Read the Windows committed-bytes and commit-limit
  counters before starting the nested all-core Cargo and Zig build. When commit
  charge is already at a measured unsafe threshold, stop with the current headroom
  and aggregate largest-process classes instead of launching the compiler; never
  terminate processes automatically. Evidence: four attempts spent about 176
  seconds failing across unrelated Zig 0.15.2 units and its readable standard
  library while the Sandbox was at 35.71/38.65 GiB committed with a full pagefile.
  Closing only user-authorized idle workspaces lowered charge to 18.85/36 GiB, and
  the same 16-job release build then succeeded in 114.792 seconds. A bounded native
  Windows helper now reads `GetPerformanceInfo`, reports the five largest
  private-memory process classes, and blocks at or below the measured unsafe 3 GiB
  headroom without terminating anything. The final real snapshot completed in 0.963
  seconds with 14.06 GiB headroom; a forced-threshold run exercised the complete
  rejection diagnostic in 1.060 seconds without starting Cargo or terminating a
  process. Owner: `scripts/local_windows_installer.py`,
  `scripts/windows_commit_headroom.ps1`, focused preflight tests, and the Candidate
  procedure in `CONTRIBUTING.md`.

- **Status: done. Expose validated packaging identity inputs through the
  canonical local installer owner.** Let `local_windows_installer.py` pass an
  explicit product name to the existing packager and record that input in its
  validated build invocation, so changing only packaging presentation can reuse an
  exact current runtime bundle without classifying source files or changing runtime
  provenance. Evidence: the restored packager parameter produced a real custom-name
  NSIS installer from the current bundle in 25.642 seconds, while committing that
  packaging-only correction changed the source fingerprint and forced a 143.767-second
  Cargo rebuild plus 194.754 seconds total to replace the canonical installer. This
  is an explicit build input, not the previously declined source-exclusion
  classifier. `build`, `candidate`, and `release-precheck` now forward an explicit
  `--product-name`; omission leaves the default with the materialized packager. A
  real custom-name installer reused the current bundle and validated in 26.248
  seconds without Cargo. Owner: `scripts/local_windows_installer.py`,
  `scripts/package_windows_installer.ps1`, focused identity tests, and the Candidate
  procedure in `CONTRIBUTING.md`.

- **Status: done. Generate one issue-reference triage report.** Extract
  qualified issue keys, subject, current source owner, and final reachable commit
  from the maintained mailboxes and release history, then join each key with a
  captured issue title and one concise behavior summary. Evidence: four
  number-only Windows partitions searched local source, the ledger, and 3,278
  commit objects; one spent 119.4 seconds and still returned uncertain entries.
  The later all-platform reconciliation repeated history and GitHub searches and
  found 13 exact mappings missing from the private ledger. Expected benefit:
  replace broad pickaxe and duplicate-history review with one deterministic,
  read-only inventory before upstream triage. Owner: a repository report command
  beside the delta workflow and the upstream issue triage procedure.
  Implemented `delta_workflow.py issue-report`: deterministic local mailbox JSON,
  qualified references, touched paths, source commit and immutable mailbox hash.
  An explicit ledger input joins only captured title/local outcome fields; missing
  fields stay unknown. No history scan, upstream query, or maintained report index.
  Mailbox source commits are reported without inventing reachability or release
  status. Focused fixtures cover deduplication, missing references, and private
  draft exclusion. The real read-only inventory covered 13 mailboxes in 1.302
  seconds; it does not replace authorized verification of upstream or release state.

- **Status: done. Give Candidate one exact vendored `portable-pty` unit-test
  path.** `--portable-pty-test-filter` now runs the library manifest with every
  logical processor, Candidate's shared temporary target, a five-minute process
  bound, exact one-test acceptance, and task-owned lock cleanup, then lets
  `candidate` rerun the same selector after push. Evidence: all 27 focused runner
  tests passed in 0.062 seconds; the real malformed-environment regression passed
  through the new owner in 15.170 seconds and left no package lock. This replaces
  the unrelated full-PTY path that stalled for 600 and 180 seconds. Owner:
  `scripts/local_windows_installer.py`, its focused tests, and the Candidate
  procedure in `CONTRIBUTING.md`.

- **Status: done. Finalize interdependent mailboxes as one verified queue
  replacement.** `delta_workflow.py refresh` accepts a reviewed linear logical
  stack, stages binary-safe mailboxes privately, preserves retained metadata, and
  proves the complete accepted tree before replacing the queue and stable base.
  Evidence: the merged v0.9.0 source could not use the single-owner linear WIP
  finalizer. The new owner produced 13 independently compiled mailboxes with exact
  replay equality while preserving published development ancestry. The existing
  single-owner finalizer remains the narrow ordinary-edit path. Owner:
  `scripts/delta_workflow.py`, its focused tests, and `CONTRIBUTING.md`.

- **Status: done. Integrate the linked product target without weakening the main
  checkout guard.** The repository's exact-base `integrate-development` operation
  reuses source-checkout validation and runs under the same resource key as
  development publication. Evidence: the generic helper correctly rejected the
  linked `candidate/development` checkout; the repository operation integrated the
  reviewed source in 5.980 seconds. Its focused check rejects stale bases and dirty
  source before fast-forwarding. Owner: `scripts/delta_workflow.py` and
  `CONTRIBUTING.md`. No global helper change or second lock implementation.

- **Status: done. Remove remote publication from the local artifact prerequisite.**
  Candidate now requires clean integrated development source and complete topic
  ancestry, not equality with the remote tip. Evidence: the completed v0.9.0 port
  was rejected before Cargo solely because it was not pushed, contradicting the
  artifact-first procedure. The focused gate now accepts the local milestone without
  network activity; publication remains required afterward. Owner:
  `scripts/local_windows_installer.py`, its focused test, and `CONTRIBUTING.md`.

- **Status: done. Reuse one caller-owned prefix cache during a stable-refresh
  correction cycle.** The mandatory 13-prefix pass took 718.804 seconds, including
  repeated single-crate checks around 40 seconds; earlier failed decomposition
  attempts also repeated cold Zig setup. Allow the existing prefix command to reuse
  one task-owned cache across corrections, while retaining all-core compilation,
  exact prefix provenance, and terminal cleanup. Expected benefit: avoid cold native
  dependency work without weakening prefix independence or adding persistent queue
  state. Owner: `scripts/delta_workflow.py` and its refresh procedure. Its Windows
  extended-path cleanup already removed the demonstrated over-260-character Zig
  residuals successfully. `compile-prefixes --work-dir` now retains the source
  checkout and Cargo target across corrections, including compile failures.
  Git owns repository/base identity; reuse rejects foreign directories, changed
  source, and unfinished replay. Every prefix still runs with its exact tree ID.
  Synthetic two-prefix checks proved persistent native and Cargo cache paths across
  two successful invocations and an intervening compile failure. Native build time
  savings are not yet measured; no actual source compilation was needed for this
  workflow-only acceptance. Explicit caller cleanup ends the one-refresh lifecycle.

- **Status: done. Document bounded recovery of demonstrably incomplete native
  caches.** The Candidate procedure distinguishes missing tracked source, memory
  pressure, disk exhaustion, and proven cache damage. It preserves the suspect
  cache on the same filesystem, permits one unchanged-source rebuild using a fresh
  cache, and requires ownership proof before cleanup. Evidence: empty Zig dependency
  directories and stale references were resolved without source/version changes;
  later measurements independently exposed disk exhaustion. This replaces serial
  cache surgery, not failure diagnosis. Owner: `CONTRIBUTING.md`; no automatic retry
  or fallback.

- **Status: done. Bound upstream remote-regression evidence retrieval.**
  `CONTRIBUTING.md` now gives short examples for issue/PR state, comment selection
  and exact owner-file diffs, reusing the existing local report. It preserves useful
  bot evidence, requires exact source when patches are truncated, and distinguishes
  symptoms, demonstrated causes, fork exposure and missing runtime evidence.
  Evidence: unfiltered issue JSON and broad fork diffs had produced truncated output
  dominated by keyboard traces, while the Windows SSH diagnosis depended on a small
  transport path. This reduces duplicate retrieval without another script, index,
  gate, or automatic upstream action. Owner: `CONTRIBUTING.md`.

- **Status: done. Accept validated source-writer branches for test-only runs.**
  `local_windows_installer.py test-one` accepts a named source branch after the same
  Git-root, common-repository, path, and required-source checks. Artifact commands
  retain their existing branch restrictions. Evidence: the stabilization writer
  `agent/v090-stabilization` was rejected by a naming-prefix check, forcing callers
  to reconstruct the existing shared-target test invocation manually. Two focused
  assertions protect the test-only permission and unchanged packaging boundary.
  Owner: `scripts/local_windows_installer.py` and its tests; no new runner or cache.

- **Status: done. Remove the duplicate local installer release gate.** The complete
  installation/uninstallation/fault matrix remains on the fresh GitHub Windows
  release runner, not as a prerequisite in the active development Sandbox. Evidence:
  the procedure required a local full matrix in addition to the workflow's existing
  release-artifact matrix, despite shared HKCU, PATH, and activation-state risks.
  Local package validation and isolated probes remain; existing manual installation
  evidence is reusable. The local diagnostic entrypoint is retained only for explicit,
  isolated investigations and its help states the mutation boundary. This removes
  one duplicate mandatory gate without introducing another runner or test suite.
  Owner: `CONTRIBUTING.md`, `ARCHITECTURE.md`, and the diagnostic CLI help.

- **Status: proposed. Resolve the provisioned MSVC inspection tools in the canonical
  local installer owner.** A clean candidate build failed before packaging because
  `dumpbin.exe` was installed under the Sandbox Visual Studio toolchain but absent
  from the caller's `PATH`; prepending its existing `Hostx64\x64` directory made the
  unchanged build succeed. Let `scripts/local_windows_installer.py` derive the
  provisioned Visual Studio environment through the repository's existing toolchain
  owner before invoking package validation, while preserving a terminal error when
  no supported installation exists. Expected benefit: remove a repeated manual shell
  prerequisite without adding a fallback packager or weakening binary validation.
  Owner: `scripts/local_windows_installer.py` and its focused packaging-tool discovery
  checks.
