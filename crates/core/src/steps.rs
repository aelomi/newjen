//! Plain-English presentation data derived from a transcript, the same for every provider.
//!
//! - [`StepLabel`]: a short headline and an optional muted detail for each tool call.
//! - [`WorkBlock`]: one turn's tool calls grouped, with a state and the time it took.
//! - [`activity_line`]: what a thread is doing right now, for the thread list.
//! - [`Placement`]: where in the thread list a thread belongs.
//!
//! All of it is computed from the items the transcript already holds, so nothing is saved and
//! old threads gain it too. Labels only restate what the event says: a command with no
//! description reads "Ran cargo test", never a guessed purpose.

use std::ops::Range;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::agent::{ApprovalRequest, ToolDescriptor};
use crate::store::ListState;
use crate::thread::{Thread, Waiting};
use crate::transcript::{Item, Status, ToolState, Transcript};

/// What kind of work a step is, for choosing an icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Read,
    Edit,
    Create,
    Delete,
    Run,
    Search,
    Web,
    Mcp,
    Agent,
    Other,
}

/// A step's headline, and a muted detail line when there is more to say (a path, the raw
/// command).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLabel {
    pub kind: StepKind,
    pub headline: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Verb {
    bare: &'static str,
    present: &'static str,
    past: &'static str,
}
const READ: Verb = Verb {
    bare: "Read",
    present: "Reading",
    past: "Read",
};
const EDIT: Verb = Verb {
    bare: "Edit",
    present: "Editing",
    past: "Edited",
};
const WRITE: Verb = Verb {
    bare: "Write",
    present: "Writing",
    past: "Wrote",
};
const CREATE: Verb = Verb {
    bare: "Create",
    present: "Creating",
    past: "Created",
};
const DELETE: Verb = Verb {
    bare: "Delete",
    present: "Deleting",
    past: "Deleted",
};
const CHANGE: Verb = Verb {
    bare: "Change",
    present: "Changing",
    past: "Changed",
};
const RUN: Verb = Verb {
    bare: "Run",
    present: "Running",
    past: "Ran",
};
const SEARCH: Verb = Verb {
    bare: "Search for",
    present: "Searching for",
    past: "Searched for",
};
const SEARCH_WEB: Verb = Verb {
    bare: "Search the web for",
    present: "Searching the web for",
    past: "Searched the web for",
};
const FETCH: Verb = Verb {
    bare: "Fetch",
    present: "Fetching",
    past: "Fetched",
};
const USE: Verb = Verb {
    bare: "Use",
    present: "Using",
    past: "Used",
};
const VIEW: Verb = Verb {
    bare: "View",
    present: "Viewing",
    past: "Viewed",
};
const UPDATE: Verb = Verb {
    bare: "Update",
    present: "Updating",
    past: "Updated",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tense {
    /// Not run (yet): waiting for an answer, denied, or expired.
    Bare,
    Present,
    Past,
}
impl Tense {
    fn of(state: ToolState) -> Tense {
        match state {
            ToolState::Running => Tense::Present,
            ToolState::Done | ToolState::Failed => Tense::Past,
            ToolState::Waiting | ToolState::Denied | ToolState::Expired => Tense::Bare,
        }
    }
}
impl Verb {
    fn say(self, tense: Tense) -> &'static str {
        match tense {
            Tense::Bare => self.bare,
            Tense::Present => self.present,
            Tense::Past => self.past,
        }
    }
}

/// A tool call as either adapter reports it, borrowed.
struct Call<'a> {
    name: &'a str,
    display_name: &'a str,
    summary: &'a str,
    input: &'a Value,
    output: Option<&'a str>,
    cwd: Option<&'a Path>,
}
impl<'a> Call<'a> {
    fn of_item(item: &'a Item, cwd: Option<&'a Path>) -> Option<Call<'a>> {
        match item {
            Item::Tool {
                name,
                display_name,
                summary,
                input,
                output,
                ..
            } => Some(Call {
                name,
                display_name,
                summary,
                input,
                output: output.as_deref(),
                cwd,
            }),
            _ => None,
        }
    }
    fn of_descriptor(tool: &'a ToolDescriptor, cwd: Option<&'a Path>) -> Call<'a> {
        Call {
            name: &tool.name,
            display_name: &tool.display_name,
            summary: &tool.summary,
            input: &tool.arguments,
            output: None,
            cwd,
        }
    }
    fn field(&self, name: &str) -> Option<&'a str> {
        self.input
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }
}

/// What one call does, before the tense is known.
#[derive(Debug, Clone)]
struct Described {
    kind: StepKind,
    verb: Verb,
    /// The files a file step covers, as the paths the event named.
    files: Vec<String>,
    /// The words after the verb, when the step is not about files.
    object: String,
    /// A headline taken as written (a Bash description): it keeps its own tense.
    fixed: Option<String>,
    detail: Option<String>,
    /// The step names something specific. False for a tool Newjen has no wording for.
    known: bool,
}
impl Described {
    fn about(kind: StepKind, verb: Verb, object: impl Into<String>) -> Self {
        Described {
            kind,
            verb,
            files: Vec::new(),
            object: object.into(),
            fixed: None,
            detail: None,
            known: true,
        }
    }
    fn files(kind: StepKind, verb: Verb, files: Vec<String>) -> Self {
        Described {
            files,
            ..Described::about(kind, verb, "")
        }
    }
    fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }
    fn fixed(kind: StepKind, headline: String, detail: Option<String>) -> Self {
        Described {
            fixed: Some(headline),
            ..Described::about(kind, RUN, "").with_detail(detail)
        }
    }
    fn unknown(call: &Call) -> Self {
        Described {
            fixed: Some(call.display_name.to_string()),
            known: false,
            ..Described::about(StepKind::Other, USE, "")
                .with_detail(Some(call.summary.to_string()).filter(|text| !text.is_empty()))
        }
    }
}

const HEADLINE_CHARS: usize = 80;
const DETAIL_CHARS: usize = 200;

fn clip(text: &str, limit: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let mut chars = line.chars();
    let mut clipped: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() || text.trim().lines().nth(1).is_some() {
        clipped.push('…');
    }
    clipped
}

fn one_line(text: &str, limit: usize) -> String {
    let flat = text.trim().replace('\n', " ↵ ");
    let mut chars = flat.chars();
    let mut clipped: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        clipped.push('…');
    }
    clipped
}

fn base_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn relative(path: &str, cwd: Option<&Path>) -> String {
    let Some(cwd) = cwd else {
        return path.to_string();
    };
    match Path::new(path).strip_prefix(cwd) {
        Ok(inside) if !inside.as_os_str().is_empty() => inside.display().to_string(),
        _ => path.to_string(),
    }
}

fn describe(call: &Call) -> Described {
    let cwd = call.cwd;
    let file_step = |kind, verb, field: &str| match call.field(field) {
        Some(path) => Described::files(kind, verb, vec![path.to_string()]),
        None => Described::about(kind, verb, "a file"),
    };
    match call.name {
        "Read" => file_step(StepKind::Read, READ, "file_path"),
        "Edit" | "MultiEdit" => file_step(StepKind::Edit, EDIT, "file_path"),
        "NotebookEdit" => match call.field("notebook_path").or(call.field("file_path")) {
            Some(path) => Described::files(StepKind::Edit, EDIT, vec![path.to_string()]),
            None => Described::about(StepKind::Edit, EDIT, "a notebook"),
        },
        "Write" => {
            let created = call
                .output
                .is_some_and(|output| output.starts_with("File created successfully"));
            if created {
                file_step(StepKind::Create, CREATE, "file_path")
            } else {
                file_step(StepKind::Edit, WRITE, "file_path")
            }
        }
        "Bash" | "Command" => command_step(call),
        "Grep" | "Glob" => {
            let detail = call
                .field("path")
                .map(|path| format!("in {}", relative(path, cwd)));
            match call.field("pattern") {
                Some(pattern) => {
                    Described::about(StepKind::Search, SEARCH, clip(pattern, HEADLINE_CHARS))
                        .with_detail(detail)
                }
                None => Described::about(StepKind::Search, SEARCH, "files"),
            }
        }
        "WebFetch" => match call.field("url") {
            Some(url) => Described::about(StepKind::Web, FETCH, host(url))
                .with_detail(Some(one_line(url, DETAIL_CHARS))),
            None => Described::about(StepKind::Web, FETCH, "a page"),
        },
        "WebSearch" | "Web search" => {
            let query = call
                .field("query")
                .or(Some(call.summary).filter(|text| !text.is_empty()));
            match query {
                Some(query) => {
                    Described::about(StepKind::Web, SEARCH_WEB, clip(query, HEADLINE_CHARS))
                }
                None => Described {
                    known: false,
                    ..Described::about(StepKind::Web, SEARCH_WEB, "the web")
                },
            }
        }
        "Task" | "Agent" => match call.field("description") {
            Some(description) => Described::fixed(
                StepKind::Agent,
                clip(description, HEADLINE_CHARS),
                call.field("subagent_type").map(str::to_string),
            ),
            None => Described::unknown(call),
        },
        "TodoWrite" => Described::about(StepKind::Other, UPDATE, "the to-do list"),
        "File change" => file_change(call),
        "View image" => match Some(call.summary).filter(|path| !path.is_empty()) {
            Some(path) => Described::files(StepKind::Read, VIEW, vec![path.to_string()]),
            None => Described::about(StepKind::Read, VIEW, "an image"),
        },
        "Tool" => {
            let tool = Some(call.summary).filter(|text| !text.is_empty() && *text != "Tool call");
            Described::about(
                StepKind::Mcp,
                USE,
                tool.map_or("a tool".to_string(), |tool| clip(tool, HEADLINE_CHARS)),
            )
        }
        name if name.starts_with("mcp__") => match name[5..].split_once("__") {
            Some((server, tool)) => mcp_step(server, tool, call),
            None => Described::unknown(call),
        },
        name if name.contains(" · ") => match name.split_once(" · ") {
            Some((server, tool)) => mcp_step(server, tool, call),
            None => Described::unknown(call),
        },
        _ => Described::unknown(call),
    }
}

fn mcp_step(server: &str, tool: &str, call: &Call) -> Described {
    let detail = Some(call.summary)
        .filter(|text| !text.is_empty() && *text != "Tool call")
        .map(|text| one_line(text, DETAIL_CHARS));
    Described::about(StepKind::Mcp, USE, format!("{server} {tool}")).with_detail(detail)
}

fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if host.is_empty() {
        clip(url, HEADLINE_CHARS)
    } else {
        host.to_string()
    }
}

fn file_change(call: &Call) -> Described {
    let changes = call.input.as_array();
    let mut files = Vec::new();
    let mut kinds: Vec<&str> = Vec::new();
    for change in changes.into_iter().flatten() {
        if let Some(path) = change["path"].as_str() {
            files.push(path.to_string());
            kinds.push(change["kind"]["type"].as_str().unwrap_or(""));
        }
    }
    if files.is_empty() {
        return Described::about(StepKind::Edit, CHANGE, "files");
    }
    let same = kinds.windows(2).all(|pair| pair[0] == pair[1]);
    let (kind, verb) = match (same, kinds[0]) {
        (true, "add") => (StepKind::Create, CREATE),
        (true, "delete") => (StepKind::Delete, DELETE),
        (true, "update") => (StepKind::Edit, EDIT),
        _ => (StepKind::Edit, CHANGE),
    };
    Described::files(kind, verb, files)
}

fn command_step(call: &Call) -> Described {
    let command = call.field("command");
    let inner = command.map(unwrap_shell);
    let raw = inner.map(|command| one_line(command, DETAIL_CHARS));
    if let Some(description) = call.field("description") {
        return Described::fixed(StepKind::Run, clip(description, HEADLINE_CHARS), raw);
    }
    match inner.map(command_words).filter(|words| !words.is_empty()) {
        Some(words) => {
            let detail = raw.filter(|raw| *raw != words);
            Described::about(StepKind::Run, RUN, words).with_detail(detail)
        }
        None => Described::about(StepKind::Run, RUN, "a command").with_detail(raw),
    }
}

/// The command a shell wrapper such as `/bin/zsh -lc 'cargo test'` runs; other text as it is.
fn unwrap_shell(command: &str) -> &str {
    let command = command.trim();
    let Some((program, mut rest)) = command.split_once(' ') else {
        return command;
    };
    if !matches!(base_name(program).as_str(), "sh" | "bash" | "zsh") {
        return command;
    }
    loop {
        rest = rest.trim_start();
        let Some((flag, tail)) = rest.split_once(' ') else {
            return command;
        };
        if !flag.starts_with('-') {
            return command;
        }
        rest = tail.trim_start();
        if !flag.starts_with("--") && flag.contains('c') {
            break;
        }
    }
    for quote in ['\'', '"'] {
        if let Some(inner) = rest
            .strip_prefix(quote)
            .and_then(|inner| inner.strip_suffix(quote))
        {
            return inner.trim();
        }
    }
    rest
}

/// A plain word of a command line: not a flag, no quotes, redirects or variables.
fn is_word(token: &str) -> bool {
    token.chars().any(|c| c.is_ascii_alphanumeric())
        && !token.starts_with('-')
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '~'))
}

/// The first words of a command, as written: its program and up to two plain words after it
/// ("cargo test", "touch notes.txt"), stopping at the first flag. Leading `cd`, `export` and `VAR=value` parts are skipped.
fn command_words(command: &str) -> String {
    let segments: Vec<&str> = command
        .split(['\n', ';', '|', '&'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect();
    let skipped = |segment: &str| {
        matches!(
            segment.split_whitespace().next(),
            Some("cd" | "export" | "set" | "source")
        )
    };
    let Some(segment) = segments
        .iter()
        .find(|segment| !skipped(segment))
        .or(segments.first())
    else {
        return String::new();
    };
    let mut tokens = segment.split_whitespace().skip_while(|token| {
        matches!(*token, "sudo" | "env" | "time" | "nohup")
            || token.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
    });
    let Some(program) = tokens.next() else {
        return String::new();
    };
    let mut words = vec![base_name(program)];
    words.extend(
        tokens
            .take_while(|token| is_word(token))
            .take(2)
            .map(str::to_string),
    );
    words.join(" ")
}

/// A group of calls that read as one step, described for `tense`.
fn label_of(group: &[Described], tense: Tense, cwd: Option<&Path>) -> StepLabel {
    let first = &group[0];
    if let Some(fixed) = &first.fixed {
        return StepLabel {
            kind: first.kind,
            headline: fixed.clone(),
            detail: first.detail.clone(),
        };
    }
    let mut files: Vec<&String> = Vec::new();
    for described in group {
        for file in &described.files {
            if !files.contains(&file) {
                files.push(file);
            }
        }
    }
    let verb = first.verb.say(tense);
    match files.len() {
        0 => StepLabel {
            kind: first.kind,
            headline: format!("{verb} {}", first.object),
            detail: first.detail.clone(),
        },
        1 => {
            let path = files[0];
            let name = base_name(path);
            let shown = relative(path, cwd);
            StepLabel {
                kind: first.kind,
                headline: format!("{verb} {name}"),
                detail: Some(shown).filter(|shown| *shown != name),
            }
        }
        count => {
            let names: Vec<String> = files.iter().map(|path| base_name(path)).collect();
            let unique = names
                .iter()
                .enumerate()
                .all(|(ix, name)| !names[..ix].contains(name));
            let shown: Vec<String> = if unique {
                names
            } else {
                files.iter().map(|path| relative(path, cwd)).collect()
            };
            StepLabel {
                kind: first.kind,
                headline: format!("{verb} {count} files"),
                detail: Some(shown.join(", ")),
            }
        }
    }
}

/// The label for one tool call, in the tense its state calls for: "Reading thread.rs" while
/// it runs, "Read thread.rs" when done, "Read thread.rs" in the bare form while it waits for
/// an answer, was denied or expired. `None` for an item that is not a tool call. Paths inside
/// `cwd` are shown relative to it.
pub fn step_label(item: &Item, cwd: Option<&Path>) -> Option<StepLabel> {
    let Item::Tool { state, .. } = item else {
        return None;
    };
    let call = Call::of_item(item, cwd)?;
    Some(label_of(&[describe(&call)], Tense::of(*state), cwd))
}

/// The approval's question in a few words: "Delete 3 files?", "Run cargo test?". `None` for a
/// tool Newjen has no wording for; show "Needs approval · <tool>" then.
pub fn approval_question(request: &ApprovalRequest, cwd: Option<&Path>) -> Option<String> {
    if !request.headline().is_empty() {
        return Some(format!("{}?", request.headline()));
    }
    let call = Call::of_descriptor(&request.view().tool, cwd);
    let described = describe(&call);
    if !described.known {
        return None;
    }
    let label = label_of(&[described], Tense::Bare, cwd);
    Some(format!("{}?", label.headline))
}

/// The line for an approval in the thread list: the question, or "Needs approval · Bash".
pub fn approval_line(request: &ApprovalRequest, cwd: Option<&Path>) -> String {
    approval_question(request, cwd).unwrap_or_else(|| {
        format!(
            "Needs approval · {}",
            request.view().tool.display_name.as_str()
        )
    })
}

/// One row of a work block: a tool call, or consecutive calls that read as one ("Read 3
/// files").
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// The transcript items (`Item::id`) this step covers, in order.
    pub item_ids: Vec<u64>,
    /// While any call waits for an answer the step waits; else while any runs it runs.
    pub state: ToolState,
    pub label: StepLabel,
    /// How long its calls took, when all of them have reported it.
    pub took: Option<Duration>,
}

impl Step {
    /// How many tool calls the step covers.
    pub fn calls(&self) -> usize {
        self.item_ids.len()
    }
}

/// How a turn's work is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkState {
    Working,
    WaitingForYou,
    Done,
    Stopped,
    Failed,
}

/// One turn's work: its tool calls grouped into steps.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkBlock {
    /// The turn's items in `Transcript::items`, up to and including the item that ended it.
    pub turn: Range<usize>,
    /// From the turn's first tool call to its last, when it made any. Replies between them
    /// are inside this range.
    pub tools: Option<Range<usize>>,
    pub steps: Vec<Step>,
    /// The tool calls in the turn, merged steps counted once per call ("7 steps").
    pub step_count: usize,
    pub state: WorkState,
    /// The turn's length as the CLI reported it. For the turn still running, the elapsed time
    /// passed to [`work_blocks`].
    pub elapsed: Option<Duration>,
}

impl WorkBlock {
    pub fn is_live(&self) -> bool {
        matches!(self.state, WorkState::Working | WorkState::WaitingForYou)
    }

    /// The collapsed line for a finished turn: "Worked for 4m 3s · 7 steps", "Stopped after
    /// 12s · 2 steps", "Failed after 3s · 1 step". `format_elapsed` writes whole seconds the
    /// way the app writes them ("4m 3s"). `None` while the turn runs, or with nothing to say.
    pub fn summary(&self, format_elapsed: impl Fn(u64) -> String) -> Option<String> {
        let (lead, after) = match self.state {
            WorkState::Done => ("Worked", "for"),
            WorkState::Stopped => ("Stopped", "after"),
            WorkState::Failed => ("Failed", "after"),
            WorkState::Working | WorkState::WaitingForYou => return None,
        };
        let mut line = match self.elapsed {
            Some(elapsed) => format!("{lead} {after} {}", format_elapsed(elapsed.as_secs())),
            None => lead.to_string(),
        };
        match self.step_count {
            0 if self.elapsed.is_none() => return None,
            0 => {}
            1 => line.push_str(" · 1 step"),
            count => line.push_str(&format!(" · {count} steps")),
        }
        Some(line)
    }
}

fn mergeable(kind: StepKind) -> bool {
    matches!(
        kind,
        StepKind::Read | StepKind::Edit | StepKind::Create | StepKind::Delete
    )
}
fn can_merge_state(state: ToolState) -> bool {
    matches!(
        state,
        ToolState::Done | ToolState::Running | ToolState::Waiting
    )
}

struct Group {
    ids: Vec<u64>,
    described: Vec<Described>,
    states: Vec<ToolState>,
    took: Vec<Option<Duration>>,
}
impl Group {
    fn accepts(&self, next: &Described, state: ToolState) -> bool {
        let first = &self.described[0];
        mergeable(first.kind)
            && first.kind == next.kind
            && first.verb == next.verb
            && first.fixed.is_none()
            && next.fixed.is_none()
            && can_merge_state(state)
            && self.states.iter().all(|state| can_merge_state(*state))
    }
    fn into_step(self, cwd: Option<&Path>) -> Step {
        let state = if self.states.contains(&ToolState::Waiting) {
            ToolState::Waiting
        } else if self.states.contains(&ToolState::Running) {
            ToolState::Running
        } else {
            self.states[self.states.len() - 1]
        };
        let took = self
            .took
            .iter()
            .copied()
            .sum::<Option<Duration>>()
            .filter(|_| state == ToolState::Done);
        Step {
            label: label_of(&self.described, Tense::of(state), cwd),
            item_ids: self.ids,
            state,
            took,
        }
    }
}

fn work_block(
    transcript: &Transcript,
    range: Range<usize>,
    live_elapsed: Option<Duration>,
    last: bool,
) -> Option<WorkBlock> {
    let items = &transcript.items()[range.clone()];
    if !items.iter().any(|item| {
        matches!(
            item,
            Item::User { .. } | Item::Assistant { .. } | Item::Tool { .. }
        )
    }) {
        return None;
    }
    let cwd = transcript.cwd();
    let mut groups: Vec<Group> = Vec::new();
    let mut first_tool = None;
    let mut last_tool = 0;
    let mut previous_was_tool = false;
    for (offset, item) in items.iter().enumerate() {
        let Item::Tool {
            id, state, took, ..
        } = item
        else {
            previous_was_tool = false;
            continue;
        };
        let call = Call::of_item(item, cwd).expect("a tool item");
        let described = describe(&call);
        first_tool.get_or_insert(range.start + offset);
        last_tool = range.start + offset;
        match groups.last_mut() {
            Some(group) if previous_was_tool && group.accepts(&described, *state) => {
                group.ids.push(*id);
                group.described.push(described);
                group.states.push(*state);
                group.took.push(*took);
            }
            _ => groups.push(Group {
                ids: vec![*id],
                described: vec![described],
                states: vec![*state],
                took: vec![*took],
            }),
        }
        previous_was_tool = true;
    }
    let step_count = groups.iter().map(|group| group.ids.len()).sum();
    let steps = groups
        .into_iter()
        .map(|group| group.into_step(cwd))
        .collect();
    let ended = match items.last() {
        Some(Item::TurnEnd { stopped, .. }) => Some(if *stopped {
            WorkState::Stopped
        } else {
            WorkState::Done
        }),
        Some(Item::Error { .. }) => Some(WorkState::Failed),
        Some(Item::Stopped { .. }) => Some(WorkState::Stopped),
        _ => None,
    };
    let state = ended.unwrap_or(match transcript.status() {
        Status::Working => WorkState::Working,
        Status::Waiting => WorkState::WaitingForYou,
        Status::Stopped => WorkState::Stopped,
        Status::Failed => WorkState::Failed,
        Status::Done | Status::Idle => WorkState::Done,
    });
    let elapsed = match items.last() {
        Some(Item::TurnEnd {
            duration_ms: Some(ms),
            ..
        }) => Some(Duration::from_millis(*ms)),
        _ if last && ended.is_none() && transcript.status().is_busy() => live_elapsed,
        _ => None,
    };
    Some(WorkBlock {
        tools: first_tool.map(|first| first..last_tool + 1),
        turn: range,
        steps,
        step_count,
        state,
        elapsed,
    })
}

/// The transcript's turns as work blocks, oldest first. A turn ends at its `TurnEnd`, its
/// `Error` or a `Stopped` notice, so turns claude starts by itself are found too. The last
/// block is live while the transcript is busy; `live_elapsed` is how long that turn has run
/// (see `Thread::turn_started_at`).
pub fn work_blocks(transcript: &Transcript, live_elapsed: Option<Duration>) -> Vec<WorkBlock> {
    let items = transcript.items();
    let mut blocks = Vec::new();
    let mut start = 0;
    for (ix, item) in items.iter().enumerate() {
        if matches!(
            item,
            Item::TurnEnd { .. } | Item::Error { .. } | Item::Stopped { .. }
        ) {
            blocks.extend(work_block(transcript, start..ix + 1, live_elapsed, false));
            start = ix + 1;
        }
    }
    if start < items.len() {
        blocks.extend(work_block(
            transcript,
            start..items.len(),
            live_elapsed,
            true,
        ));
    }
    blocks
}

/// What the transcript says the agent is doing, in a few words, for the thread list.
///
/// - waiting for an approval: its question ("Delete 3 files?"), or "Needs approval · Bash";
/// - working: the label of the step that runs now, or "Writing a reply" while a reply
///   streams; `None` when nothing in particular shows (the app has its own words for that);
/// - after the turn: "Answered" when the turn ended with a reply, else its last step; "Stopped"
///   or "Failed" when it did not finish;
/// - `None` when nothing was ever sent.
pub fn activity_line(transcript: &Transcript) -> Option<String> {
    transcript.cached_line(|| compute_activity_line(transcript))
}

fn compute_activity_line(transcript: &Transcript) -> Option<String> {
    let cwd = transcript.cwd();
    match transcript.status() {
        Status::Waiting => Some(match transcript.pending_approval() {
            Some(request) => approval_line(request, cwd),
            None => "Needs approval".to_string(),
        }),
        Status::Working => running_step(transcript).or_else(|| {
            transcript
                .items()
                .iter()
                .rev()
                .find_map(|item| match item {
                    Item::Assistant { live, .. } => Some(*live),
                    Item::Tool { .. } | Item::User { .. } => Some(false),
                    _ => None,
                })
                .filter(|live| *live)
                .map(|_| "Writing a reply".to_string())
        }),
        Status::Stopped => Some("Stopped".to_string()),
        Status::Failed => Some("Failed".to_string()),
        Status::Done => outcome(transcript),
        Status::Idle => None,
    }
}

fn is_turn_end(item: &Item) -> bool {
    matches!(
        item,
        Item::TurnEnd { .. } | Item::Error { .. } | Item::Stopped { .. }
    )
}

/// The last block `work_blocks` would return, built from the last turn alone: the scan goes
/// back from the end only as far as that turn starts.
fn last_work_block(transcript: &Transcript) -> Option<WorkBlock> {
    let items = transcript.items();
    let mut end = items.len();
    while end > 0 {
        let closed = is_turn_end(&items[end - 1]);
        let before = if closed { end - 1 } else { end };
        let start = items[..before]
            .iter()
            .rposition(is_turn_end)
            .map_or(0, |ix| ix + 1);
        let open_tail = end == items.len() && !closed;
        if let Some(block) = work_block(transcript, start..end, None, open_tail) {
            return Some(block);
        }
        end = start;
    }
    None
}

fn running_step(transcript: &Transcript) -> Option<String> {
    let block = last_work_block(transcript)?;
    let step = block.steps.last()?;
    (step.state == ToolState::Running).then(|| step.label.headline.clone())
}

fn outcome(transcript: &Transcript) -> Option<String> {
    let block = last_work_block(transcript)?;
    let items = &transcript.items()[block.turn.clone()];
    let replied = items
        .iter()
        .rev()
        .find(|item| matches!(item, Item::Assistant { .. } | Item::Tool { .. }))
        .is_some_and(
            |item| matches!(item, Item::Assistant { text, .. } if !text.trim().is_empty()),
        );
    if replied {
        Some("Answered".to_string())
    } else {
        block.steps.last().map(|step| step.label.headline.clone())
    }
}

/// Where a thread sits in the thread list, from the top: Pinned, Queue, Read (the list with no
/// header), then the dock below it: In flight, Snoozed and Settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Pinned by the user. It stays here whatever it does and keeps its status mark.
    Pinned,
    /// Waits on you: an approval or a question to answer, or a turn that ended and has not been
    /// opened yet.
    Queue,
    /// Nothing to do: opened since it last landed, and not put away.
    Read,
    /// A turn runs, a worktree is being made, a background command runs, or a usage limit holds
    /// it back.
    InFlight,
    /// Out of the list until its snooze runs out.
    Snoozed,
    /// Put away by the user. Sending a message brings it back.
    Settled,
}

/// What [`placement`] decides from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementFacts {
    pub status: Status,
    pub approval_pending: bool,
    /// A usage limit refused the thread's provider and has not reset yet.
    pub limit_paused: bool,
    /// The first message waits while a worktree is made or the project's base is recorded.
    pub preparing: bool,
    pub background_tasks: usize,
    /// Something happened since the user last had the thread on screen.
    pub unread: bool,
    pub list: ListState,
    /// Unix milliseconds, the unit of [`ListState`].
    pub now_ms: i64,
}

impl PlacementFacts {
    /// An approval or a question is waiting for an answer.
    pub fn waits_on_you(&self) -> bool {
        self.approval_pending || self.status == Status::Waiting
    }
}

/// Where a thread belongs. The user's choices come first: pinned, settled, then snoozed while
/// the snooze lasts. Then what the thread itself is doing: an answer owed to you, work in
/// flight, a turn you have not looked at, and last a command left running in the background.
/// An unread turn outranks a lone background command, so a failure is not hidden behind a dev
/// server.
pub fn placement(facts: PlacementFacts) -> Placement {
    let list = facts.list;
    if list.pinned_at.is_some() {
        Placement::Pinned
    } else if list.settled_at.is_some() {
        Placement::Settled
    } else if list.snoozed_until.is_some_and(|until| until > facts.now_ms) {
        Placement::Snoozed
    } else if facts.waits_on_you() {
        Placement::Queue
    } else if facts.preparing || facts.status.is_busy() || facts.limit_paused {
        Placement::InFlight
    } else if facts.unread {
        Placement::Queue
    } else if facts.background_tasks > 0 {
        Placement::InFlight
    } else {
        Placement::Read
    }
}

impl Thread {
    /// The turns' work blocks, the running one timed from when its turn started.
    pub fn work_blocks(&self) -> Vec<WorkBlock> {
        work_blocks(
            self.transcript(),
            self.turn_started_at().map(|started| started.elapsed()),
        )
    }

    /// Where the thread belongs in the thread list at `now_ms` (Unix milliseconds).
    pub fn placement(&self, now_ms: i64) -> Placement {
        placement(PlacementFacts {
            status: self.status(),
            approval_pending: self.transcript().pending_approval().is_some(),
            limit_paused: self.paused_until(now_ms / 1000).is_some(),
            preparing: self.preparing().is_some(),
            background_tasks: self.background_tasks().len(),
            unread: self.is_unread(),
            list: self.list_state(),
            now_ms,
        })
    }

    /// An approval or a question is waiting for an answer.
    pub fn waits_on_you(&self) -> bool {
        self.status() == Status::Waiting || self.transcript().pending_approval().is_some()
    }

    /// The line under the thread's title at `now` (Unix seconds): what it does right now, or
    /// how it ended. `None` when there is nothing specific to say.
    pub fn activity_line(&self, now: i64) -> Option<String> {
        let busy = self.status().is_busy();
        if let Some(reset) = self.paused_until(now).filter(|_| !busy) {
            return Some(format!(
                "Usage limit reached · resets {}",
                crate::picker::reset_time(reset, now)
            ));
        }
        let failed_unread = self.status() == Status::Failed && self.is_unread();
        if self.waits_on_you() || failed_unread {
            return activity_line(self.transcript());
        }
        match self.preparing() {
            Some(Waiting::Copy(_)) => Some("Creating a worktree".to_string()),
            Some(Waiting::Project(_)) => Some("Preparing the project".to_string()),
            None if busy => activity_line(self.transcript()),
            None => match self.background_tasks() {
                [] => activity_line(self.transcript()),
                [task] if !task.description.trim().is_empty() => {
                    Some(clip(&task.description, HEADLINE_CHARS))
                }
                [_] => Some("Background task running".to_string()),
                tasks => Some(format!("{} background tasks running", tasks.len())),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AgentBatch, AgentEvent, ApprovalKey, ApprovalKind, ApprovalView, Checkpoint, RememberScope,
        ToolOutcome, TurnOutcome,
    };
    use serde_json::json;

    fn tool(id: &str, name: &str, input: Value) -> ToolDescriptor {
        ToolDescriptor {
            id: id.into(),
            name: name.into(),
            display_name: name.into(),
            summary: String::new(),
            arguments: input,
        }
    }
    fn batch(events: Vec<AgentEvent>) -> AgentBatch {
        AgentBatch {
            events,
            checkpoint: Checkpoint::Transient,
        }
    }
    fn start(transcript: &mut Transcript, descriptor: ToolDescriptor) {
        transcript.apply(&batch(vec![AgentEvent::ToolUpsert(descriptor)]));
    }
    fn finish(transcript: &mut Transcript, id: &str, output: &str) {
        transcript.apply(&batch(vec![AgentEvent::ToolFinished {
            id: id.into(),
            outcome: ToolOutcome::Completed,
            output: output.into(),
        }]));
    }
    fn end_turn(transcript: &mut Transcript, ms: u64) {
        transcript.apply(&batch(vec![AgentEvent::TurnFinished {
            outcome: TurnOutcome::Completed,
            duration_ms: Some(ms),
            cost_usd: None,
            forget_conversation: false,
            queued: false,
        }]));
    }
    fn label(name: &str, input: Value, output: Option<&str>, state: ToolState) -> StepLabel {
        let mut transcript = Transcript::new();
        transcript.push_user("go");
        start(&mut transcript, tool("t", name, input));
        match state {
            ToolState::Running => {}
            ToolState::Done => finish(&mut transcript, "t", output.unwrap_or("")),
            other => panic!("{other:?}"),
        }
        step_label(&transcript.items()[1], None).unwrap()
    }
    fn running(name: &str, input: Value) -> StepLabel {
        label(name, input, None, ToolState::Running)
    }
    fn done(name: &str, input: Value) -> StepLabel {
        label(name, input, None, ToolState::Done)
    }
    fn headline(label: StepLabel) -> (String, Option<String>) {
        (label.headline, label.detail)
    }

    #[test]
    fn files_are_named_by_their_file_name_with_the_path_as_detail() {
        let input = json!({"file_path": "crates/core/src/thread.rs"});
        assert_eq!(
            headline(running("Read", input.clone())),
            (
                "Reading thread.rs".into(),
                Some("crates/core/src/thread.rs".into())
            )
        );
        assert_eq!(done("Read", input.clone()).headline, "Read thread.rs");
        assert_eq!(
            headline(done("Edit", json!({"file_path": "Cargo.toml"}))),
            ("Edited Cargo.toml".into(), None)
        );
        assert_eq!(running("Edit", input.clone()).headline, "Editing thread.rs");
        assert_eq!(running("MultiEdit", input).kind, StepKind::Edit);
    }

    #[test]
    fn a_write_says_created_only_when_the_result_says_so() {
        let input = json!({"file_path": "/tmp/project/store.rs"});
        assert_eq!(running("Write", input.clone()).headline, "Writing store.rs");
        let created = label(
            "Write",
            input.clone(),
            Some("File created successfully at: /tmp/project/store.rs"),
            ToolState::Done,
        );
        assert_eq!(
            (created.headline.as_str(), created.kind),
            ("Created store.rs", StepKind::Create)
        );
        let rewritten = label(
            "Write",
            input,
            Some("The file /tmp/project/store.rs has been updated successfully."),
            ToolState::Done,
        );
        assert_eq!(rewritten.headline, "Wrote store.rs");
    }

    #[test]
    fn a_command_uses_its_description_and_keeps_the_command_as_detail() {
        let input =
            json!({"command": "cargo test --workspace", "description": "Running the tests"});
        assert_eq!(
            headline(running("Bash", input.clone())),
            (
                "Running the tests".into(),
                Some("cargo test --workspace".into())
            )
        );
        assert_eq!(done("Bash", input).headline, "Running the tests");
    }

    #[test]
    fn a_command_without_a_description_is_named_by_its_first_words_only() {
        let ran = |command: &str| headline(done("Bash", json!({ "command": command })));
        assert_eq!(
            ran("cargo test --workspace --locked"),
            (
                "Ran cargo test".into(),
                Some("cargo test --workspace --locked".into())
            )
        );
        assert_eq!(ran("ls -la").0, "Ran ls");
        assert_eq!(ran("cat Cargo.toml").0, "Ran cat Cargo.toml");
        assert_eq!(ran("npm run build").0, "Ran npm run build");
        assert_eq!(
            ran("cd crates && cargo build 2>&1 | tail -5").0,
            "Ran cargo build"
        );
        assert_eq!(ran("RUST_LOG=debug cargo run -p app").0, "Ran cargo run");
        assert_eq!(ran("sed 's/a/b/' file").0, "Ran sed");
        assert_eq!(ran("   ").0, "Ran a command");
        assert_eq!(
            running("Bash", json!({"command": "git status"})).headline,
            "Running git status"
        );
        assert_eq!(
            ran("echo one\necho two").1.as_deref(),
            Some("echo one ↵ echo two")
        );
    }

    #[test]
    fn a_shell_wrapper_around_a_codex_command_is_looked_through() {
        let ran = |command: &str| headline(done("Command", json!({ "command": command })));
        assert_eq!(
            ran("/bin/zsh -lc 'touch workspace.txt'"),
            ("Ran touch workspace.txt".into(), None)
        );
        assert_eq!(ran("bash -c \"cargo test\"").0, "Ran cargo test");
        assert_eq!(ran("zsh -lc cargo").0, "Ran cargo");
        assert_eq!(ran("bash script.sh").0, "Ran bash script.sh");
    }

    #[test]
    fn searches_and_other_tools_are_named_from_the_event() {
        assert_eq!(
            headline(done(
                "Grep",
                json!({"pattern": "fn main", "path": "crates"})
            )),
            ("Searched for fn main".into(), Some("in crates".into()))
        );
        assert_eq!(
            done("Glob", json!({"pattern": "**/*.rs"})).headline,
            "Searched for **/*.rs"
        );
        assert_eq!(
            headline(done(
                "WebFetch",
                json!({"url": "https://example.com/docs?a=1"})
            )),
            (
                "Fetched example.com".into(),
                Some("https://example.com/docs?a=1".into())
            )
        );
        assert_eq!(
            done("WebSearch", json!({"query": "gpui"})).headline,
            "Searched the web for gpui"
        );
        assert_eq!(
            headline(done(
                "Task",
                json!({"description": "Explore the store", "subagent_type": "Explore"})
            )),
            ("Explore the store".into(), Some("Explore".into()))
        );
        assert_eq!(
            done("mcp__pencil__get_style", json!({"name": "x"})).headline,
            "Used pencil get_style"
        );
        assert_eq!(
            running("mcp__pencil__get_style", json!({})).headline,
            "Using pencil get_style"
        );
        assert_eq!(
            done("pencil · get_style", Value::Null).headline,
            "Used pencil get_style"
        );
        assert_eq!(
            done("SomethingNew", json!({"a": 1})).headline,
            "SomethingNew"
        );
    }

    #[test]
    fn codex_file_changes_say_what_they_do_to_how_many_files() {
        let changes = |kinds: &[&str]| {
            Value::Array(
                kinds
                    .iter()
                    .enumerate()
                    .map(|(ix, kind)| json!({"path": format!("/p/f{ix}.txt"), "kind": {"type": kind}}))
                    .collect(),
            )
        };
        assert_eq!(
            headline(done("File change", changes(&["add"]))),
            ("Created f0.txt".into(), Some("/p/f0.txt".into()))
        );
        assert_eq!(
            done("File change", changes(&["update"])).headline,
            "Edited f0.txt"
        );
        let deleted = done("File change", changes(&["delete", "delete", "delete"]));
        assert_eq!(
            headline(deleted),
            (
                "Deleted 3 files".into(),
                Some("f0.txt, f1.txt, f2.txt".into())
            )
        );
        assert_eq!(
            done("File change", changes(&["add", "update"])).headline,
            "Changed 2 files"
        );
        assert_eq!(done("File change", Value::Null).headline, "Changed files");
        assert_eq!(
            running("Web search", json!({"query": "rust"})).headline,
            "Searching the web for rust"
        );
    }

    fn turn_with(calls: &[(&str, &str, Value, &str)]) -> Transcript {
        let mut transcript = Transcript::new();
        transcript.push_user("go");
        for (id, name, input, output) in calls {
            start(&mut transcript, tool(id, name, input.clone()));
            if !output.is_empty() {
                finish(&mut transcript, id, output);
            }
        }
        transcript
    }
    fn read(
        id: &'static str,
        path: &'static str,
    ) -> (&'static str, &'static str, Value, &'static str) {
        (id, "Read", json!({ "file_path": path }), "ok")
    }

    #[test]
    fn consecutive_reads_merge_into_one_step() {
        let mut transcript = turn_with(&[
            read("a", "src/thread.rs"),
            read("b", "src/session.rs"),
            read("c", "Cargo.toml"),
            read("d", "src/thread.rs"),
        ]);
        end_turn(&mut transcript, 4000);
        let blocks = work_blocks(&transcript, None);
        assert_eq!(blocks.len(), 1);
        let block = &blocks[0];
        assert_eq!(block.steps.len(), 1);
        assert_eq!(block.step_count, 4);
        assert_eq!(block.steps[0].calls(), 4);
        assert_eq!(block.steps[0].label.headline, "Read 3 files");
        assert_eq!(
            block.steps[0].label.detail.as_deref(),
            Some("thread.rs, session.rs, Cargo.toml")
        );
        assert_eq!(block.tools, Some(1..5));
        assert_eq!(block.turn, 0..6);
    }

    #[test]
    fn steps_only_merge_when_nothing_separates_or_distinguishes_them() {
        let mut transcript = turn_with(&[
            read("a", "a.rs"),
            ("b", "Edit", json!({"file_path": "a.rs"}), "ok"),
            read("c", "b.rs"),
        ]);
        // Reply text between calls starts a new step.
        transcript.apply(&batch(vec![AgentEvent::Text {
            key: crate::agent::ContentKey(1),
            update: crate::agent::TextUpdate::Replace("Now the other one".into()),
        }]));
        start(
            &mut transcript,
            tool("d", "Read", json!({"file_path": "c.rs"})),
        );
        finish(&mut transcript, "d", "ok");
        let steps = &work_blocks(&transcript, None)[0].steps;
        let headlines: Vec<_> = steps.iter().map(|s| s.label.headline.as_str()).collect();
        assert_eq!(
            headlines,
            ["Read a.rs", "Edited a.rs", "Read b.rs", "Read c.rs"]
        );

        // A failed call stays on its own and says it failed.
        let mut transcript = turn_with(&[read("a", "a.rs")]);
        start(
            &mut transcript,
            tool("b", "Read", json!({"file_path": "b.rs"})),
        );
        transcript.apply(&batch(vec![AgentEvent::ToolFinished {
            id: "b".into(),
            outcome: ToolOutcome::Failed,
            output: "No such file".into(),
        }]));
        let steps = &work_blocks(&transcript, None)[0].steps;
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].state, ToolState::Failed);
    }

    #[test]
    fn a_step_reads_in_the_tense_its_state_calls_for() {
        let mut transcript = turn_with(&[read("a", "a.rs")]);
        start(
            &mut transcript,
            tool("b", "Read", json!({"file_path": "b.rs"})),
        );
        let block = &work_blocks(&transcript, Some(Duration::from_secs(5)))[0];
        assert_eq!(block.steps.len(), 1);
        assert_eq!(block.steps[0].state, ToolState::Running);
        assert_eq!(block.steps[0].label.headline, "Reading 2 files");
        assert_eq!(block.state, WorkState::Working);
        assert_eq!(block.elapsed, Some(Duration::from_secs(5)));
        assert_eq!(block.summary(|s| format!("{s}s")), None);
    }

    fn approval(id: &str, name: &str, input: Value) -> AgentEvent {
        AgentEvent::ApprovalRequested(ApprovalRequest {
            key: ApprovalKey {
                namespace: 1,
                serial: 1,
            },
            view: ApprovalView {
                tool: tool(id, name, input),
                kind: ApprovalKind::OtherTool,
                question: String::new(),
                affected_path: None,
                remember: RememberScope::OnceOnly(crate::agent::OnceReason::NoSafeScope),
            },
            headline: String::new(),
            risks: Vec::new(),
            model_headline: false,
        })
    }

    #[test]
    fn a_pending_approval_asks_in_a_few_words() {
        let mut transcript = Transcript::new();
        transcript.push_user("go");
        let changes: Vec<Value> = (0..3)
            .map(|ix| json!({"path": format!("/p/{ix}.txt"), "kind": {"type": "delete"}}))
            .collect();
        let descriptor = tool("t", "File change", Value::Array(changes));
        transcript.apply(&batch(vec![
            AgentEvent::ToolUpsert(descriptor.clone()),
            approval("t", "File change", descriptor.arguments.clone()),
        ]));
        assert_eq!(transcript.status(), Status::Waiting);
        assert_eq!(
            activity_line(&transcript).as_deref(),
            Some("Delete 3 files?")
        );
        let block = &work_blocks(&transcript, None)[0];
        assert_eq!(block.state, WorkState::WaitingForYou);
        assert_eq!(block.steps[0].label.headline, "Delete 3 files");

        let mut transcript = Transcript::new();
        transcript.push_user("go");
        let descriptor = tool(
            "t",
            "Bash",
            json!({"command": "rm -rf target", "description": "Clear the build folder"}),
        );
        transcript.apply(&batch(vec![
            AgentEvent::ToolUpsert(descriptor.clone()),
            approval("t", "Bash", descriptor.arguments.clone()),
        ]));
        assert_eq!(
            activity_line(&transcript).as_deref(),
            Some("Clear the build folder?")
        );

        let mut transcript = Transcript::new();
        transcript.push_user("go");
        let unknown = tool("t", "SomethingNew", json!({}));
        transcript.apply(&batch(vec![
            AgentEvent::ToolUpsert(unknown.clone()),
            approval("t", "SomethingNew", json!({})),
        ]));
        assert_eq!(
            activity_line(&transcript).as_deref(),
            Some("Needs approval · SomethingNew")
        );
    }

    #[test]
    fn the_line_follows_the_turn_from_running_to_its_outcome() {
        let mut transcript = Transcript::new();
        assert_eq!(activity_line(&transcript), None);
        transcript.push_user("go");
        assert_eq!(activity_line(&transcript), None);
        start(
            &mut transcript,
            tool("a", "Bash", json!({"command": "cargo test"})),
        );
        assert_eq!(
            activity_line(&transcript).as_deref(),
            Some("Running cargo test")
        );
        finish(&mut transcript, "a", "ok");
        assert_eq!(activity_line(&transcript), None);
        transcript.apply(&batch(vec![AgentEvent::Text {
            key: crate::agent::ContentKey(1),
            update: crate::agent::TextUpdate::Append("All green".into()),
        }]));
        assert_eq!(
            activity_line(&transcript).as_deref(),
            Some("Writing a reply")
        );
        end_turn(&mut transcript, 9000);
        assert_eq!(activity_line(&transcript).as_deref(), Some("Answered"));
    }

    #[test]
    fn a_turn_that_ended_on_a_step_reports_that_step() {
        let mut transcript = turn_with(&[read("a", "a.rs")]);
        end_turn(&mut transcript, 1000);
        assert_eq!(activity_line(&transcript).as_deref(), Some("Read a.rs"));
    }

    #[test]
    fn stopped_and_failed_turns_say_so_and_summaries_say_how_long() {
        let mut transcript = turn_with(&[read("a", "a.rs"), read("b", "b.rs")]);
        transcript.apply(&batch(vec![AgentEvent::TurnFinished {
            outcome: TurnOutcome::Stopped,
            duration_ms: Some(12_000),
            cost_usd: None,
            forget_conversation: false,
            queued: false,
        }]));
        assert_eq!(activity_line(&transcript).as_deref(), Some("Stopped"));
        let block = &work_blocks(&transcript, None)[0];
        assert_eq!(block.state, WorkState::Stopped);
        assert_eq!(
            block.summary(|s| format!("{s}s")).as_deref(),
            Some("Stopped after 12s · 2 steps")
        );

        let mut transcript = turn_with(&[read("a", "a.rs")]);
        transcript.apply(&batch(vec![AgentEvent::TurnFinished {
            outcome: TurnOutcome::Failed {
                title: "Claude stopped".into(),
                detail: "boom".into(),
            },
            duration_ms: None,
            cost_usd: None,
            forget_conversation: false,
            queued: false,
        }]));
        assert_eq!(activity_line(&transcript).as_deref(), Some("Failed"));
        let block = &work_blocks(&transcript, None)[0];
        assert_eq!(block.state, WorkState::Failed);
        assert_eq!(
            block.summary(|s| format!("{s}s")).as_deref(),
            Some("Failed · 1 step")
        );

        let mut transcript = turn_with(&[read("a", "a.rs")]);
        end_turn(&mut transcript, 243_000);
        let block = &work_blocks(&transcript, None)[0];
        assert_eq!(
            block
                .summary(|s| format!("{}m {}s", s / 60, s % 60))
                .as_deref(),
            Some("Worked for 4m 3s · 1 step")
        );
    }

    #[test]
    fn each_turn_is_its_own_block_and_a_lone_notice_makes_none() {
        let mut transcript = turn_with(&[read("a", "a.rs")]);
        end_turn(&mut transcript, 1000);
        transcript.push_user("again");
        start(&mut transcript, tool("b", "Bash", json!({"command": "ls"})));
        finish(&mut transcript, "b", "x");
        end_turn(&mut transcript, 2000);
        transcript.normalize_after_relaunch();
        assert_eq!(transcript.status(), Status::Done);
        let blocks = work_blocks(&transcript, None);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].steps[0].label.headline, "Read a.rs");
        assert_eq!(blocks[1].steps[0].label.headline, "Ran ls");
        assert!(blocks.iter().all(|block| block.state == WorkState::Done));

        let mut interrupted = turn_with(&[read("a", "a.rs")]);
        start(
            &mut interrupted,
            tool("b", "Bash", json!({"command": "sleep 9"})),
        );
        interrupted.normalize_after_relaunch();
        let blocks = work_blocks(&interrupted, None);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].state, WorkState::Stopped);
        let finished = work_blocks(&interrupted, None);
        interrupted.normalize_after_relaunch();
        assert_eq!(work_blocks(&interrupted, None), finished);
    }

    fn failed_turn(transcript: &mut Transcript) {
        transcript.apply(&batch(vec![AgentEvent::TurnFinished {
            outcome: TurnOutcome::Failed {
                title: "Claude stopped".into(),
                detail: "boom".into(),
            },
            duration_ms: None,
            cost_usd: None,
            forget_conversation: false,
            queued: false,
        }]));
    }

    #[test]
    fn the_last_block_is_built_from_the_last_turn_alone() {
        let same_as_full = |transcript: &Transcript| {
            assert_eq!(
                last_work_block(transcript),
                work_blocks(transcript, None).into_iter().next_back(),
                "{:?}",
                transcript.items()
            );
        };
        let mut transcript = Transcript::new();
        same_as_full(&transcript);
        transcript.push_user("one");
        same_as_full(&transcript);
        start(
            &mut transcript,
            tool("a", "Read", json!({"file_path": "a.rs"})),
        );
        same_as_full(&transcript);
        finish(&mut transcript, "a", "ok");
        end_turn(&mut transcript, 1000);
        same_as_full(&transcript);
        end_turn(&mut transcript, 5);
        same_as_full(&transcript);
        transcript.push_user("two");
        start(&mut transcript, tool("b", "Bash", json!({"command": "ls"})));
        same_as_full(&transcript);
        finish(&mut transcript, "b", "x");
        failed_turn(&mut transcript);
        same_as_full(&transcript);
        transcript.push_user("three");
        same_as_full(&transcript);
        start(
            &mut transcript,
            tool("c", "Bash", json!({"command": "pwd"})),
        );
        same_as_full(&transcript);
        finish(&mut transcript, "c", "x");
        end_turn(&mut transcript, 7);
        same_as_full(&transcript);
        transcript.normalize_after_relaunch();
        same_as_full(&transcript);
        assert_eq!(activity_line(&transcript).as_deref(), Some("Ran pwd"));
    }

    #[test]
    fn the_scan_ignores_turns_before_the_last_one() {
        let mut transcript = Transcript::new();
        for turn in 0..50 {
            transcript.push_user("go");
            for call in 0..20 {
                let id = format!("t{turn}-{call}");
                start(&mut transcript, tool(&id, "Read", json!({"file_path": id})));
                finish(&mut transcript, &id, "ok");
            }
            end_turn(&mut transcript, 10);
        }
        transcript.push_user("last");
        start(&mut transcript, tool("z", "Bash", json!({"command": "ls"})));
        let block = last_work_block(&transcript).unwrap();
        assert_eq!(block.turn.len(), 2);
        assert_eq!(block.step_count, 1);
        assert_eq!(activity_line(&transcript).as_deref(), Some("Running ls"));
    }

    #[test]
    fn the_cached_line_follows_every_change_to_the_transcript() {
        let mut transcript = Transcript::new();
        transcript.push_user("go");
        let revision = transcript.revision();
        assert_eq!(activity_line(&transcript), None);
        assert_eq!(activity_line(&transcript), None);
        assert_eq!(transcript.revision(), revision, "reading changes nothing");
        start(
            &mut transcript,
            tool("a", "Bash", json!({"command": "make"})),
        );
        assert!(transcript.revision() > revision);
        assert_eq!(activity_line(&transcript).as_deref(), Some("Running make"));
        transcript.apply(&batch(vec![approval(
            "a",
            "Bash",
            json!({"command": "make"}),
        )]));
        assert_eq!(transcript.status(), Status::Waiting);
        assert_ne!(activity_line(&transcript).as_deref(), Some("Running make"));
        let key = transcript.pending_approval().unwrap().key();
        transcript.resolve_approval(key, crate::agent::ApprovalIntent::AllowOnce);
        assert_eq!(transcript.status(), Status::Working);
        assert_eq!(activity_line(&transcript).as_deref(), Some("Running make"));
        finish(&mut transcript, "a", "ok");
        end_turn(&mut transcript, 9);
        assert_eq!(activity_line(&transcript).as_deref(), Some("Ran make"));
        let clone = transcript.clone();
        transcript.push_user("again");
        assert_eq!(activity_line(&clone).as_deref(), Some("Ran make"));
        assert_eq!(activity_line(&transcript), None);
    }

    fn facts(status: Status) -> PlacementFacts {
        PlacementFacts {
            status,
            approval_pending: false,
            limit_paused: false,
            preparing: false,
            background_tasks: 0,
            unread: false,
            list: ListState::default(),
            now_ms: 1_000,
        }
    }

    #[test]
    fn threads_sort_into_the_queue_in_flight_and_read() {
        use Placement::*;
        let place = placement;
        assert_eq!(place(facts(Status::Idle)), Read);
        assert_eq!(place(facts(Status::Done)), Read);
        assert_eq!(place(facts(Status::Stopped)), Read);
        assert_eq!(place(facts(Status::Failed)), Read);
        assert_eq!(place(facts(Status::Working)), InFlight);
        assert_eq!(place(facts(Status::Waiting)), Queue);
        for status in [Status::Done, Status::Failed, Status::Stopped] {
            assert_eq!(
                place(PlacementFacts {
                    unread: true,
                    ..facts(status)
                }),
                Queue,
                "{status:?}"
            );
        }
        assert_eq!(
            place(PlacementFacts {
                approval_pending: true,
                ..facts(Status::Working)
            }),
            Queue
        );
        assert_eq!(
            place(PlacementFacts {
                preparing: true,
                ..facts(Status::Idle)
            }),
            InFlight
        );
        assert_eq!(
            place(PlacementFacts {
                background_tasks: 1,
                ..facts(Status::Done)
            }),
            InFlight
        );
    }

    #[test]
    fn a_usage_limit_holds_a_thread_in_flight_whether_or_not_it_works() {
        use Placement::*;
        for status in [Status::Done, Status::Failed, Status::Working] {
            assert_eq!(
                placement(PlacementFacts {
                    limit_paused: true,
                    unread: true,
                    ..facts(status)
                }),
                InFlight,
                "{status:?}"
            );
        }
        assert_eq!(
            placement(PlacementFacts {
                limit_paused: true,
                approval_pending: true,
                ..facts(Status::Waiting)
            }),
            Queue
        );
    }

    #[test]
    fn an_unread_turn_is_not_hidden_behind_a_background_command() {
        use Placement::*;
        let running = PlacementFacts {
            background_tasks: 1,
            ..facts(Status::Failed)
        };
        assert_eq!(placement(running), InFlight);
        assert_eq!(
            placement(PlacementFacts {
                unread: true,
                ..running
            }),
            Queue
        );
    }

    #[test]
    fn the_users_choices_outrank_what_the_thread_does() {
        use Placement::*;
        let busy = PlacementFacts {
            approval_pending: true,
            unread: true,
            background_tasks: 2,
            ..facts(Status::Waiting)
        };
        let list = |list| PlacementFacts { list, ..busy };
        let at = |pinned_at, snoozed_until, settled_at| {
            list(ListState {
                pinned_at,
                snoozed_until,
                settled_at,
                landed_at: 0,
            })
        };
        assert_eq!(placement(busy), Queue);
        assert_eq!(placement(at(Some(1), None, None)), Pinned);
        assert_eq!(placement(at(Some(1), Some(5_000), Some(1))), Pinned);
        assert_eq!(placement(at(None, Some(5_000), Some(1))), Settled);
        assert_eq!(placement(at(None, Some(5_000), None)), Snoozed);
        assert_eq!(placement(at(None, Some(1_001), None)), Snoozed);
        assert_eq!(placement(at(None, Some(1_000), None)), Queue);
        assert_eq!(placement(at(None, Some(999), None)), Queue);
    }
}
