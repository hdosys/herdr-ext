# Contributing to Herdr Extended

Thanks for helping keep Herdr useful on Windows.

Herdr Extended is an upstream-first distribution, not an independent product fork.
Changes should either reduce the Windows delta, keep it replayable, or improve
the small control plane that validates and publishes it.

Read `PRODUCT.md` for stable user-visible fork behavior and `ARCHITECTURE.md` for
stable technical/source boundaries. This file owns the procedure for changing
those owners and their implementation; it does not duplicate their decisions.

## Choose the right owner

Before editing, classify the change:

- **Upstream Herdr behavior:** propose it to
  [`herdrdev/herdr`](https://github.com/herdrdev/herdr) under that
  project's contribution policy. Do not use the fork to bypass upstream review.
- **Maintained Windows behavior:** implement and validate it on a topic branch or
  `candidate/development`. Update the owning logical mailbox in `patches/delta/`
  only after the current user explicitly authorizes a patch update or release.
- **Fork control plane:** edit repository branding, contributor and delta-workflow
  automation, patch inventory tests, or the two workflows directly in this
  repository.
- **Frozen patch archive:** do not refresh or rename files in
  `patches/upstream/`; existing links must remain valid.
- **Stable user-visible fork behavior:** update `PRODUCT.md` and the mirrored
  public README projection. Update an applicable logical mailbox only under the
  explicit patch authorization below.
- **Stable technical design:** update `ARCHITECTURE.md` and the owning code/tests.
  Update a mailbox only under the explicit patch authorization below.
- **Selected future product work:** use `BACKLOG.md` only after the current user
  chooses the outcome for later implementation. It never owns findings,
  verification assignments, evidence, or test reminders.
- **Repository-specific process improvements:** use `AGENT_IMPROVEMENTS.md` while
  proposed, then move accepted procedure to this file or the owning automation.

Open an issue only when a substantial change still needs product or architecture
scope alignment before implementation. A bounded change whose scope is already
accepted does not wait on issue ceremony. A useful bug report includes the
Herdr Extended release tag, Windows version, terminal, shell, exact reproduction,
current behavior, and expected behavior.

When offering Herdr Extended builds in an upstream discussion, link to the
[current releases page](https://github.com/hdosys/herdr-ext/releases) rather than
to a version-specific release so the guidance remains current.

## Developing the maintained delta

The patch queue is the release representation, not the day-to-day editing surface.
Do not make a product-source edit only in this repository's control checkout. The
development build starts from recorded `BASE`, so every finished product
change selected for a release must eventually be represented by the canonical
queue.

Patch promotion is a hard user-authorization boundary. Ordinary development,
candidate building, installer acceptance, clean-slate work, and completion of a
topic or development commit never authorize patch generation or a write under
`patches/`. Before invoking `delta_workflow.py finalize`, `git format-patch`, or
any equivalent patch-generation path, the current user must have explicitly
requested an update, regeneration, or finalization of the maintained patches, or
creation or publication of a release. If the current request does not already
name that outcome, stop and ask the user explicitly before the first such action.
Do not carry authorization from an earlier task. Until authorized, commit and push
only the topic branch or `candidate/development`; leave `patches/delta/`, its
`series`, and `BASE` byte-identical.

Maintained product-source work uses one long-lived shared worktree on
`candidate/development`. Its local branch and remote
`origin/candidate/development` target use the same name and are the repository's
one cumulative development state. Documentation and control-plane changes stay in
the control checkout. Ordinary sessions reopen, coordinate, and work directly in
that development tree. They do not create a worktree per issue.

Integrate every coherent completed topic into `candidate/development`, build the
fixed installer, then run focused checks and push that cumulative branch. An explicit current-user patch or
release request gates patch-queue promotion, not routine integration of completed
development work.

Only when `candidate/development` does not yet exist locally or on `origin`, create
the development tree from the exact local commit recorded in `BASE`, replay the
queue, and publish it:

```powershell
$control = (Get-Location).Path
$base = (Get-Content -LiteralPath patches/delta/BASE -Raw).Trim()
$created = herdr worktree create --cwd $control --branch "candidate/development" --base $base --no-focus --json | ConvertFrom-Json
python scripts/delta_workflow.py materialize --worktree $created.result.worktree.path
python scripts/delta_workflow.py publish-development `
  --worktree $created.result.worktree.path
```

After that first publication, `origin/candidate/development` is the authoritative
recovery source for the cumulative line. Reopen its registered worktree, or in a
fresh checkout create the same local branch from the exact fetched remote tip.
Never reconstruct it from `BASE` while the remote branch exists.

`materialize` validates the checkout, shared repository identity, exact `BASE`,
and clean state before applying `series` once with `git am --3way`. Its Git trust is
scoped to the control checkout and selected worktree. It never queries official
upstream.

Create a topic worktree only for a concrete parallel collision or risky isolation
boundary. Base it on the exact current development commit, not `BASE`. The creating
agent owns integration, remote durability, and complete cleanup. After its focused
check, merge the finished commit into `candidate/development`, publish it through
`delta_workflow.py publish-development`, then remove the topic worktree, local
branch, and any temporary remote ref. Never ask the user to classify or clean
these internal resources.

The cumulative product source is a linked checkout, unlike control `master`.
Use `delta_workflow.py integrate-development --worktree <development-path>
--base <full-current-development-oid> --head <full-coherent-topic-oid>` for its
exact-base fast-forward. It validates the same repository, clean source state,
branch, and ancestry, and performs no build, replay, fetch, or publication.
In concurrent mode, run the entire command through the global `resource-lock`
entrypoint with key `resource:herdr-win-candidate-development`. Use that same key
around `publish-development`; never hold it during builds or tests. The generic
control-branch integration helper intentionally accepts only the main checkout.

The global interactive-delivery order applies. This repository's source-target
exception is that the coherent source commit must first reach
`candidate/development`, because only that branch may build the canonical setup.
Build and report it before behavioral checks and publication. Never report a topic
artifact. Normal publication uses `delta_workflow.py publish-development` and does
not authorize a release.

Both development publication and the fixed installer inspect registered linked
topic worktrees. A committed topic head that is not an ancestor of the development
head blocks the operation. Dirty uncommitted topic state remains in progress and is
not part of the reported completed superset.

Only an explicit current-user request to update, regenerate, or finalize the
maintained patches, or to create or publish a release, authorizes promotion of the
complete reported development tree. A statement that the fixed installer works
does not authorize patch generation. Authorization never promotes an individual
topic. One session owns the complete path:

1. Reinspect shared ownership, collect completed handoffs, and stop overlapping
   writes. Reuse the focused evidence while its source, inputs, and environment
   assumptions remain unchanged.
2. Regenerate every owning mailbox represented in the accepted development range.
   Use isolated linear replay worktrees internally when a mailbox finalizer needs
   one:

   ```powershell
   python scripts/delta_workflow.py finalize `
     --worktree <durable-absolute-worktree-path> `
     --mailbox <series-entry.patch> `
     --expected-tree <tested-tree-id>
   ```

   It requires a clean current WIP branch, preserves its commits, regenerates only
   the named mailbox, keeps later mailboxes byte-identical, and writes the mailbox
   only after a complete candidate replay matches the tested tree. A conflict or
   mismatch leaves the checked-in mailbox unchanged.
   For an explicitly approved new responsibility, append one higher-numbered
   mailbox from a candidate containing one or more linear WIP commits over the
   current queue. The first WIP commit owns the mailbox metadata:

   ```powershell
   python scripts/delta_workflow.py finalize `
     --worktree <durable-absolute-worktree-path> `
     --mailbox <new-series-entry.patch> `
     --expected-tree <tested-tree-id> `
     --new-mailbox
   ```

   This mode derives commit metadata from that WIP commit, renumbers existing
   format-patch subjects, appends the series entry, and restores every delta input
   if exact replay verification fails.
3. Run the inventory tests below. A matching tree transfers the source evidence to
   the checked-in queue, so mailbox regeneration alone does not require another
   product gate.
4. Require the complete checked-in queue to reproduce the exact accepted
   development tree. Review, commit, and push `master`, refresh the development
   baseline, rebuild the cumulative installer, and clean every integrated topic
   worktree, local branch, and temporary remote ref. These are internal mechanics
   and require no further user approval.

Installer acceptance does not authorize publication. Release remains a separate
explicit user request.

There is no scheduled or nightly delta replay. Run replay and mailbox inventory
only during a patch promotion explicitly authorized under the boundary above, an
explicitly assigned release, or a separately requested read-only diagnosis. A
release and any `BASE` refresh remain blocked until the complete accepted
development tree has been promoted into the canonical queue.

Keep the queue small and responsibility-oriented rather than mirroring development
commit history. Never hand-edit a diff to force application; regenerate the owning
mailbox from its reviewed logical commit. A replay conflict or tree mismatch is a
real promotion blocker. Repeated builds, broad gates, and raw review of generated
mailbox churn are not substitutes for source review plus exact tree identity.

An explicitly authorized post-stable backport finalization does not advance `BASE`.
Append one mailbox per coherent correction, combining fixture repairs with their
owning fix rather than preserving incidental development commits. Preserve source
author credit, qualified upstream references, and immutable reviewed source hashes.
Keep existing mailboxes byte-identical except for their required total-count subject
renumbering. Stage the complete candidate queue privately and prove the accepted
source tree before replacing inputs. Every new prefix must compile before control
publication; when the existing prefix command checks the whole queue, run it once
with a disjoint task-owned `--work-dir`. Do not add a verification bypass to skip
the reviewed foundation. Keep backport inventory in `patches/delta/README.md`, mark
local candidates as unpublished, and retire a correction only after an authorized
stable refresh proves equivalent behavior including necessary fork adaptations.

### Fast cumulative Windows development installer

The control checkout owns one thin local entrypoint that reuses the materialized
source packager. It adds no installer implementation. Only the exact
`candidate/development` branch writes
`target/x86_64-pc-windows-msvc/release/herdr-win_local_candidate_setup.exe`, which
is the repository's canonical path named in `AGENTS.md`. It always contains the
current replay plus every completed change integrated into the development branch.
Topic branches keep package output temporary and remove it after their focused
check under the global artifact lifecycle.

Commit the coherent integrated development tree, then build and report the installer
before focused behavior checks and publication. Candidate packaging compares every
changed embedded integration with the accepted queue, requires a higher migration
version, and requires its Rust constant to match. This permits multiple cumulative
Candidate generations before patch finalization:

```powershell
python scripts/local_windows_installer.py candidate `
  --source-worktree <development-worktree>
```

After the handoff, run the selected Rust behavior check once through the shared
Candidate target. Reuse passing evidence while source, inputs, and environment are
unchanged; a Git commit or push alone does not require another run:

```powershell
python scripts/local_windows_installer.py test-one `
  --source-worktree <development-worktree> `
  --test-filter "<one exact test filter>"
```

For a regression owned by the vendored `portable-pty` library, select that owner
instead of routing it through the `herdr` binary:

```powershell
python scripts/local_windows_installer.py test-one `
  --source-worktree <development-worktree> `
  --portable-pty-test-filter "<one exact portable-pty test filter>"
```

Local Candidate compilation enables optimized incremental reuse and explicitly keeps
release's 16 codegen units. Optimization level, native payload validation, exact
build identity, all-core job count, and public release profiles do not change.
Before each optimized compile, Candidate requires 2 GiB of free target-volume space
and refuses an incremental cache already above its 2 GiB iteration budget. The
budget is a preflight bound, not a filesystem quota. Cache data never substitutes
for a build or its acceptance checks. No automatic pruning or non-incremental
fallback occurs. Keep only the current compiler/profile cache; after its users
have stopped, obsolete `release/incremental` contents may be removed under the
normal disposable-cache cleanup procedure. Retain the useful shared target between
ordinary iterations instead of rebuilding dependencies in a new directory.

These commands keep one Sandbox-local Cargo target, remove local build-identity
variables from the normal test profile, and pass the detected logical processor
count to Cargo. The ordinary filter uses `just test-one`; the vendored filter runs
the library manifest with a task-owned temporary lock beside it and removes that
lock after success or failure. No second target or unrelated PTY lifecycle harness
is required. Before every
Cargo test or build phase, the control owner reads the native Windows commit
counters.
At or below 3 GiB of remaining commit headroom it stops before Cargo, reports the
largest private-memory process classes, and never terminates a process. Use
`--release-test-filter` instead only when the selected test's signal depends on
optimization or another release-profile boundary. After
validating the exact input bundle and before packaging, Candidate exercises its built
runtime from `cmd.exe` through the Windows interactive Task Scheduler server-launch
path. The probe places only that command shell in a bounded kill-on-close job, then
verifies bootstrap consumption, user and session ownership, terminal process cleanup,
and zero scheduled-task residue. It replaces the fixed setup only after the selected
test, source identity, native probe, bundle, and package checks pass.

For a Bun- or Python-owned behavior change, build and report Candidate without a
Rust filter, then run the exact repository-owned check. Do not compile the Rust test
binary merely to prove an embedded script already exercised by its own runtime:

```powershell
Push-Location -LiteralPath <development-worktree>
try {
    bun test src/integration/assets/opencode/herdr-agent-state.test.ts
    if ($LASTEXITCODE -ne 0) { throw "OpenCode integration asset check failed" }
} finally {
    Pop-Location
}
```

The optional filters on `candidate` still support an explicitly combined unattended
check/build invocation. They run before packaging and are not the interactive
artifact-first path. For a build-only repeat, omit the filter:

```powershell
python scripts/local_windows_installer.py candidate `
  --source-worktree <development-worktree>
```

The command generates one UTC `YYYY.MM.DD.HHMMZ` freshness label and derives the
exact current build identity from that label, the source fingerprint, and a
per-candidate nonce. An incomplete attempt retains one temporary stamp so a rerun
can reuse the exact validated input bundle before compiling. Successful publication
removes the stamp and superseded bundles. Do not omit focused verification after a
source behavior change; perform the exact source-owned check after the installer
handoff rather than repeating it inside the packaging command.

### Recovering incomplete compiler caches

Distinguish missing source, memory pressure, disk exhaustion, and cache corruption
before another build. Check Git status first. A missing tracked file is not a cache
entry: preserve unexplained deletions and obtain recovery authority rather than
patching around them. Candidate's memory and disk preflights stop before compiler
work; they never terminate processes or delete other sessions' data.

When compiler diagnostics and the cache contents demonstrate empty dependency
directories or stale native references, stop every task-owned compiler using that
exact cache. Preserve the suspect cache on the same filesystem, use a new empty
cache through the existing build owner's path, and perform one bounded rebuild of
unchanged committed source. Remove the preserved cache only after successful
verification and proof that no other user owns it. If the fresh-cache build fails,
retain the evidence and diagnose that failure instead of retrying or repairing
individual dependency entries. Do not change dependency versions as cache recovery.

Git worktrees, active source, user configuration, package-manager state, and the
canonical installer bundle are not disposable compiler caches. A failed Herdr
worktree removal is a separate lifecycle boundary, not authorization to recursively
delete its residual directory.

Prepare a persistent ignored input bundle directly only when supplying already
built runtime, launcher, helper, or staged ConPTY payloads:

```powershell
python scripts/local_windows_installer.py prepare `
  --source-worktree <materialized-source-worktree> `
  --stage-dir <validated-stage-directory> `
  --launcher-exe <herdr-launcher.exe> `
  --installer-helper-exe <herdr-installer-helper.exe>
```

The command copies only regular non-reparse inputs below
`target/x86_64-pc-windows-msvc/installer-inputs/<build-id>/`, records every file
SHA-256 and the UTC freshness label, and binds the bundle to the runtime and launcher
identities. It never
extracts a prior setup with 7-Zip. The source ConPTY validator remains the stage
owner.

For development installer, artwork, copy, validator, or packaging-only iterations,
reuse that bundle without rebuilding Rust payloads:

```powershell
python scripts/local_windows_installer.py build `
  --source-worktree <materialized-source-worktree> `
  --input-bundle <reported-input-bundle>
```

When the runtime product identity itself is the packaging change, pass the explicit
validated input without changing or rebuilding the runtime bundle:

```powershell
python scripts/local_windows_installer.py build `
  --source-worktree <materialized-source-worktree> `
  --input-bundle <reported-input-bundle> `
  --product-name "<runtime product name>"
```

`candidate` accepts the same optional input. An explicitly requested, isolated
`release-precheck` diagnostic must use the same value for that installer. Omitting
it keeps the current `Herdr` default. The materialized packager remains the one
validation owner.

Every invocation rechecks all bundle hashes, exact ConPTY stage contents, runtime
and launcher identity, then delegates to the materialized source's existing NSIS
packager. The exact development branch writes the setup to the one short
replaceable path
`target/x86_64-pc-windows-msvc/release/herdr-win_local_candidate_setup.exe` and
reports its freshness label and new hash. After that publication, only its matching
bundle remains. A topic branch selects isolated generated state that the completed
candidate check removes; pass
`--isolated` explicitly when forcing that behavior on the development branch. A
tiny, clearly bounded daytime packaging-only request uses
a soft goal of roughly two minutes from the user request to that installable
artifact. Request-to-artifact is the primary user-wait metric; build time,
development integration, remote backup, and promotion are separate timings. The goal
is never a deadline or reason to stop discovery, diagnosis, or a running build;
complex work has no two-minute expectation. Report the installer immediately so
user and agent testing can continue in parallel. Missing or corrupt inputs are a
clear preparation blocker, not authority to unpack an old installer or repeatedly
rebuild unchanged payloads.

### Installer acceptance and execution location

The full installation, uninstallation, and installer fault matrix belongs to the
fresh GitHub-hosted Windows runner in `.github/workflows/release.yml`, where it
checks the actual release artifacts. It is **not** an additional local prerequisite
before dispatching the release build. Do not repeat that complete matrix in the
actively used development Sandbox or user profile.

Local work retains build and package-integrity validation, focused source checks,
and native probes whose state and process ownership are demonstrably isolated.
User-performed installation and usage checks remain acceptance evidence; reuse them
while the relevant artifact, source, and environment assumptions are unchanged.

A separate runtime or remote sidecar, an alternate Herdr session, or an overridden
temporary data directory does not establish installer isolation. These tests can
still modify shared HKCU registration, user PATH, launcher, and activation state.
Do not change those shared resources underneath a running working environment.

<details>
<summary>Explicit installer diagnostics in an isolated environment</summary>

The existing `release-precheck` command is retained for a specifically requested
installer investigation, not routine release preparation. Use it only after proving
that its Windows installation/profile state is isolated from active work. A release
request alone does not authorize running this mutating diagnostic in the working
Sandbox:

```powershell
python scripts/local_windows_installer.py release-precheck `
  --source-worktree <materialized-source-worktree> `
  --input-bundle <reported-input-bundle>
```

This reuses the existing installer recovery, hard-termination, managed-skill, and
pending-update fixtures. It runs through Windows PowerShell 5.1,
uses a short ignored output directory, and removes its generated fault installers
after completion. The pending-update fixture deliberately constructs Windows CRLF
checkout input before writing canonical package bytes, and holds its synthetic
runtime lease until an explicit signal releases it. Never replace either boundary
with a fixed-duration sleep. Passing evidence remains reusable while the source
worktree, bundle bytes, and relevant environment remain unchanged.

</details>

### Refreshing from official upstream

Do not fetch, merge, rebase, or advance the queue to newer
`herdrdev/herdr` source as part of an ordinary feature, fix, documentation, or
maintenance task. Refresh official upstream only when the user explicitly requests
that separate operation. For every approved refresh:

1. Query the official latest GitHub release and require it to be neither draft nor
   prerelease.
2. Fetch its exact `v<version>` tag, peel the release commit, and verify the tag
   version matches replayed Cargo package version.
3. Replay and review the complete queue on that commit, dropping upstreamed hunks
   and anything no longer required by current fork behavior from its logical owner.
4. Reconstruct one linear responsibility commit per retained mailbox on that exact
   stable commit. Preserve author/date/message and qualified upstream references.
   The cumulative development history may contain merges; never rewrite it merely
   to satisfy the queue representation. Use Git's temporary index and `commit-tree`
   for the internal logical stack when necessary.
5. Run the stable-refresh owner with the full logical head and accepted tree:

   ```powershell
   python scripts/delta_workflow.py refresh --base <stable-commit> `
     --head <logical-stack-head> --expected-tree <accepted-source-tree> `
     --drop-mailbox <obsolete-series-entry.patch>
   ```

   Repeat `--drop-mailbox` only for responsibilities now completely absent from
   the delta. The command stages all mailboxes privately, preserves retained
   metadata, rejects control files, and proves complete replay equality before
   replacing the queue and `BASE`; failures preserve existing inputs.
6. Before publishing control `master`, run `delta_workflow.py compile-prefixes`.
   Every Windows x86_64 prefix must compile with all logical processors and
   incremental disabled. Use `--work-dir <absolute-task-owned-empty-directory>`
   throughout one refresh correction cycle to retain both the source checkout's
   native Zig caches/output and its sibling Cargo `target`. Choose a short Windows
   path: Zig's relative generator-executable launch can hit
   `MAX_PATH` before normalization even when the resolved executable path is shorter.
   The first invocation accepts only a new or empty directory outside the control
   checkout. Subsequent invocations require the same local repository and `BASE`,
   reject dirty source or unfinished replay, and return to `BASE` without forced
   checkout or cleanup.
   Do not edit or share this disposable workspace. A compile failure retains it;
   a replay conflict requires inspection rather than automatic recovery. Use a new
   workspace when changing `BASE`. Every invocation replays and checks every prefix,
   printing its exact tree; compiler caches are never verification attestations.
   `--target-dir` remains the Cargo-only alternative and cannot accompany
   `--work-dir`. After the refresh, inspect the workspace and explicitly remove
   only that caller-owned directory, using extended-path-aware cleanup on Windows.
   No automatic expiry or cache index is maintained.
   Evidence from the exact same privately staged prefix trees
   remains valid; queue finalization or cleanup alone does not require recompilation.

Between explicit refreshes, `BASE` remains pinned to that reviewed stable release;
there is no scheduled upstream query, replay, build, or release. Manual candidate
build and promotion dispatches are not an upstream refresh; the build operation
must use the stable commit already recorded in `BASE`.

Repository branding, GitHub Actions, patch metadata, and release orchestration
must not be included in product mailboxes.

Patch 0004 owns the replayed managed Windows distribution. Preserve the update,
installer, exact-layout rejection, and cross-agent-skill boundaries documented in
`ARCHITECTURE.md`; changing one is an explicit architecture/product decision, not
incidental mailbox maintenance.

Every explicit release assignment and every ConPTY dependency refresh must query
the official Microsoft package and release metadata, then select the newest
non-preview `Microsoft.Windows.Console.ConPTY` package that has been published for
at least seven complete days. Keep it reproducible: pin its exact package version,
release tag, URL, package SHA-256, and extracted runtime hashes in
`packaging/windows/conpty.json`. Never use a floating version, preview package, or
runtime auto-update. If the newest eligible stable package is already pinned, do
not create version churn. A changed pin must pass the focused package/vendor checks
and the packaged native ConPTY gate before promotion.

`website/preview.json` is generated by the promotion operation after release
publication. Do not hand-edit it or include its manifest-only commit in release
source identity. Repository release immutability is a required external setting
mirrored by the `HERDR_RELEASE_IMMUTABILITY_ENABLED=true` repository variable. The
workflow must still verify the actual release is immutable, generate the manifest
with the candidate's tested replay generator, commit only `website/preview.json`,
and fail closed if `master` advances beyond the candidate's control revision.

Use the single manual **Build and promote Herdr Extended release** workflow in two
separate dispatches:

1. Choose `build` and supply one unused Herdr Extended CalVer in `YYYY.MM.DD.N` format.
   Use the intended UTC release date and increment `N` for another release that
   day. The successful run retains the complete candidate and its provenance for
   14 days but does not publish a release. Record its workflow run ID. The workflow
   derives one candidate-scoped runtime build ID from the selected control commit,
   run ID, and attempt; never assign that identity to a separately built local
   artifact.
2. Review the successful candidate run, then choose `promote` and supply that run
   ID before its artifacts expire. Promotion accepts no replacement CalVer and
   does not replay source, compile, or package; it publishes only the candidate's
   validated files as one normal, non-prerelease GitHub release and marks it Latest.
   If `master` has advanced or the candidate expired, dispatch a new build instead.

Do not reuse one CalVer for different source. Linux and macOS publish raw
`herdr-win_v<CalVer>_{linux,macos}_{amd64,arm64}` executables. Windows publishes
`herdr-win_v<CalVer>_windows_amd64.zip` and appends `_setup.exe` for setup; upstream
package versions and source/control hashes remain separate provenance. Preserve
these machine-consumed names; show the stable Herdr version beside the CalVer in
the GitHub release title, notes, and installer metadata instead of changing
updater-facing filenames.

The retained candidate compiles that CalVer into every platform binary.
`herdr --version` must be `herdr-ext <CalVer> (Herdr <upstream-version>)`; Windows
setup and Installed Apps use the same CalVer as their primary display version.
Separately built local artifacts use one UTC `YYYY.MM.DD.HHMMZ` freshness label plus
their secondary build ID and must never claim a release CalVer.
That CalVer is also the fork update-order key. Promotion must reject a candidate
whose CalVer is equal to or older than the published manifest, and the updater
must reject an equal or older feed CalVer regardless of build ID. Build ID remains
the immutable runtime and matching remote-asset key; protocol remains the
client/server compatibility gate. The distribution feed is fixed at compile time,
with no `herdr channel` command or `update.channel` setting.

The build fails closed on replay conflict, source drift, or a wrong installer pin.
Promotion additionally validates the selected successful workflow run and attempt,
source/control identities, expected file set, and every digest before publication;
it also fails on missing or mutable assets, a draft or prerelease classification,
or feed content that was not fetched and verified independently. Runtime builds
retain the manifest's `preview` schema token for wire-format compatibility, while
the required `prerelease: false` value gates update selection and CalVer owns fork
update order and release presentation. All builds use only fork-owned update/setup
sources.

Manual release work is ephemeral: never create or force-push an integration
branch, merge upstream into a release branch, resolve replay conflicts
automatically, or publish releases from ordinary pushes. A conflict fails closed.
Once promotion publishes the immutable release and manifest commit, publication is
complete even though the separate public-feed verification job may still be waiting
for GitHub's branch-content cache. If only that post-publication job fails, keep the
same release and CalVer and rerun only the failed jobs from the original promotion:

```powershell
gh run rerun <promotion-run-id> --failed --repo hdosys/herdr-ext
```

Never dispatch promotion again for that candidate. The existing immutable release
is canonical, and a complete promotion rerun must remain blocked by the equal-CalVer
gate. The failed-job rerun uses the original workflow revision to download and
validate the complete canonical asset set and the real fixed updater URL without
replacing or repointing an asset. Promotion never deletes a tag or GitHub release.
Preserve any draft or mutable release and fail before manifest publication instead
of trying to recover by removing public state.

### Preparing a WinGet manifest update

Start from the exact pull-request branch with Git's sparse mode enabled before any
checkout populates the working tree. Never convert a full clone or a `--no-checkout`
clone into this path after the index has represented omitted files:

```powershell
git clone --filter=blob:none --sparse --single-branch --branch <pr-branch> `
  https://github.com/microsoft/winget-pkgs.git <checkout>
git -C <checkout> sparse-checkout set manifests/h/hdosys/herdr-win
git -C <checkout> status --short --branch
```

The final status must be clean before editing package manifests. This procedure
only prepares an external contribution checkout; it does not authorize manifest
changes, a pull request, or release publication.

## Verification

The **Fork verification policy** in `AGENTS.md` is the admission gate for every
new or retained Herdr Extended check. Change evidence must identify its stable contract,
unique realistic failure, cheapest reliable layer, and observed runtime. Remove or
retier a check when those facts no longer justify its maintenance or wait cost.
Workflow review rejects software provisioned only to satisfy a test.

During explicit promotion, run the fast inventory checks from the control repository:

```powershell
python -m unittest scripts.test_delta_patches scripts.test_upstream_patches
```

Run formatting and the smallest changed-behavior test in the replayed task tree
before recording its tested tree ID. The finalizer's exact tree match transfers
that evidence to the checked-in queue without another checkout, compile, or test
pass. Do not run blanket Clippy or all Rust tests for every ordinary edit.

For Windows packaging changes, select only the focused package or vendor modules
that own the changed boundary rather than running the whole list by default:

```powershell
python -m unittest scripts.test_package_windows_conpty
python -m unittest scripts.test_vendor_libghostty_vt
python -m unittest scripts.test_vendor_portable_pty
```

After the first artifact, run Windows-target Clippy only when the exact changed
boundary requires it. Broad native and cross-platform matrices belong to an
explicit release or unattended verification assignment:

```powershell
cargo clippy --bins --locked --target x86_64-pc-windows-msvc -- -D warnings
```

An explicit release assignment uses the manually dispatched workflow's `build`
operation for the Linux/macOS target builds and machine checks as well as the
signed ConPTY package, native quiet-uninstall checks, installer-helper lifecycle
and focused fault-retry matrix, managed launcher, and system-fallback gates that
depend on GitHub's Windows runner.

Workflow changes require `actionlint` plus review of triggers, permissions,
credential persistence, immutable source identity, artifact digests, and failure
behavior. Native package or installer changes build and report the local artifact
first, then run the smallest equivalent real-platform evidence. The full release
gate remains reserved for an explicit release assignment.

Documentation, process, and canonical-owner-only changes that do not alter a
mailbox or executable workflow use inline review, `git diff --check`, README mirror
checks when applicable. Run queue inventory tests only when the change touches
`BASE`, `series`, mailbox or archive invariants, or their control scripts. Pure
documentation and process changes do not require product replay, a Rust gate, or
the native installer matrix.

## Documentation

Keep `PRODUCT.md` as the concise canonical user-visible truth and
`ARCHITECTURE.md` as the stable technical truth. Project their relevant public
facts into the fork README without turning it into internal design documentation.
Keep root `README.md` and `docs/next/README.md` byte-for-byte identical. Product
documentation carried in release source belongs in the logical mailbox that owns
the behavior. When a completed, pushed development candidate fixes a referenced
upstream issue, add its concise user-visible outcome to the README's current
development candidate section in the same milestone. List only fixes actually on
`origin/candidate/development`, link the fully qualified upstream report, and state
that they remain unpublished. At explicit patch promotion or release, move each
entry into its normal capability or changelog owner instead of retaining a stale
candidate list. Root `CHANGELOG.md` records only Herdr Extended CalVer releases and
user-visible fork changes; link the official upstream Herdr changelog instead of
copying upstream release entries. Do not edit changelog, release notes, website,
or broad docs unless changed behavior requires it, and never edit generated
preview/version documentation directories. The repository pre-commit hook rejects
invalid staged whitespace before it can enter the worktree.

## Local issue-reference triage

Run `python scripts/delta_workflow.py issue-report` before issue-owner triage.
It prints deterministic JSON from the current series and mailboxes only: qualified
references (including hunk references), subjects, touched source paths, mailbox
SHA-256, and the mailbox's recorded source commit. Repeated references within a
mailbox produce one row; mailboxes without references remain visible. `null` means
unknown, not permission to query upstream. Touched paths are ownership leads, not
proof that a particular hunk fixes an issue; the recorded source commit is not a
claim of current reachability or release inclusion.

An explicit `--ledger <private-ledger-path>` optionally joins existing canonical
`### owner/repository#number` sections. Only single-line `- Title:` and
`- Local outcome:` fields before a nested heading are included. Missing captured
fields stay unknown. No ledger is discovered automatically; author headers,
verification text, and drafts are not emitted. Treat an explicitly joined report
as private, review the selected fields before sharing, and never commit generated
reports. This command performs no replay, history scan, network call, or ledger
write and creates no second maintained issue index.

### Focused upstream follow-up

Only perform the exact upstream reads authorized by the current user. Reuse an
existing local report while its inputs are unchanged. Start with the reported
symptom, reproduction, affected process and current issue/PR state; then follow
the relevant source owners rather than scanning complete history or every hunk.

```powershell
gh issue view <issue> --repo herdrdev/herdr --json number,title,state,url
gh pr view <pr> --repo herdrdev/herdr --json state,headRefOid,mergedAt,url
```

Read the relevant reproduction once. For long threads, list comment identities
before selecting the acceptance, correction or remaining-failure evidence:

```powershell
gh api 'repos/herdrdev/herdr/issues/<issue-or-pr>/comments?per_page=100' --paginate `
  --jq '.[] | {id,author:.user.login,updated_at,html_url}'
gh api 'repos/herdrdev/herdr/issues/comments/<comment-id>' --jq '{html_url,body}'
```

Do not filter solely by author type: bot comments can contain relevant fix,
verification or closure evidence, and some automated accounts have user accounts.
For a bridge/IPC fix, request the actual owner hunks rather than the full PR:

```powershell
gh api 'repos/herdrdev/herdr/pulls/<pr>/files?per_page=100' --paginate `
  --jq '.[] | select(.filename == "src/remote/attach.rs" or .filename == "src/ipc.rs") | {filename,status,patch}'
```

Change the selected paths to the owners established for that issue. Missing or
truncated patches require reading the exact source blob at the recorded commit,
not inferring that behavior is absent. Compare against the immutable accepted fork
and recorded `BASE`; a merged PR is not proof of stable-release inclusion or full
fork equivalence. Stop when the bounded question is answered. Keep separate facts
for an observed symptom, a demonstrated root cause, fork exposure and missing
runtime evidence. These reads never authorize a post, import, or new recovery path.

## Pull requests and commits

- Keep pull requests focused on one logical owner.
- Explain how the queue was replayed and which Windows gates ran.
- Use lowercase conventional commit subjects, without emoji or AI co-author
  lines.
- Do not commit generated artifacts, credentials, logs, or temporary replay
  checkouts.
- Do not open upstream issues or pull requests on someone else's behalf.

By contributing, you agree that your changes are licensed under the repository's
Apache License 2.0.
