# Project source control and native tool settings

Research date 2026-10-31. This is a design memo, not an application implementation.

The owner wants a source-control choice when adding a project. Newjen must support Git, Jujutsu, and both with jj primary. The owner clarified that "Git as backup" means using Git for operations jj does not implement, against the same history. Settings must show both installed tools, refresh their status, check for updates, and support an appropriate update action.

This request supersedes the earlier blanket choice to initialize jj in every plain-Git Rift copy. Git mode must work without jj installed. The [landing memo](git-only-landing.md) retains the transfer experiments and protection requirements. Its automatic apply requirement for folders without version control still applies.

## Recommended project choices

Offer three choices during Add project:

- **Git.** Use Git for project status, snapshots, branches, commits, and remotes. Do not initialize jj in the project or its Rift copies.
- **Jujutsu.** Use jj for the workflow. Initialize a new repository with `jj git init --no-colocate`, placing private backing storage under `.jj`. Normal Git commands do not operate directly in that working folder.
- **Jujutsu + Git.** Use jj for normal Newjen actions. Initialize a new repository with `jj git init --colocate`, keeping `.git` alongside `.jj` so Git tools can access the same history when needed.

"Jujutsu only" describes the user's workflow, not storage without Git. The installation docs require Git 2.42 or newer. The dependency is capability-specific in the tested jj 0.46.0. Local initialization, snapshots, bookmarks, import, export, and colocation conversion worked with Git absent from PATH and an invalid configured Git executable. Remote operations, including fetch from another local directory, failed without Git. Settings can therefore report "Ready for local work" while disabling remotes, fetch-based landing, Git fallback, and any binary export implemented through the Git CLI. Do not label every jj operation unavailable because Git is missing. [jj installation and runtime requirements](https://docs.jj-vcs.dev/latest/install-and-setup/#runtime-requirements).

Call the combined option **Jujutsu + Git**, with the description "Use jj primarily, with Git compatibility." Both tools share one backing store. This provides a fallback, not an independent backup. A scratch Git clone preserved an exported commit but did not contain `.jj` metadata. jj's operation log records repository views and recovery operations beyond ordinary Git branch history. [jj operation log](https://docs.jj-vcs.dev/latest/operation-log/).

The owner confirmed Git fallback on shared history. Independent backup storage is outside this requirement. The combined mode must expose Git operations where jj has no native equivalent while keeping jj as the primary workflow.

## Add a project without hiding a conversion

Detect the repository before presenting the choice. Detection must validate repository roots and metadata paths, not just check whether marker names exist.

For an existing Git repository, preselect Git. For a non-colocated jj repository, preselect Jujutsu. For a colocated repository, preselect Jujutsu + Git. Explain the detected layout beside the selection. Do not replace existing metadata merely because a different mode is highlighted.

For a new folder, the final Add project summary must say what initialization will create. Git creates `.git`. Jujutsu creates `.jj` with hidden Git backing storage. The combined option creates `.jj` and a colocated `.git`. Always pass the chosen colocation flag explicitly. jj 0.46.0 defaults to colocation, and user configuration can change that default. Probes confirmed that explicit flags override either configuration. The non-colocated choice keeps ordinary Git tools out of the jj-only working folder, at the cost of an explicit export step for fetch-based transfer.

Keep a secondary **Continue without source control** action for ordinary folders. This preserves the owner's earlier requirement for review and automatic apply without forcing initialization. It is not a fourth competing repository format.

Selecting a mode and converting a repository are different operations. Existing repositories need an explicit conversion summary before any metadata change. If conversion is unavailable for that layout, retain the project and explain which setup is required.

The installed jj 0.46.0 supports `jj git colocation enable` and `disable`. The scratch probe enabled and disabled colocation while preserving working-file bytes and an exported bookmark commit. The documented commands move the backing Git directory between the workspace root and `.jj`. This is not equivalent to changing a UI preference. [jj colocation commands](https://docs.jj-vcs.dev/latest/cli-reference/#jj-git-colocation).

Require no active Newjen turn, background task, or repository mutation during conversion. Preserve recovery data first. Inspect staged work and unfinished Git operations before importing an existing repository. jj does not preserve Git's staging model. Do not promise that disabling colocation or choosing Git removes jj metadata safely in every layout. Those transitions need their own tested conversion paths.

An existing colocated project cannot be treated as a pristine Git-only repository simply by hiding the jj controls. Display the layout mismatch and offer an explicit transition. Never delete `.jj` as a side effect of changing a selection.

## What T3 actually implements

The detailed source inspection uses T3 commit `9c0fb54e8cb3bc15d52c5832d5e4514ee589b784`. A separate read of the current upstream settings and discovery files confirmed the same Git-versus-jj distinction. This was source research, not a live UI run.

### Installed tools and settings

T3 separates installed version-control tools from hosting integrations. Its Git row shows discovery state and a detected version. The availability switch is disabled. It is an indicator, not a project mode selector. The Jujutsu row can show a detected jj version while still displaying Coming Soon because implementation support is a separate field. [SourceControlSettings.tsx](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/web/src/components/settings/SourceControlSettings.tsx#L208).

The rescan button calls the discovery query's `refresh`. Discovery runs `git --version` and `jj --version`, with bounded output and a timeout. It supplies installation guidance after a failure. It does not install, upgrade, or check the latest release of either tool. [SourceControlDiscovery.ts](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/server/src/sourceControl/SourceControlDiscovery.ts#L45).

Discovery targets the selected server environment. Its input has no project directory. Repository defaults such as automatic pull and PR merge method live in separate scoped settings. Hosting account state is also separate. [Discovery query](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/packages/client-runtime/src/state/sourceControl.ts#L15), [repository defaults](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/web/src/components/settings/ProjectDefaultsSettings.tsx#L358).

T3's application updater checks and installs T3 itself. It is not a Git updater. [AboutVersionSection](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/web/src/components/settings/SettingsPanels.tsx#L143).

### Repository operations

T3 has a VCS driver registry, but registers only Git. Its automatic selection chooses Git. An explicit jj request produces an unsupported-driver error. A `.t3code/vcs.json` preference is an extension point, not completed jj behavior. Newjen must implement both drivers rather than reproduce that placeholder. [Registry](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/server/src/vcs/VcsDriverRegistry.ts#L60), [project configuration](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/server/src/vcs/VcsProjectConfig.ts#L72).

The useful separation is repository detection, snapshots, status publication, and hosting. T3's process layer centralizes execution limits and errors. Its status broadcaster coalesces local refreshes, shares remote requests, and checks branch identity before publishing remote results. [VCS driver](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/server/src/vcs/VcsDriver.ts#L53), [process execution](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/server/src/vcs/VcsProcess.ts#L28), [status broadcaster](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/server/src/vcs/VcsStatusBroadcaster.ts#L370).

T3 distinguishes local status from fetch and pull. Its background fetch updates remote refs. Its guarded pull uses `--ff-only`. Neither updates the Git executable. Newjen should retain those distinctions, but not copy automatic pull into a jj workflow. [Git status and fetch](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/server/src/vcs/GitVcsDriverCore.ts#L1308).

Internal Git checkpoints use a temporary index, tree objects, commits, and refs. User-facing commits use ordinary `git commit` and stream command output. Its restore path can restore the entire checkout and clean untracked files. That is not an appropriate implementation of thread-specific discard in Newjen's shared project folder. [Checkpoints and restore](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/server/src/vcs/GitVcsDriver.ts#L891).

T3's workflows still call Git-specific services above the driver boundary. The presence of a generic registry does not remove assumptions about attached HEAD, the index, or worktrees. Rift copies also have independent repositories, unlike T3's shared worktree backing store. [GitWorkflowService.ts](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3bc15d52c5832d5e4514ee589b784/apps/server/src/git/GitWorkflowService.ts#L264).

## Native Newjen settings

Use native GPUI Kit controls and its default theme. No browser-based settings panel is needed. This memo specifies behavior, not unverified component APIs.

**Settings > Source control > Tools on this Mac** contains one row each for Git and Jujutsu. Each row shows the selected executable, installed version, compatibility with Newjen, installation origin when known, last successful check, and current action state. Missing, failed to probe, unsupported, and ready are different states.

Use separate actions with separate effects:

- **Refresh tools** resolves the configured paths and reruns version and capability probes. It does not contact project remotes or upgrade anything.
- **Check for updates** queries the selected tool's update channel. It reports a newer release, up-to-date information, or an unknown result with a timestamp.
- **Update** runs the supported update method for that installation after an explicit user click. It streams progress and re-probes the exact tool Newjen will use.
- **Refresh changes** updates repository observations. In jj modes it does not snapshot unsaved working files.
- **Fetch** updates repository remote state. It is a repository mutation, not a tool refresh or upgrade. Its jj implementation needs an explicit working-copy policy rather than assuming Git fetch semantics.
- **Apply**, **integrate**, and **push** remain separate project operations with their own review and target.

Do not copy T3's disabled availability switch as the mode selector. Mode belongs in Add project and Project settings. Installing tools is machine-wide, while choosing which workflow a project uses is project-specific. Hosting and authentication belong in a separate section. Git availability alone does not establish access to a remote host.

Show a supported tool as usable even when an update check is offline. A failed network request must not turn an installed tool into Missing. An unsupported newer version must not be labelled Ready merely because `--version` succeeded.

### Update through the installation that owns the selected binary

For a verified Homebrew installation, use Homebrew's machine-readable package information for the selected formula and its installed keg. The API can report release metadata, and the local installation record identifies what Homebrew actually owns. A similarly named binary elsewhere on PATH is not enough evidence. [Querying Homebrew](https://docs.brew.sh/Querying-Brew), [formula API](https://formulae.brew.sh/docs/api/).

Offer an in-app update only when the planned target is within Newjen's tested support range and ownership of the selected installation is established. Otherwise show the compatibility reason and external guidance.

A native update action can run a fixed, formula-specific `brew upgrade git` or `brew upgrade jj` command through Newjen's process runner. Do not run a blanket upgrade. Homebrew can also upgrade dependencies, upgrade or reinstall dependents, refresh package metadata through an implicit `brew update`, and run cleanup. Disclose those possible effects. A formula-specific command is not a promise to touch only one package. Use the selected installation's channel and formula revision, not a naive comparison with an arbitrary upstream version string. [Homebrew command reference](https://docs.brew.sh/Manpage).

Apple's Git comes with Xcode or Command Line Tools. Show that origin and direct updates through the Apple-managed mechanism. Do not overwrite `/usr/bin/git`, install a second Git silently, or run Homebrew against Apple Git. Custom, Cargo, MacPorts, and unknown installations initially get installation-specific guidance unless Newjen implements and tests their updater. [Git installation on macOS](https://git-scm.com/install/mac), [jj installation](https://docs.jj-vcs.dev/latest/install-and-setup/).

The native row remains useful when an installation cannot be upgraded in-app. It can show status, check the relevant channel where known, open update instructions, and refresh after an external update. "Update available" and "Newjen supports this version" must remain independent.

Block an app-managed tool update while Newjen turns, background tasks, or repository mutations may be using that tool. Serialize updates machine-wide and do not kill agents to perform them. Report the shared-project impact. External programs can still use or update these binaries, so this is not a machine-wide process lock.

After an update, resolve the configured path again, verify the version and required capabilities, and invalidate affected detection and status caches. If the upgraded tool fails compatibility checks, preserve repositories and show recovery guidance. Do not silently switch a project from jj to Git. If the selected path still resolves to an older Apple or custom binary, report that clearly instead of claiming the update is active in Newjen.

Path resolution must work when launched from Finder. Reuse the project's tool-resolution conventions and connect the Rift PATH follow-up rather than inventing another unrelated resolver. Settings and actual operations must use the same selection. In jj modes, pass the chosen Git executable through jj's documented `git.executable-path` setting for app-launched commands. [jj subprocess configuration](https://docs.jj-vcs.dev/latest/config/#git-subprocessing-behavior).

No update or package-manager install was performed for this investigation.

## jj rules that change the implementation

The jj FAQ says neither its library API nor CLI is stable. Its library offers structured data, while the CLI respects the user's installed or custom jj. Because Newjen must reflect tool updates, the recommended MVP uses version-aware CLI adapters, not an embedded library tied to an unrelated installed version. Support a tested version range and capabilities. Do not promise immediate support for every future release. [jj integration FAQ](https://docs.jj-vcs.dev/latest/faq/#i-want-to-write-a-tool-which-integrates-with-jujutsu-should-i-use-the-library-or-parse-the-cli).

jj has a working-copy commit rather than Git's staging area. Most commands snapshot files. Background observers need `--ignore-working-copy`; intentional checkpoint operations must be explicit. That flag observes the last recorded state, so it cannot by itself report every unsnapshotted edit as a live diff. Read current files against the saved baseline without mutating jj, or label the result as the last checkpoint with its freshness. Never present stale recorded state as a clean live working folder. In the scratch probe, version and observer refreshes left both the working commit and operation ID unchanged, while `jj util snapshot` recorded the edit. [Working copy](https://docs.jj-vcs.dev/latest/working-copy/).

Bookmarks name revisions, but there is no active bookmark equivalent to Git's checked-out branch. A probe verified that `jj new` did not advance the bookmark. The UI must show the working change and relevant bookmarks without inventing an active Git branch. Use immutable commit IDs for reviewed results, even when the jj change ID persists through edits. [Bookmarks](https://docs.jj-vcs.dev/latest/bookmarks/).

A successful jj operation can leave conflicts recorded in its result. Model conflict state explicitly instead of treating exit status zero as a clean landing. A user-facing Git fallback must not mutate a jj-conflicted working tree blindly. [First-class conflicts](https://docs.jj-vcs.dev/latest/conflicts/).

jj fetch and import can abandon revisions that became unreachable, and replacing an abandoned working-copy commit can create a new change. A reviewer probe fetched a rewritten local remote and reproduced abandonment, descendant rebasing, and removal of a working file in both layouts. A rerun with `--ignore-working-copy` preserved the file bytes but still changed revision history. The flag alone is not a remote-read safety guarantee. Treat remote synchronization as an intentional repository operation with preflight, coordination with active writers, and result checks. Keep it out of installed-tool discovery and passive observers. The CLI reference documents these effects. [jj fetch](https://docs.jj-vcs.dev/latest/cli-reference/#jj-git-fetch).

Colocation imports and exports Git state automatically during ordinary jj operations. It does not preserve Git's staging workflow. Background Git writers can therefore affect a jj-primary project. Keep ordinary Newjen mutations jj-driven in both jj modes. In the combined mode, expose a deliberate Git action for operations jj does not implement. Coordinate that action with jj operations, then reconcile and refresh the resulting jj state. Do not run both as simultaneous writers. Select fallback by operation capability, not by retrying every failed jj command through Git. Authentication errors, conflicts, or an unsupported tool version are not evidence that Git should retry the operation. Git compatibility has documented limitations, including hooks and attributes, so unsupported projects need a reason and an available Git mode. [Git compatibility](https://docs.jj-vcs.dev/latest/git-compatibility/).

Use the documented `jj --ignore-working-copy git root` to discover backing storage. The scratch probe verified it for non-colocated and colocated layouts. Do not hardcode `.jj/repo/store/git` in production. The earlier landing probe used that internal path only to establish binary export mechanics.

## Core, app, and store boundaries

Keep four facts separate in the domain model:

- **Tool installation.** Git or jj executable selection, probe result, support, and update state on this Mac.
- **Project workflow.** Git, Jujutsu, or Jujutsu + Git, selected by the user.
- **Detected repository.** Git-only, non-colocated jj, colocated jj, unversioned, unsupported, or inaccessible, with canonical identity and layout.
- **Hosting connection.** Remote location and access state, independent of Git or jj choice.

These are separate variants and records, not a bag of booleans such as `use_git`, `use_jj`, and `git_backup`. A mismatch between requested workflow and actual layout needs a setup result rather than a silent fallback.

Core owns typed discovery, operation execution, snapshots, update coordination, and errors. The app renders state and sends user intents. Add independent Git and jj adapters around shared capabilities such as status, diff, checkpoint, apply, and remotes. Preserve backend-specific details where they affect decisions. Do not invent a universal "commit" operation that hides jj's working-copy and bookmark semantics.

Persist project workflow and initialization state with the project. Persist tool preferences at the machine settings level. Keep transient scanning and progress state out of durable project settings. Existing projects need detection and a confirmed mode when ambiguous, not an unconditional migration to jj. The current [`projects` table](../../crates/core/src/store.rs) has no source-control mode, and [`rift::Tools`](../../crates/core/src/rift.rs) only selects Rift and jj. This memo introduces no columns or APIs.

Status refresh should coalesce requests per canonical repository and discard stale results if repository identity, layout, mode, selected tools, or generation changes. Use repository-specific observers. A settings scan must never make checkpoints or start an agent turn.

Both Claude and Codex receive the chosen project workflow and allowed version-control behavior. Source-control execution is independent of provider wire events. Agent-managed mode still means the harness does not alter the agent's repository state on its behalf.

## Rift and shared-folder behavior

Git projects get ordinary independent Git copies. jj projects get independent jj copies using the selected layout. Preserve the existing jj rule against editing a shared mutable change across source and copy. Every copy records its baseline kind and immutable reviewed result.

Returning edits must follow the project's selected mode. A Git target uses the Git implementation, a jj target uses the jj implementation, and an ordinary folder uses the recoverable file-apply path. Keep the owner's apply-without-an-automatic-final-commit behavior. In jj modes, working files are already part of jj's working-copy model, so describe that effect accurately rather than claiming that jj leaves work "uncommitted" in Git's sense.

A Git-only project cannot depend on jj for hidden snapshots or landing. This adds a real Git implementation requirement for slice 4. The previous memo's dirty-baseline, binary, index, whitespace, copy-independence, and recovery requirements still apply.

For **commit or branch import from a non-colocated jj copy**, the copy path alone is not a usable Git remote. Under harness-managed mode, freeze writers, finalize the reviewed snapshot and bookmark, and run `jj git export` in the copy to export its bookmarks. Discover its backing store with `jj --ignore-working-copy git root`. Fetch the selected thread bookmark from that store in the destination with the selected Git executable available to jj. Verify that the imported bookmark points to the reviewed immutable commit. Export and fetch are mutations, not passive discovery.

The targeted scratch probe reproduced both failures. Fetching the non-colocated workspace path failed to find a Git repository. Fetching its backing store before export found no thread branch. Explicit export followed by fetch imported the expected commit. A colocated copy exports bookmarks automatically, so its ordinary copy path works as the remote. Do not apply the existing colocated landing recipe unchanged to jj-only projects.

In agent-managed mode, running export on the agent's behalf would violate the no-version-control-writes promise. Require an already exported agent-created ref for branch import, or use a separately designed file-apply path that preserves that promise. The export recipe is an import mechanism, not permission to merge inherited baseline commits or replace the reviewed net-delta apply. A direct snapshot-file apply is also distinct from Git-based binary-patch export and may have different tool requirements.

Project-folder threads have already edited their destination. They need review and keep semantics, and safe baselines for any discard action. The project choice does not solve overlapping writers. Do not copy T3's whole-checkout restore into thread-specific discard.

## Verification and handoff

The local probe `/tmp/newjen-git-landing/project-modes.py` passed twelve assertions with Git 2.54.0 and jj 0.46.0. It verified all three layouts, independent Git copies, hidden backing storage, read-only refresh, explicit snapshots, bookmark behavior, colocation conversion, Git clone limits, and jj's configured Git executable dependency. The artifact location is in `/tmp/newjen-git-landing/latest-modes`.

A separate reviewer probe of a rewritten remote verified the fetch effects described above. Its scripts are `/tmp/newjen-git-landing/modes-review-astra/rewritten.py` and `ignored-fetch.py`. Both were independently rerun.

The additional local probe `/tmp/newjen-git-landing/fable-check.py` passed eight assertions for explicit layout flags, local jj operations without Git, and non-colocated export and fetch. Its artifact location is in `/tmp/newjen-git-landing/latest-fable-check`. No Homebrew installation or upgrade was performed.

These are mechanics tests, not a completed onboarding, settings, updater, migration, or landing feature. No GUI, build, package installation, or live Claude or Codex test was required. Review and implementation must still cover failed conversion, shared writers, tool replacement, unsupported versions, partial apply recovery, and installation ownership.

GPT 6.1 Sol gave the design a clean review. GPT 6 Astra reproduced one issue in the earlier draft's generic Fetch promise. The corrected jj-specific warning includes that evidence and its working-copy limitations. Sol also checked Git-only operation with jj absent from PATH. Claude Fable 5.1 subsequently completed the previously rate-limited review and approved the three-mode design with clarifications about explicit initialization flags, non-colocated transfer, capability-specific Git requirements, and Homebrew side effects. Those requirements are now recorded here. The implementation-relevant jj findings were independently reproduced, and Homebrew behavior was checked against its help and reference. All requested reviewers have returned. This records completed review and local corrections, not a second unanimous panel verdict on the revised text. The prose-only comment scan found no code comments or suppressions and changed no files.

Implementation is outside this discussion session. The open-project session owns onboarding, slice 4 owns snapshots and landing, and the merge pass owns coordinated schema changes. Native tool settings and updating need a separately assigned implementation if they do not fit an existing lane.

The owner resolved the final product question. "Git as backup" means Git operations fill gaps in jj's native capabilities on the same repository history. It does not require a separate recovery copy. This clarification changes no repository or application code.
