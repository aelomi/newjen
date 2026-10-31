# Decision memo: isolating a thread's work (2026-10-31)

Research for slice 3: a thread can work in its own Rift copy instead of the project folder. It covers how other agent development environments isolate parallel agent work. Sources are each product's docs and the public source of T3 Code (`9c0fb54e8cb3`), Vibe Kanban (`7c3807084daa`), Claude Squad (`ce1ffb4392b0`), Synara (`0cf5ea460d19`), MonoCode (`b9ab5065d341`) and Zeron (`4f55b0e1fe6d`). "Not documented" means the primary docs and source say nothing. The decisions taken from this memo are in [`docs/PLAN.md`](../PLAN.md#thread-lifecycle).

## 1. What they do

| Tool | Default | When chosen | Mechanism / location | Branch | Copies ignored files? |
|---|---|---|---|---|---|
| Claude Code CLI | project folder | `-w` at launch, or `EnterWorktree` mid-session | git worktree, `<repo>/.claude/worktrees/<name>` | `worktree-<name>` from the default branch | only `.worktreeinclude` |
| Claude desktop | project folder (Worktree toggle off) | per session, at creation | git worktree, `.claude/worktrees/` (configurable) | user-set prefix + name | only `.worktreeinclude` |
| Codex app | Local or Worktree (default not documented) | per thread; Hand off switches later | git worktree, `$CODEX_HOME/worktrees` | detached HEAD; "Create branch here" → `codex/…` | only `.worktreeinclude` |
| Codex CLI | project folder | none built in (#12862) | manual `git worktree` | n/a | n/a |
| T3 Code | per-project setting (`root` / `worktree`) | per thread; root→worktree handoff only | git worktree, `~/.t3/…/worktrees/<repo>/<branch-dir>` | `t3/<8 hex>`, renamed by a model later | no (discussion #6905 asks) |
| Conductor | always a worktree | per workspace | git worktree, `~/conductor/workspaces/<repo>/<city>` | new branch, which the agent renames | `.worktreeinclude`-style patterns |
| Vibe Kanban | always a worktree | per workspace | git worktree in system temp dir | `vk/<id>-<slug>` from the target branch | "copy files" field |
| Claude Squad | always a worktree | per session | git worktree, `~/.claude-squad/worktrees/<branch>_<ns>` | `<username>/<session>` | no |
| Cursor Agents Window | local | per agent; "move into worktree" | git worktree, `~/.cursor/worktrees/<repo>/<id>` | not documented | via setup script |
| Cursor cloud agents | cloud VM | per agent | VM cloning the repo | own branch, then a PR | n/a |
| Zed | project folder | per thread, from the title-bar worktree picker | git worktree (location not documented) | detached HEAD | via `create_worktree` task hook |
| Paseo | `--isolation local` or `worktree` (default not documented) | per workspace | git worktree, `$PASEO_HOME/worktrees/<hash>` | set at creation (`--new-branch`, `--base`) | `.worktreeinclude` |
| Synara (T3 fork) | local checkout (quickstart); default set in Settings → New threads | per task | git worktree (location not found) | `synara/*` | not found |
| MonoCode | current checkout | per session (draft workspace) | git worktree, `<repo>-worktrees/` beside the repo | `mc/native-<session>-<ms>` | not found |
| Zeron | project folder (worktree toggle in branch picker) | per session | git worktree, `~/.zeron/worktrees/<repo>/<adj-noun>` | `zeron/<adj-noun>`, renamed from the title | not found |


**Every one uses git worktrees. None uses copy-on-write copies.** Containers or VMs appear only in cloud modes (Codex Cloud, Cursor cloud agents, Conductor Cloud).

- **Default.** The tools where isolation is optional start in the project folder: Claude desktop, Zed, Zeron, Synara, MonoCode, Codex, and T3 per project. Conductor, Vibe Kanban and Claude Squad always isolate.
- **Switching later.** Only Codex (Hand off), T3 (into a worktree, ending the session) and Claude Code (`EnterWorktree`) move a running thread, and Codex and T3 have open bugs about it (Codex #10572, #16570).
- **Ignored files are the main setup cost.** `.worktreeinclude` bugs, reinstalling `node_modules` and a missing `.env` (Claude Code #70466, #83098, #79424; T3 discussion #6905).
- **Ports.** Only Conductor (10 per workspace, `CONDUCTOR_PORT`) and Paseo (`$PASEO_PORT`) give each copy its own.
- **Order.** T3 and Zeron start the agent while setup still runs. Synara, Paseo and Vibe Kanban wait. Synara shows "Preparing worktree" and keeps the prompt for a retry; T3 never falls back to the project folder.
- **Cleanup that deletes work is the most common complaint** (Claude Code #46444, #77268, #96162, #58885; Cursor forum 167129, 167201). Safer designs save first (Zed's WIP commits pinned by a ref, Conductor 0.33.5 saving git state on archive, Codex's snapshot before delete) or refuse and say why (Synara, T3).

## 2. What Newjen does, and why

- **A Rift copy, chosen when the thread is created, and fixed for its life.** `--resume` is tied to the folder, so there is no handoff.
- **`rift create --copy-all`.** The copy brings `.env`, `node_modules` and `target/`, so there is no include list and no reinstall. `postcreate` hooks remain for what embeds absolute paths, such as venvs.
- **The copy starts from the user's real state**, uncommitted and ignored files included. With jj there is no "branch already checked out" error and no detached-HEAD workaround: the copy works on its own new change on a base the main repo no longer edits.
- **The copy is ready before claude starts.** The first message waits behind a "Creating a Rift copy" row that shows the hooks' output; a failure gives the message back with Try again. It never falls back to the project folder.
- **`RIFT_ID` and `RIFT_SOURCE` are set for claude**, so a project can derive ports. Newjen does not hand out port ranges yet.
- **The thread shows a "Rift copy" tag and the copy's path.** Approvals that touch the project folder from a copy already say "Outside the project".
- **Archiving a thread in a copy always asks first**, because slice 3 has no landing. Slice 4 adds Land and Discard, and saving the copy's change into the main repo before removal, as Zed and Conductor do.
