# Opening and switching projects

Newjen keeps multiple projects in one window. Switching projects keeps each
project's threads and drafts alive. Opening a project never starts an agent turn.

## Other environments

Checked on 2026-10-31.

- [T3 Code](https://raw.githubusercontent.com/pingdotgg/t3code/main/docs/user/thread-sidebar.md)
  keeps a current project for new threads. On mobile, it offers a project picker
  when none is selected. Its project selection also accounts for connected machines.
- [Codex](https://openai.com/index/introducing-the-codex-app/)
  organizes parallel agent work by project in its desktop app.
- [Conductor](https://www.conductor.build/docs/first-workspace)
  imports an existing local repository through Open project and groups its
  isolated workspaces in a project sidebar.
- [Zed](https://zed.dev/docs/windows-and-projects)
  opens projects in the current window by default. Switching preserves each
  project's threads and layout. It also offers separate windows.

These products support keeping project navigation inside the app. Newjen uses
one window because a switch should preserve a running approval or a half-written
prompt without requiring window management.

## Ownership

A project shell owns the opened project views and selects which one to show.
Each project view continues to own its threads, event pumps, draft fields, and
Rift jobs. Hiding a project changes visibility and unread tracking, not its
working folders or process lifetime.

A shared thread registry was considered. It would require moving thread slots,
draft selection, event listeners, and reply state out of the existing chat view.
Retaining project views gives the MVP the required behavior with fewer changes
to thread lifecycle code.

The shell owns database error reporting, pending copy cleanup, and combined
shutdown. Those jobs span all projects and must not run once per project view.
Hidden views cannot take keyboard focus. Selecting a project restores its
composer or approval dock.

Rift copies the repository root when the selected project is a subfolder.
Copy creation checks busy threads' actual working folders across loaded projects.
This includes Rift folders opened as projects. Sends into an overlapping source
also wait while its copy is being prepared. Selecting another folder must not
bypass the protection against copying unfinished edits.

## Opening folders

The macOS folder picker accepts one directory. A canonical path identifies the
project, so opening it through a symlink selects the same loaded view. Recent
projects come from the existing projects table, ordered by last opened time.
They remain available after restarting Newjen.

Without an explicit command-line folder or `NEWJEN_PROJECT`, Newjen shows a
welcome screen with Open Project and recent projects. It does not treat the
home folder or an inherited current directory as a project. Missing folders
produce a recoverable error without replacing the current project.

## Integration boundaries

- Instance locking can pass a folder to `ProjectView::open_project`. Transport
  between instances belongs to the instance-lock work.
- Project selection does not choose a provider. The providers work must connect
  its launch defaults where a project's saved threads and draft configuration
  are constructed. Every saved thread must retain its provider and working folder.
- Notifications and the approvals inbox must enumerate every loaded project,
  including hidden ones. They belong to the parallel-threads work.
