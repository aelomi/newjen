//! What the git button in the review panel offers, and what each choice does.
//!
//! Newjen never commits on its own. The agent may run git itself; these are the one-click
//! actions the user takes, chosen from the repository's state (`GitFacts`) by one pure rule
//! (`plan`) and run as a fixed sequence of steps (`Job::run`).

use std::path::{Path, PathBuf};

use anyhow::Result;
use newjen_core::vcs::{self, Status};
use newjen_core::worktree::{Tools, Worktree};

/// A worktree thread's relation to the branch its work goes back to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Source {
    /// The branch the project folder has checked out, if it has one.
    pub branch: Option<String>,
    /// The worktree's `HEAD` is already part of that branch.
    pub merged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitFacts {
    pub status: Status,
    /// `gh` is installed.
    pub gh: bool,
    /// The branch has commits no remote has.
    pub unpushed: bool,
    /// Set for a worktree thread.
    pub source: Option<Source>,
    /// Turns that have a checkpoint, for the scope menu.
    pub turns: usize,
}

impl GitFacts {
    /// Reads the repository. Runs git, so never on the UI thread.
    pub(super) fn load(
        tools: &Tools,
        thread_id: &str,
        cwd: &Path,
        worktree: Option<&Worktree>,
    ) -> Result<GitFacts> {
        let status = vcs::status(tools, cwd)?;
        let source = worktree
            .filter(|worktree| !worktree.is_legacy())
            .map(|worktree| Source {
                branch: vcs::status(tools, &worktree.source)
                    .ok()
                    .and_then(|status| status.branch),
                merged: vcs::merged_into_source(tools, worktree).unwrap_or(false),
            });
        Ok(GitFacts {
            gh: installed(&vcs::find_gh()),
            unpushed: vcs::unpushed(tools, cwd).unwrap_or(false),
            turns: vcs::turns(tools, cwd, thread_id).unwrap_or(0),
            status,
            source,
        })
    }
}

/// `find_gh` and `find_cli` return the bare program name when nothing is installed.
pub(super) fn installed(path: &Path) -> bool {
    path.components().count() > 1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Commit,
    Push,
    PullRequest,
    Merge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GitAction {
    Commit,
    CommitPush,
    CommitPushPr,
    CommitMerge,
    Push,
    PushPr,
    Merge,
}

impl GitAction {
    pub(super) fn steps(self) -> &'static [Step] {
        use Step::*;
        match self {
            GitAction::Commit => &[Commit],
            GitAction::CommitPush => &[Commit, Push],
            GitAction::CommitPushPr => &[Commit, Push, PullRequest],
            GitAction::CommitMerge => &[Commit, Merge],
            GitAction::Push => &[Push],
            GitAction::PushPr => &[Push, PullRequest],
            GitAction::Merge => &[Merge],
        }
    }

    pub(super) fn commits(self) -> bool {
        self.steps().contains(&Step::Commit)
    }

    /// `target` is the branch a merge goes into.
    pub(super) fn label(self, target: &str) -> String {
        let target = shorten(target);
        match self {
            GitAction::Commit => "Commit".into(),
            GitAction::CommitPush => "Commit & push".into(),
            GitAction::CommitPushPr => "Commit, push & PR".into(),
            GitAction::CommitMerge => format!("Commit & merge into {target}"),
            GitAction::Push => "Push".into(),
            GitAction::PushPr => "Push & create PR".into(),
            GitAction::Merge => format!("Merge into {target}"),
        }
    }

    pub(super) fn icon(self) -> crate::ui::IconName {
        use crate::ui::IconName;
        match self {
            GitAction::Commit => IconName::GitCommitHorizontal,
            GitAction::CommitPush | GitAction::Push => IconName::Upload,
            GitAction::CommitPushPr | GitAction::PushPr => IconName::GitPullRequest,
            GitAction::CommitMerge | GitAction::Merge => IconName::GitMerge,
        }
    }

    /// The notice after the action worked: a title and a detail.
    pub(super) fn done(self, target: &str, pull_request: Option<&str>) -> (&'static str, String) {
        let url = pull_request.unwrap_or_default().to_string();
        match self {
            GitAction::Commit => ("Committed", String::new()),
            GitAction::CommitPush | GitAction::CommitPushPr => ("Committed and pushed", url),
            GitAction::Push | GitAction::PushPr => ("Pushed", url),
            GitAction::CommitMerge => ("Committed and merged", format!("Into {target}")),
            GitAction::Merge => ("Merged", format!("Into {target}")),
        }
    }
}

const LONGEST_BRANCH: usize = 28;

/// A branch name short enough to sit in a button.
fn shorten(name: &str) -> String {
    if name.chars().count() <= LONGEST_BRANCH {
        return name.to_string();
    }
    let head: String = name.chars().take(LONGEST_BRANCH - 1).collect();
    format!("{head}…")
}

impl Step {
    pub(super) fn failed(self) -> &'static str {
        match self {
            Step::Commit => "Commit failed",
            Step::Push => "Push failed",
            Step::PullRequest => "Couldn't open the pull request",
            Step::Merge => "Merge failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Plan {
    /// What the user may do, the one ⌘↵ runs first.
    Actions(Vec<GitAction>),
    /// Nothing to do, and why.
    Idle(&'static str),
}

impl Plan {
    pub(super) fn primary(&self) -> Option<GitAction> {
        match self {
            Plan::Actions(actions) => actions.first().copied(),
            Plan::Idle(_) => None,
        }
    }
}

/// The rules, after T3 Code's git button: uncommitted work offers commits (to a pull request
/// when it can), a branch with unpushed commits offers a push, and a worktree thread also
/// offers a merge back into the branch it came from.
pub(super) fn plan(facts: &GitFacts) -> Plan {
    let status = &facts.status;
    if status.branch.is_none() {
        return Plan::Idle("No branch checked out");
    }
    let pull_request = facts.gh && status.remote && !status.default_branch;
    let target = facts
        .source
        .as_ref()
        .is_some_and(|source| source.branch.is_some());
    let mut actions = Vec::new();
    if status.dirty {
        if status.remote {
            if pull_request {
                actions.push(GitAction::CommitPushPr);
            }
            actions.push(GitAction::CommitPush);
        }
        actions.push(GitAction::Commit);
        if target {
            actions.push(GitAction::CommitMerge);
        }
    } else {
        let pushable = status.remote && (status.ahead > 0 || (!status.upstream && facts.unpushed));
        if pushable {
            if pull_request {
                actions.push(GitAction::PushPr);
            }
            actions.push(GitAction::Push);
        }
        if target && facts.source.as_ref().is_some_and(|source| !source.merged) {
            actions.push(GitAction::Merge);
        }
    }
    if actions.is_empty() {
        Plan::Idle("Up to date")
    } else {
        Plan::Actions(actions)
    }
}

/// Everything an action needs, owned so it can run on a background thread.
pub(super) struct Job {
    pub tools: Tools,
    pub gh: PathBuf,
    pub cwd: PathBuf,
    pub worktree: Option<Worktree>,
    pub title: String,
}

pub(super) struct Failed {
    pub step: Step,
    pub error: anyhow::Error,
}

impl Job {
    /// Runs the action's steps in order and stops at the first that fails. Returns the pull
    /// request's address when one was opened. Its title is the message's first line and its
    /// body the rest; without a message (a push) the thread's title.
    pub(super) fn run(&self, action: GitAction, message: &str) -> Result<Option<String>, Failed> {
        let mut pull_request = None;
        for &step in action.steps() {
            let result = match step {
                Step::Commit => vcs::commit(&self.tools, &self.cwd, message),
                Step::Push => vcs::push(&self.tools, &self.cwd),
                Step::PullRequest => {
                    let (title, body) = split_message(message, &self.title);
                    vcs::create_pr(&self.tools, &self.gh, &self.cwd, &title, &body).map(|url| {
                        pull_request = Some(url).filter(|url| !url.is_empty());
                    })
                }
                Step::Merge => match &self.worktree {
                    Some(worktree) => vcs::merge_worktree(&self.tools, worktree, &self.title),
                    None => Err(anyhow::anyhow!("This thread has no worktree to merge")),
                },
            };
            result.map_err(|error| Failed { step, error })?;
        }
        Ok(pull_request)
    }
}

/// A pull request's title and body from a commit message.
pub(super) fn split_message(message: &str, fallback: &str) -> (String, String) {
    let message = message.trim();
    let (title, body) = message.split_once('\n').unwrap_or((message, ""));
    let title = title.trim();
    (
        if title.is_empty() { fallback } else { title }.to_string(),
        body.trim().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use GitAction::*;

    fn facts(status: Status) -> GitFacts {
        GitFacts {
            status,
            gh: true,
            unpushed: false,
            source: None,
            turns: 0,
        }
    }

    fn on_branch(name: &str) -> Status {
        Status {
            branch: Some(name.into()),
            remote: true,
            ..Status::default()
        }
    }

    fn worktree(facts: GitFacts, merged: bool) -> GitFacts {
        GitFacts {
            source: Some(Source {
                branch: Some("master".into()),
                merged,
            }),
            ..facts
        }
    }

    fn actions(facts: &GitFacts) -> Vec<GitAction> {
        match plan(facts) {
            Plan::Actions(actions) => actions,
            Plan::Idle(reason) => panic!("expected actions, got {reason}"),
        }
    }

    #[test]
    fn uncommitted_work_on_a_feature_branch_commits_pushes_and_opens_a_pull_request() {
        let mut status = on_branch("feature");
        status.dirty = true;
        assert_eq!(actions(&facts(status)), [CommitPushPr, CommitPush, Commit]);
    }

    #[test]
    fn the_default_branch_is_not_offered_a_pull_request() {
        let mut status = on_branch("master");
        status.dirty = true;
        status.default_branch = true;
        assert_eq!(actions(&facts(status)), [CommitPush, Commit]);
    }

    #[test]
    fn without_gh_there_is_no_pull_request() {
        let mut status = on_branch("feature");
        status.dirty = true;
        let facts = GitFacts {
            gh: false,
            ..facts(status)
        };
        assert_eq!(actions(&facts), [CommitPush, Commit]);
    }

    #[test]
    fn without_a_remote_the_only_commit_action_is_commit() {
        let mut status = on_branch("feature");
        status.dirty = true;
        status.remote = false;
        assert_eq!(actions(&facts(status)), [Commit]);
    }

    #[test]
    fn a_worktree_thread_can_also_commit_and_merge() {
        let mut status = on_branch("newjen/abc");
        status.dirty = true;
        status.remote = false;
        assert_eq!(
            actions(&worktree(facts(status), false)),
            [Commit, CommitMerge]
        );
    }

    #[test]
    fn a_clean_branch_ahead_of_its_upstream_pushes() {
        let mut status = on_branch("feature");
        status.upstream = true;
        status.ahead = 2;
        assert_eq!(actions(&facts(status)), [PushPr, Push]);
    }

    #[test]
    fn a_new_branch_with_commits_and_no_upstream_pushes() {
        let status = on_branch("newjen/abc");
        let facts = GitFacts {
            unpushed: true,
            ..facts(status)
        };
        assert_eq!(actions(&facts), [PushPr, Push]);
    }

    #[test]
    fn a_new_branch_with_nothing_of_its_own_has_nothing_to_push() {
        let facts = facts(on_branch("newjen/abc"));
        assert_eq!(plan(&facts), Plan::Idle("Up to date"));
    }

    #[test]
    fn a_clean_unmerged_worktree_merges() {
        let status = on_branch("newjen/abc");
        let facts = worktree(facts(status), false);
        assert_eq!(actions(&facts), [Merge]);
    }

    #[test]
    fn pushing_comes_before_merging_when_both_apply() {
        let mut status = on_branch("newjen/abc");
        status.upstream = true;
        status.ahead = 1;
        let facts = worktree(facts(status), false);
        assert_eq!(actions(&facts), [PushPr, Push, Merge]);
    }

    #[test]
    fn a_merged_clean_worktree_is_up_to_date() {
        let facts = worktree(facts(on_branch("newjen/abc")), true);
        assert_eq!(plan(&facts), Plan::Idle("Up to date"));
    }

    #[test]
    fn a_worktree_whose_source_has_no_branch_cannot_merge() {
        let mut status = on_branch("newjen/abc");
        status.dirty = true;
        status.remote = false;
        let mut facts = worktree(facts(status), false);
        facts.source.as_mut().unwrap().branch = None;
        assert_eq!(actions(&facts), [Commit]);
    }

    #[test]
    fn a_detached_head_offers_nothing() {
        let mut status = on_branch("x");
        status.branch = None;
        status.dirty = true;
        assert_eq!(plan(&facts(status)), Plan::Idle("No branch checked out"));
    }

    #[test]
    fn labels_name_the_branch_a_merge_goes_into() {
        assert_eq!(Merge.label("main"), "Merge into main");
        assert_eq!(CommitMerge.label("main"), "Commit & merge into main");
        assert_eq!(CommitPushPr.label("main"), "Commit, push & PR");
    }

    #[test]
    fn a_very_long_branch_name_does_not_stretch_the_button() {
        let name = "feature/".to_string() + &"x".repeat(80);
        let label = Merge.label(&name);
        assert!(label.chars().count() <= "Merge into ".len() + LONGEST_BRANCH);
        assert!(label.ends_with('…'));
        assert_eq!(
            Merge.label("日本語のブランチ"),
            "Merge into 日本語のブランチ"
        );
    }

    #[test]
    fn only_actions_that_commit_ask_for_a_message() {
        assert!(
            [Commit, CommitPush, CommitPushPr, CommitMerge]
                .iter()
                .all(|action| action.commits())
        );
        assert!(![Push, PushPr, Merge].iter().any(|action| action.commits()));
    }

    #[test]
    fn a_pull_request_takes_the_first_line_as_its_title() {
        assert_eq!(
            split_message("feat: add x\n\nWhy it helps.\n", "thread"),
            ("feat: add x".to_string(), "Why it helps.".to_string())
        );
        assert_eq!(
            split_message("", "thread"),
            ("thread".to_string(), String::new())
        );
    }
}
