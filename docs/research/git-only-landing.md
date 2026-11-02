# Landing work without an existing jj repository

> **Note (2026-11-02):** Newjen switched to git worktrees and git landing, and no longer uses Rift or jj. The jj and Rift decisions in this memo are historical. See [how landing works now](../landing.md).

The later [project source-control design](project-source-control.md) supersedes the blanket copy-local jj policy below. Git mode now requires a Git implementation without jj, while jj modes use the selected layout. The transfer experiments and automatic apply requirement remain relevant. The decision sections below record the earlier discussion, not the current per-project mode policy.

Research and decision date 2026-10-31. The owner chose jj confined to Rift copies for plain Git projects and required automatic apply for folders without version control. This memo changes no Newjen behavior.

Newjen needs to review and return a thread's work without requiring a Git user to adopt jj. A folder without version control needs the same review-and-apply loop. Both cases include threads that edit the project folder and threads in Rift copies.

## Decision

Use option B for harness-managed Rift threads. Initialize jj only inside the independent copy. Keep the original project Git-only, or without version control if that is how it started. Do not initialize jj in the original automatically.

Record the exact starting files before the agent runs. Review the difference between that snapshot and the final snapshot. **Apply to project** returns only those edits as uncommitted files. Preserve the source's existing staged changes. **Import branch** remains a separate operation for Git projects when the user wants the copy's commits. Importing is local, not a push or a merge.

For ordinary folders, automatic apply is an MVP requirement. Review and export alone do not meet the owner's chosen scope. Use copy-local snapshots and a scoped patch or file transaction to return the reviewed delta without creating `.git` or `.jj` in the original folder. Build baseline validation, recoverable writes, and conflict handling before enabling that action. Keep export as a fallback for unsupported files and existing copies without a baseline, not the default for all ordinary folders.

Project-folder threads already write into the original. Their finish action reviews and keeps those files. Their baseline must live in app-owned storage outside the original, without changing its Git index or adding jj there. Copy-local jj does not itself solve that case. The same rule applies to agent-managed threads that promise no harness writes to their version-control state.

The decision reuses slice 4's jj machinery for copies. It accepts detached Git HEAD and loss of staging distinctions inside harness-managed copies. Those effects must stay out of the original project. The apply action retains the proposed uncommitted-file behavior. The owner's replies selected the snapshot strategy and required non-repo automatic apply, without requesting an automatic final commit.

The research initially recommended native Git snapshots to preserve Git behavior inside copies, with review and export first for ordinary folders. The owner selected jj reuse and a broader automatic-apply requirement instead. The alternatives below remain evidence for that decision, not unresolved choices.

## What other agent environments do

The common pattern is a branch in a Git worktree. Worktrees share commits and branches with the original repository. A Rift copy does not normally share that metadata, so Newjen needs an explicit transfer even when both copies started on the same branch.

| Environment | Observed review and landing behavior | Lesson for Newjen |
| --- | --- | --- |
| Claude Code | Worktree sessions have their own branch. Changed files and new commits cause a keep-or-remove prompt at exit. Worktrees require an existing commit. The docs describe ordinary Git operations rather than a universal local landing transaction. | Keep cleanup separate from integration. Unknown state must not count as an empty diff. |
| Codex app | Worktrees start detached. Users can create a branch for commits and a PR, or hand the chat and work back to Local. Managed worktree cleanup saves a restorable snapshot. | A branch and an apply-to-project operation serve different purposes. Preserve work before cleanup. |
| Conductor | Each workspace has a branch. Its documented flow reviews the diff, checks readiness, creates a PR, merges, then archives. | Review, merge, and archive are separate steps. A green run does not itself land work. |
| Vibe Kanban | Its documented flow commits work and opens PRs. The workspace action named Merge brings the target branch into the working branch. It does not land the task into the target. | Say which direction an action moves work. Avoid a generic Merge button. |
| T3 Code | Worktree branches remain after cleanup. Merge-based cleanup checks inclusion in the remote default branch, with special handling for squash and rebase PR merges. | Retaining a branch is different from proving that its files reached the project. |
| MonoCode | Its remote-access docs expose staging, commits, pushing, and PR creation through normal Git views. A non-Git folder has an empty Changes view. | Do not imply Git review exists for ordinary folders unless Newjen records its own baseline. |

Sources checked for this investigation are [Claude worktrees](https://code.claude.com/docs/en/worktrees), [Codex worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees), [Conductor worktrees](https://www.conductor.build/docs/concepts/git-worktrees), [Conductor review and merge](https://www.conductor.build/docs/guides/review-and-merge), [Vibe Kanban Git operations at 7c3807084daa](https://github.com/BloopAI/vibe-kanban/blob/7c3807084daa/docs/workspaces/git-operations.mdx), [T3 project settings at 9c0fb54e8cb3](https://github.com/pingdotgg/t3code/blob/9c0fb54e8cb3/docs/user/project-settings.md), and [MonoCode remote access at b9ab5065d341](https://github.com/hardbeat920/monocode/blob/b9ab5065d341/docs/remote-access.md).

The earlier [isolation survey](isolation.md) also covers Paseo, Synara, and Zeron. That survey establishes worktree isolation, not a verified special landing algorithm for each product. This memo does not infer such an algorithm from their isolation choice.

## Three options

| Option | What the user gets | What maintainers inherit | Main cost |
| --- | --- | --- | --- |
| A. Native Git snapshots and transfer. | The project stays Git-only. Copy commits can be imported. The thread's net edits can be applied without committing them. | A Git baseline, checkpoint, diff, and transfer implementation beside the existing jj implementation. | More core implementation than requiring jj everywhere. Dirty starts need a saved file snapshot, not just a commit ID. |
| B. jj only inside the Rift copy. Selected. | The original stays Git-only. Newjen snapshots with jj and exports ordinary Git commits or patches. | More reuse of slice 4's jj machinery for isolated threads. | The copy gets `.jj`, later jj operations detach Git HEAD, and Git staging distinctions are lost. Project-folder threads still need another solution. |
| C. Review and export only for the first release. | Git-only and non-repo threads can keep working. Newjen exposes the copy and a patch where a baseline exists. Users integrate manually. | A narrower release boundary. No claim of automatic landing outside supported jj repos. | Users leave Newjen to finish integration. Patch review still requires an accurate baseline. |

Option A separates the snapshot engine from the final action. A branch carries history. A patch carries the net file changes. They are complementary, not competing substitutes for a baseline.

Option C was considered but not selected. An individual unsupported layout may still need a disabled action with an explanation. Never treat an unknown baseline as an empty diff.

## The baseline must include the actual starting files

Use three states when reasoning about a thread:

- `H` is the Git commit at thread creation, if one exists.
- `B` is the exact in-scope file snapshot before the first agent edit.
- `T` is the reviewed result snapshot.

For a Rift thread, capture `B` from the completed copy before starting the agent. A source snapshot taken before copying may disagree with the files actually copied. Rift setup hooks run before creation returns. Capturing afterward treats their edits as setup, not thread work. The UI must disclose that boundary. Including hook edits in review would require an earlier snapshot inside the copy, which is not tested here.

Review `B` to `T`. Comparing `H` to `T` also shows the user's pre-existing staged, unstaged, and untracked work. `H` alone is a valid baseline only when the relevant starting files match it.

The scratch tests proved this with a staged file, an unstaged file, and an untracked file. A synthetic copy baseline followed by an agent change isolated the agent change. Fetching the entire resulting ancestry also imported the synthetic baseline. A later merge of that ancestry would include the user's inherited edits.

Never label the full imported ancestry as exclusively agent-authored. With a dirty start, default to applying the `B` to `T` delta. A branch containing private baseline commits can remain a recovery artifact, but it is not automatically suitable for a PR.

For native Git, a temporary index can capture `B` without changing the user's real index. The ordinary-file probe used `GIT_INDEX_FILE`, `git read-tree`, `git add -A`, `git write-tree`, and `git commit-tree`, then pinned the snapshot under a private ref. A reviewer proved that seeding only from `HEAD` omits newly staged files covered by ignore rules. A follow-up probe seeded the temporary index from the real index instead. `git add -A` then preserved those tracked paths, and the resulting diff included their edits. Preserve the real index's tracked-path membership when constructing both snapshots. The tested correction uses an ordinary index. Split indexes and unresolved index entries still need explicit handling. Keeping index bytes unchanged does not prove that the file snapshot is complete.

`commit-tree` creates a snapshot commit without running normal commit hooks or signing a user commit. Supply an explicit technical author and committer identity for app-owned snapshots. A probe reproduced failure under `user.useConfigOnly=true` with no configured identity, then success with `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME`, and `GIT_COMMITTER_EMAIL` set for the snapshot command. Do not change the user's global identity. This distinction must be explicit. Final user-facing commits must follow the project's chosen checks, identity, and signing policy. Do not disable those silently.

Checkpoint snapshots must be pinned before returning success. Retain `B` and the reviewed `T` across restarts. If a thread continues after review, invalidate the old approval. A second apply must cover only work after the last successful apply or report that the same result is already applied. Never reapply the original full delta blindly.

## What jj can and cannot hide

### Initialize jj in the original Git project

`jj git init --colocate` reuses `.git` and adds `.jj`. A supplemental scratch probe also observed a rewritten `.git/index`, new Git objects, and private `refs/jj/keep` refs at initialization. The branch remained attached and the staged path remained staged at that point. It does not clone the repository or require a network operation. It is still a change to the user's repository setup.

The initial ordinary-file fixture showed jj 0.46.0 capturing tracked edits and small untracked files while excluding ignored files. This is not a completeness guarantee. Its default `snapshot.max-new-file-size` is 1 MiB for new files. A later probe produced a warning and exit status zero while omitting a 2 MiB untracked file. Newjen must set a deliberate snapshot-size policy or show each exclusion and withhold a claim that all in-scope work is reviewed or transferred. Raising the cap explicitly included the fixture file. This limit does not mean all modifications to already tracked large files are omitted. After `jj new`, Git HEAD was detached and the copied staging distinction was gone. This is a meaningful workflow change for a Git user, even though committed history remains Git-compatible.

Do not choose this automatically. An explicit future Convert to jj action could offer it after explaining the consequences.

### Initialize jj only in an independent Rift copy

This worked in the scratch test. Running jj inside a copy with a real, independent `.git` directory left the original's files and metadata byte-for-byte unchanged. The copy acquired `.jj`. A jj bookmark became a normal Git branch that the original could fetch without installing jj metadata there.

Before conversion, record Git's tracked-path membership, including newly staged ignored files. jj initialization alone omitted such a file in the follow-up probe. `jj file track --include-ignored` on that exact omitted path included it. Restore that membership in the copy before finalizing `B`. Do not broadly force-track ignored files.

Record the complete initial jj snapshot as `B`, then start a child change before the agent runs. Otherwise inherited dirty files become part of the reviewed work. Snapshot with the chosen lifecycle rules. Read-only UI queries still need `--ignore-working-copy`.

This is the selected approach for harness-managed copies. In agent-managed mode, adding jj and periodically snapshotting it contradicts the promise that Newjen does not write version control on the agent's behalf. Agent-managed Git threads need a saved baseline for review, but Newjen should not move their branches, clear their index, or invent final commits.

Do not use `jj diff --git` alone as the transfer artifact. A supplemental probe created a new binary file in a non-colocated jj copy. jj emitted a descriptive `Binary files ... differ` record, and `git apply --check` rejected it. The initial native Git diff against the copy's backing store restored the exact binary bytes in a non-repo destination under a clean configuration. A later review proved that porcelain diff is unsafe under user diff configuration. Use the hardened `diff-tree` recipe below for transfer. For a colocated copy, use its `.git` store. The non-colocated probe used `.jj/repo/store/git` with jj 0.46.0. That path was only an experimental detail. Use the documented `jj --ignore-working-copy git root` to discover it in the core adapter, as verified in the later project-mode probe. A direct file transaction from snapshot contents is another option. jj snapshots do not require using jj's display diff for transfer.

jj's documented compatibility gaps include Git attributes, hooks, submodules, sparse checkouts, partial clones, and unfinished Git operations. The scratch tests cover ordinary files, not those configurations. Read the [jj compatibility documentation](https://docs.jj-vcs.dev/latest/git-compatibility/) before expanding support.

### Point a separate jj workspace at the original Git repository

`jj git init --git-repo=<original>` is not an independent backing store. In a scratch test, creating a bookmark and running `jj git export` wrote the branch into the original Git repository. A different working directory does not imply isolated metadata.

Do not use this to claim that Newjen leaves the original repository unchanged. Pointing it at the copy's own independent Git store is a different case.

### Ordinary folders

`jj git init --no-colocate` inside a scratch copy created private `.jj` metadata and no top-level `.git`. jj captured ordinary files and honored the test's `.gitignore`. It can supply snapshots for a folder without modifying the original folder.

Exclude Newjen and Rift metadata from file snapshots and patches. In particular, the source and copy have different `.rift` marker contents. A non-repo folder has no `.git/info/exclude` to hide that marker.

That proves a snapshot mechanism, not safe automatic landing. It also does not establish that arbitrary folders have suitable ignore rules. A folder containing caches, credentials, sockets, or large files needs a deliberate inclusion policy before automatic snapshotting.

## Native Git transfer

### Commit in the copy and fetch a named branch

For a clean-start copy, the test created a branch, committed the result, and fetched it into a new branch in the original. The import preserved the commit ID. It left the original HEAD, index, working files, and repository config unchanged. Fetch did write the expected Git objects, ref, and fetch metadata.

The tested command shape was:

```sh
git -C "$source" fetch --no-tags "$copy" \
	refs/heads/newjen/thread:refs/heads/newjen/imported
```

A fetched branch contains committed files only. Uncommitted and ignored files remain in the copy. Importing a branch alone must not authorize deleting the copy.

This copy-path recipe applies to ordinary Git or colocated jj copies. A non-colocated jj copy needs an explicit bookmark export and its discovered backing Git-store path instead. The later [project source-control design](project-source-control.md#rift-and-shared-folder-behavior) records the tested sequence and agent-managed limitation.

No persistent remote is required. Use a validated thread-specific destination ref and refuse an unexpected existing value. Do not force-update a user's branch. Verify the fetched commit equals the immutable reviewed result before integration.

A clean, unchanged original fast-forwarded successfully with `git merge --ff-only`. A diverged original refused fast-forward. `git merge --squash` combined non-overlapping work in the divergence test without creating a commit, but staged the result. These are separate product choices. Squash does not preserve the copy's individual commits.

A production merge or cherry-pick path needs an explicit target branch, a clean target index and worktree, no ongoing merge or rebase, and a conflict policy. Do not automatically stash the user's edits. The experiment does not prove crash recovery or production locking.

### Return a patch instead of merging ancestry

Generate a binary-capable patch between saved trees `B` and `T`. The original porcelain `git diff` recipe is superseded. User text-conversion configuration made it emit a patch that applied successfully but wrote display text instead of the reviewed binary. Other tested diff settings changed prefixes, added color, or suppressed the patch. The following plumbing recipe restored binary and nested-file contents under each tested setting. The explicit apply whitespace flag preserves reviewed trailing spaces:

```sh
git -C "$copy" diff-tree -r -p --binary --full-index \
	--no-ext-diff --no-textconv --no-color \
	--src-prefix=a/ --dst-prefix=b/ "$baseline" "$result" -- > "$patch"
git -C "$source" apply --check --whitespace=nowarn "$patch"
git -C "$source" apply --whitespace=nowarn "$patch"
```

The test preserved a binary file, a symlink, a deletion, a new file, and an executable bit. Applying the jj-generated delta to the matching dirty source also left the original index bytes unchanged. New files arrived untracked, and edits to staged files appeared as additional unstaged changes.

A plain `git diff HEAD` omitted untracked files in the test. Build the result tree explicitly rather than assuming that command contains all work. A normal diff patch also does not carry commit authors or messages. `git format-patch` with `git am` is a different, commit-producing workflow.

Ordinary `git apply` refused a conflicting multi-file patch without changing either file in the test. The [Git apply manual](https://git-scm.com/docs/git-apply) documents that normal failure behavior. `--3way` can instead update the index and leave conflict markers. It needs the baseline blobs locally. Do not silently retry with it, `--reject`, or an overwrite mode.

Always specify a whitespace policy that preserves the reviewed bytes for both check and apply. A reviewer probe reproduced Git silently removing reviewed trailing spaces under `apply.whitespace=fix`. `--whitespace=nowarn` preserved them. Verify the resulting touched files against `T`, including content, type, and mode, before recording success.

A successful `--check` is not a lock. Revalidate the reviewed result, project identity, target branch, index, and touched file versions at apply time. Pause Newjen writers for the whole project during the transaction. External editors can still race, so durable recovery remains necessary.

A clean textual application is also not proof that the result still makes sense. For the MVP, require touched paths to match their reviewed baseline content, type, and mode. Treat an unexpected existing destination for an added or renamed file as a conflict. If a touched path changed, stop and refresh the review instead of combining it silently. Unrelated project files may change.

This conservative rule sacrifices automatic combination of independent edits in the same file. It gives users a clear result without a conflict editor in the MVP. Applying to a different source branch should require a new target review.

## Project-folder threads already changed the project

There is nothing to fetch when a thread works in the original folder. Its changes are already there. Use **Review changes** and **Keep changes** rather than suggesting that Land will transfer files.

Capture `B` before the first edit with an app-owned snapshot outside the user's repository. A temporary index by itself is insufficient. A probe stored a baseline under `refs/newjen/baseline` in the project and sent it to a local remote with `git push --mirror`. A snapshot branch is also eligible for `push --all`. A private-looking ref name is not an isolation boundary.

For private project-folder checkpoints, use an app-owned Git object store and index, with explicit `GIT_DIR`, `GIT_WORK_TREE`, and `GIT_INDEX_FILE` command environment. The probe captured the fixture files while leaving the project's `.git` file set and contents unchanged. Preserve the project's tracked-path membership and ignore policy explicitly. The external store does not inherit the project's `.git/info/exclude`, including Rift's marker exclusion. The initial empty-store scan must hash its selected files. Snapshot identity and objects must belong to that store. Do not assume that copying an index or adding object alternates proves independent, durable storage. Only deliberately selected and reviewed refs should enter the import or push workflow. Intentional checkpoints in ordinary project history must not be described as private.

Passive Git UI reads also need a defined command policy. In the probe, `git diff` and `git diff --quiet` rewrote the index even with `GIT_OPTIONAL_LOCKS=0`. The tested `diff-index` and `diff-files` reads did not. Use plumbing observations such as `diff-index`, `diff-files`, `diff-tree`, and `ls-files`, with optional locks disabled. Verify the chosen commands against the supported Git configurations rather than treating every Git read as write-free. For a non-repo folder, a private store or file manifest is a possible design, not a complete tested implementation.

Do not attribute arbitrary `git status` changes to one thread. Other project-folder threads, terminals, and editors can edit the same files. The current creation guard prevents making a copy while a project-folder turn runs. It is not a general guarantee of exclusive ownership over the project.

Without exclusive ownership and a baseline, thread-specific Discard is unsafe. Never implement it as `git reset --hard` or `git clean`. Restore only recorded paths after checking that their current content still matches the thread's reviewed result. Otherwise keep the files and ask the user to resolve the overlap.

In agent-managed mode, review the chosen commits plus remaining file changes. Existing agent-created commits need no fetch in a project-folder thread. Do not silently rewrite or remove them when archiving the conversation.

## Folders without Git, and unsupported Git layouts

`git apply` worked in the scratch non-repo folder without creating `.git`. It still needed every expected old file to exist. Thus patches can return files without converting the project, provided a baseline and a complete, scoped patch exist.

For the first release, implement **Apply to folder** for ordinary folders. Compare each touched destination with `B`, preserve a durable recovery copy, and write the reviewed delta. Pin `B` and `T` outside the disposable copy before changing the destination. Journal which paths have changed so recovery after a partial write can complete or restore the transaction. A conflict leaves the project untouched. A write failure keeps the copy and recovery data available. These are implementation requirements, not behaviors proven by the current probes.

Respect the project's `.gitignore` when selecting files. Exclude internal metadata. Define the policy for other files and disclose skipped paths before claiming that all work has transferred. The probe qualifies ordinary file content, symlinks, deletions, and executable mode. Other filesystem objects require explicit handling or a named limitation.

A folder with no reliable `B` cannot reconstruct its earlier state after the fact. Existing `base: None` copies must remain available for manual comparison.

A Git repository with no commits also has no `HEAD` baseline. Treat it explicitly as an unborn repository. A private snapshot can model its files, but the tested ordinary branch-fetch flow does not automatically solve its first commit.

A copied `.git` file can still point to the original repository's metadata. The scratch linked-worktree copy resolved exactly the same Git directory as its source. Rift copies the pointer file, not the metadata it references. Before running any mutating Git or jj command in a copy, verify that both the Git directory and common directory are independent and inside the copy. Also require the canonical resolved working-tree root to equal the intended copy root. A reviewer probe set absolute `core.worktree` to the source. Both directory checks passed, but Git snapshotted source content and `git restore` in the copy modified the source. Control inherited Git redirection variables too. Preflight unsupported layouts before Rift runs setup hooks or starts agents, then revalidate before snapshot and transfer operations. Check symlinks and object alternates too. Do not assume that every folder named `.git` is an independent directory.

For the MVP, classify linked worktrees, external gitdirs, submodules, sparse checkouts, and shared object stores as unsupported for automatic landing until their independence is established. Keep the copy and explain the limitation. The ordinary-file tests do not qualify Git LFS, attributes and filters, shallow or partial clones, nested repositories, unusual filesystem entries, or extended attributes.

## Seam for slice 4 and the merge pass

Current [`rift.rs`](../../crates/core/src/rift.rs) records `Copy.base` as an optional jj commit ID. The [`copies` store](../../crates/core/src/store.rs) persists it. `None` means no jj baseline was recorded. It does not distinguish Git-only from non-repo, nor prove that no files changed.

Do not put Git commit IDs into that existing jj field and later call jj on them. A future implementation needs an explicit repository kind, snapshot kind, original Git HEAD when present, file baseline `B`, reviewed result `T`, and the last successful apply result. Project-folder threads need a baseline too, although they have no copy row.

Repository detection and file transfer belong in core. The UI should receive capabilities such as review, apply, import branch, and export patch. Unsupported or unknown state needs a reason. Do not make the UI infer support from a missing diff.

Keep archive and discard separate from transfer. Before removing a copy, verify that the reviewed result has a durable destination or obtain explicit discard of what remains. Patches and imported branches do not automatically preserve ignored files. The confirmation must say what is excluded. Never infer successful landing from an empty command output.

All decisions apply equally to Claude and Codex. Landing reads files and version-control state. It must not depend on provider-specific messages or start an agent turn. The providers merge needs no translator-specific landing behavior.

No code or schema change is part of this memo. Slice 4 and the merge pass must connect the selected copy-local jj strategy, baseline storage, and automatic apply. Until those requirements are implemented, the current non-jj rows remain unsupported. That is an implementation gap, not an undecided product direction.

## Evidence and limits

The live scratch tests ran on macOS with Apple Git 2.54.0 and jj 0.46.0. They used ordinary temporary directory copies with independent `.git` directories. This models Rift's resulting file layout. It does not re-test APFS copying or Rift hooks, which slice 3 already covers.

The local rerunnable probe is `/tmp/newjen-git-landing/more.py`, with its companion `probe.py`. Run `python3 /tmp/newjen-git-landing/more.py`. The latest artifact directory is recorded in `/tmp/newjen-git-landing/latest`. The scripts and temporary repositories are intentionally outside the bookmark. The scripts contain no Newjen code. The relevant command shapes and results are recorded here so the memo travels independently.

The probe passed assertions for these behaviors:

- Local branch fetch preserved the reviewed commit and did not change source HEAD, index, files, or config.
- Fast-forward integration preserved binary content, symlinks, and executable mode.
- jj confined to an independent copy did not change the source.
- jj captured inherited dirty files, then cleared the copy's staged distinction after `jj new`.
- A saved baseline isolated the agent delta with both jj and native Git.
- Full ancestry import included the private baseline, unlike applying just its delta.
- Patch transfer handled additions, deletions, binary files, symlinks, and executable mode.
- A supplemental jj probe showed that `jj diff --git` could not transfer a new binary file. Git binary export from the private backing store restored its exact bytes in a non-repo destination.
- A reviewer probe reproduced source writes through absolute `core.worktree` despite independent Git directories.
- A reviewer probe reproduced omission of a newly staged ignored file. Seeding a private index from the real index captured its changes. jj also initially omitted the file, and explicit tracking restored it.
- A supplemental reviewer probe showed that `apply.whitespace=fix` silently changes reviewed bytes. Explicit `--whitespace=nowarn` preserved them.
- Ordinary conflicting apply left the tested files untouched. Three-way apply left conflicts in files and the index.
- Non-repo patch application required the expected old files and did not initialize a repository.
- A copied Git worktree pointer resolved to source metadata.
- An unborn Git repository had no `HEAD` commit.
- Diverged history rejected fast-forward. Squash integrated non-overlapping edits into the index without committing.
- A jj workspace backed by the original Git store exported a branch into that original.
- Non-colocated jj in a non-repo copy produced private snapshots without top-level `.git`.

The late review probe `/tmp/newjen-git-landing/late-landing-review.py` passed 21 assertions. It reproduced textconv corruption, tested deterministic plumbing patches and nested files, proved mirror publication of a nominally private ref, checked an external snapshot store, verified technical snapshot identity, exercised jj's new-file limit, and compared index writes from porcelain and plumbing reads. The artifact location is recorded in `/tmp/newjen-git-landing/latest-late-landing`. Duplicate whitespace and working-tree-redirection findings were already corrected.

These tests establish mechanics. They do not prove an application transaction, cancellation, crash recovery, concurrent-editor safety, or a complete policy for selecting files. No GUI, application build, or live Claude or Codex run was needed.

## Review status

GPT 6.1 Sol found one verified issue with inherited whitespace-fixing configuration. GPT 6 Astra found two verified issues with redirected working trees and staged ignored files. The findings were independently rerun. This memo now records the corrected requirements and tested remedies. These are local corrections after review, not a second panel approval of a production implementation.

Claude Fable 5.1 later completed both this standalone landing review and the broader [project source-control review](project-source-control.md#verification-and-handoff). The landing verdict accepts the architecture with corrections to command shapes, snapshot storage, completeness checks, identity, and observation behavior. Its whitespace and working-tree-redirection findings overlap earlier corrections. The remaining findings were independently reproduced and are recorded here. All requested reviewers have returned. This is completed review plus local corrections, not a second unanimous verdict on revised text or approval of an implemented landing transaction.

The prose-only comment scan found no code comments, commented-out code, or suppressions. It changed no files. The owner's decisions arrived during review and are recorded above. No reviewer approved an implemented landing transaction because this task implements none.

## Outcome and implementation boundaries

The owner selected jj only inside Rift copies for plain Git and required automatic apply for ordinary folders. No top-level product decision from this discussion remains pending.

The finish behavior remains an apply of reviewed edits without an automatic commit. Branch import is separate. Never convert the original project, silently stash its work, or overwrite conflicts.

The next implementation needs these concrete boundaries:

- Keep private project-folder checkpoint objects and refs in app-owned storage, not the project's Git store. Preserve source ignore and tracked-path policies explicitly.
- Generate transfer patches with the hardened plumbing recipe and verify the resulting bytes. Use non-mutating Git observations and explicit snapshot identity.
- Treat omitted or oversized jj files as incomplete review coverage, even when the snapshot command succeeds.
- Capture a durable baseline for new copy and project-folder threads. Do not fabricate one for older `base: None` rows.
- Reject redirected working trees and shared metadata before copy hooks or agents can mutate them. Preserve the original tracked-path membership when snapshotting.
- Use copy-local jj for harness-managed copies. Export binary-capable patches from its Git store or transfer snapshot files directly. Preserve the no-version-control-writes promise in agent-managed mode.
- Define the file-selection policy for ordinary folders and show unsupported or skipped files.
- Validate each touched destination before apply. Pin the reviewed result, maintain recovery data across partial writes, verify the result, and handle repeated apply idempotently.
- Resolve shared project-folder ownership before offering thread-specific discard. Its current snapshot problem is independent of Git versus jj.
- Allocate schema migrations during the merge pass. Other sessions also add store fields.

Implementation remains in slice 4 or a separately assigned follow-up. This session produces only the decision memo and scratch evidence.
