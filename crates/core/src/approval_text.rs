//! Plain-English approval headlines and conservative risk tags, shared by both providers.
//! A tag is set only when the command shows it; anything unclear gets no tag.

use crate::agent::ApprovalRisk;
use std::path::{Component, Path, PathBuf};

#[derive(Default)]
struct Segment {
    words: Vec<String>,
    redirects: Vec<String>,
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

#[derive(Default)]
struct Splitter {
    segments: Vec<Segment>,
    segment: Segment,
    word: String,
    has_word: bool,
    redirect: bool,
    heredoc_next: Option<bool>,
    heredocs: Vec<(String, bool)>,
}
impl Splitter {
    fn flush(&mut self) {
        if self.has_word {
            let finished = std::mem::take(&mut self.word);
            if let Some(dash) = self.heredoc_next.take() {
                self.heredocs.push((finished, dash));
            } else if self.redirect {
                self.segment.redirects.push(finished);
            } else {
                self.segment.words.push(finished);
            }
            self.redirect = false;
            self.has_word = false;
        }
    }
    fn end(&mut self) {
        self.flush();
        if !self.segment.words.is_empty() || !self.segment.redirects.is_empty() {
            self.segments.push(std::mem::take(&mut self.segment));
        }
        self.redirect = false;
        self.heredoc_next = None;
    }
}

/// Splits a shell line into simple commands at `&&`, `||`, `;`, `|`, `&`, newlines and
/// parentheses, keeping quoted text whole and setting redirect targets and heredoc bodies aside.
fn split(command: &str) -> Vec<Segment> {
    let chars: Vec<char> = command.chars().collect();
    let n = chars.len();
    let mut s = Splitter::default();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if matches!(c, '<' | '>') || (c == '&' && chars.get(i + 1) == Some(&'>')) {
            if s.has_word && s.word.chars().all(|c| c.is_ascii_digit()) {
                s.word.clear();
                s.has_word = false;
            } else {
                s.flush();
            }
            let start = i;
            while i < n && matches!(chars[i], '<' | '>' | '&' | '|') {
                i += 1;
            }
            let op: String = chars[start..i].iter().collect();
            if op == "<<" || op == "<<-" {
                s.heredoc_next = Some(op == "<<-");
            } else {
                s.redirect = true;
            }
            continue;
        }
        match c {
            '\'' => {
                s.has_word = true;
                i += 1;
                while i < n && chars[i] != '\'' {
                    s.word.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                s.has_word = true;
                i += 1;
                while i < n && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < n {
                        i += 1;
                    }
                    s.word.push(chars[i]);
                    i += 1;
                }
            }
            '\\' => {
                if i + 1 < n {
                    i += 1;
                    if chars[i] != '\n' {
                        s.word.push(chars[i]);
                        s.has_word = true;
                    }
                }
            }
            '#' if !s.has_word => {
                while i + 1 < n && chars[i + 1] != '\n' {
                    i += 1;
                }
            }
            '\n' => {
                s.end();
                let mut next = i + 1;
                for (delimiter, dash) in std::mem::take(&mut s.heredocs) {
                    while next < n {
                        let end = chars[next..]
                            .iter()
                            .position(|&c| c == '\n')
                            .map_or(n, |offset| next + offset);
                        let line: String = chars[next..end].iter().collect();
                        next = (end + 1).min(n);
                        let line = if dash {
                            line.trim_start_matches('\t')
                        } else {
                            &line
                        };
                        if line == delimiter {
                            break;
                        }
                    }
                }
                i = next - 1;
            }
            ';' | '|' | '&' | '(' | ')' => s.end(),
            c if c.is_whitespace() => s.flush(),
            c => {
                s.word.push(c);
                s.has_word = true;
            }
        }
        i += 1;
    }
    s.end();
    s.segments
}

const KEYWORDS: &[&str] = &[
    "do", "then", "else", "elif", "if", "while", "until", "!", "{", "}",
];
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "nohup", "time", "nice", "ionice", "exec", "xargs",
];
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];
const PATH_COMMANDS: &[&str] = &[
    "cd", "ls", "cat", "cp", "mv", "mkdir", "touch", "rm", "rmdir", "ln", "chmod", "chown", "tee",
    "head", "tail", "find", "tar", "unzip", "zip", "rsync", "scp", "open", "less", "more", "wc",
    "stat", "du", "diff", "install", "truncate", "source", ".", "code", "vim", "nano",
];

fn wrapper_value_flags(wrapper: &str) -> &'static [&'static str] {
    match wrapper {
        "sudo" | "doas" => &[
            "-u", "-g", "-C", "-h", "-p", "-r", "-t", "-U", "-D", "-R", "-T",
        ],
        "nice" => &["-n"],
        "ionice" => &["-c", "-n", "-p"],
        "env" => &["-u", "-C"],
        "xargs" => &["-I", "-n", "-P", "-d", "-E", "-L", "-s", "-a"],
        _ => &[],
    }
}

fn base_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Index of the word that names the program: after assignments, keywords and wrappers.
fn command_start(words: &[String]) -> usize {
    let mut i = 0;
    loop {
        while i < words.len() && (is_assignment(&words[i]) || KEYWORDS.contains(&words[i].as_str()))
        {
            i += 1;
        }
        let Some(word) = words.get(i) else {
            return words.len();
        };
        let name = base_name(word);
        if !WRAPPERS.contains(&name) {
            return i;
        }
        i += 1;
        let value_flags = wrapper_value_flags(name);
        while let Some(word) = words.get(i) {
            if is_assignment(word) {
                i += 1;
            } else if word.starts_with('-') && word.len() > 1 {
                i += if value_flags.contains(&word.as_str()) {
                    2
                } else {
                    1
                };
            } else {
                break;
            }
        }
    }
}

/// The script after `sh -c` (also `-lc`, `-ec`), or after `eval`.
fn nested_script(name: &str, args: &[String]) -> Option<String> {
    if name == "eval" {
        return (!args.is_empty()).then(|| args.join(" "));
    }
    if !SHELLS.contains(&name) {
        return None;
    }
    let position = args
        .iter()
        .position(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c'))?;
    args.get(position + 1).cloned()
}

/// The program the line runs first, ignoring a leading `cd`, for "Run <program>".
pub(crate) fn first_program(command: &str) -> Option<String> {
    first_program_at(command, 0)
}
fn first_program_at(command: &str, depth: u8) -> Option<String> {
    let mut fallback = None;
    for segment in split(command) {
        let start = command_start(&segment.words);
        let Some(word) = segment.words.get(start) else {
            continue;
        };
        let name = base_name(word);
        if depth < 3
            && let Some(script) = nested_script(name, &segment.words[start + 1..])
            && let Some(inner) = first_program_at(&script, depth + 1)
        {
            return Some(inner);
        }
        if name.is_empty() {
            continue;
        }
        if name == "cd" {
            fallback.get_or_insert_with(|| name.to_string());
            continue;
        }
        return Some(name.to_string());
    }
    fallback
}

fn note(found: &mut Option<ApprovalRisk>, risk: ApprovalRisk) {
    *found = Some(found.map_or(risk, |current| current.min(risk)));
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// True when `raw`, relative to `base`, is a path outside `project`. Lexical only: symlinks are
/// not followed. Words with variables or unknown `~` forms are left alone.
pub(crate) fn is_outside(
    project: &Path,
    base: Option<&Path>,
    raw: &str,
    home: Option<&Path>,
) -> bool {
    if !project.is_absolute() || raw.contains('$') || raw.contains('`') {
        return false;
    }
    let path = if raw == "~" {
        home.map(Path::to_path_buf)
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.map(|home| home.join(rest))
    } else if raw.starts_with('~') {
        None
    } else if raw.starts_with('/') {
        Some(PathBuf::from(raw))
    } else if Path::new(raw)
        .components()
        .any(|component| component == Component::ParentDir)
    {
        base.filter(|base| base.is_absolute())
            .map(|base| base.join(raw))
    } else {
        None
    };
    let Some(path) = path else {
        return false;
    };
    let resolved = lexical(&path);
    if ["null", "stdin", "stdout", "stderr", "tty"]
        .iter()
        .any(|name| resolved == Path::new("/dev").join(name))
    {
        return false;
    }
    !resolved.starts_with(lexical(project))
}

struct Scan<'a> {
    project: Option<&'a Path>,
    home: Option<&'a Path>,
    found: Option<ApprovalRisk>,
}

impl Scan<'_> {
    fn outside(&self, base: Option<&Path>, raw: &str) -> bool {
        let raw = match raw.strip_prefix("--").and_then(|flag| flag.split_once('=')) {
            Some((_, value)) => value,
            None if raw.starts_with('-') => return false,
            None => raw,
        };
        self.project
            .is_some_and(|project| is_outside(project, base, raw, self.home))
    }

    fn line(&mut self, command: &str, base: &mut Option<PathBuf>, depth: u8) {
        for segment in split(command) {
            for target in &segment.redirects {
                if self.outside(base.as_deref(), target) {
                    note(&mut self.found, ApprovalRisk::OutsideProject);
                }
            }
            let start = command_start(&segment.words);
            let Some(word) = segment.words.get(start) else {
                continue;
            };
            let name = base_name(word);
            let args = &segment.words[start + 1..];
            if depth < 3
                && let Some(script) = nested_script(name, args)
            {
                self.line(&script, base, depth + 1);
                continue;
            }
            if let Some(risk) = program_risk(name, args) {
                note(&mut self.found, risk);
            }
            if PATH_COMMANDS.contains(&name)
                && args.iter().any(|arg| self.outside(base.as_deref(), arg))
            {
                note(&mut self.found, ApprovalRisk::OutsideProject);
            }
            if name == "cd" {
                *base = match args.iter().find(|arg| !arg.starts_with('-')) {
                    Some(dir) if !dir.contains('$') && !dir.starts_with('~') => {
                        base.as_ref().map(|current| lexical(&current.join(dir)))
                    }
                    _ => None,
                };
            }
        }
    }
}

fn first_non_flag<'a>(args: &'a [String], with_value: &[&str]) -> Option<(usize, &'a str)> {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if with_value.contains(&arg.as_str()) {
            i += 2;
        } else if arg.starts_with('-') {
            i += 1;
        } else {
            return Some((i, arg));
        }
    }
    None
}

fn has_short_flag(args: &[String], flag: char) -> bool {
    args.iter()
        .any(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.contains(flag))
}

fn program_risk(name: &str, args: &[String]) -> Option<ApprovalRisk> {
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    match name {
        "rm" | "rmdir" | "unlink" => Some(ApprovalRisk::DeletesFiles),
        "find" => {
            let runs_removal = args.windows(2).any(|pair| {
                ["-exec", "-execdir", "-ok", "-okdir"].contains(&pair[0].as_str())
                    && ["rm", "rmdir", "unlink"].contains(&base_name(&pair[1]))
            });
            (has("-delete") || runs_removal).then_some(ApprovalRisk::DeletesFiles)
        }
        "curl" | "wget" => Some(ApprovalRisk::UsesNetwork),
        "git" => {
            let (at, sub) = first_non_flag(
                args,
                &[
                    "-C",
                    "-c",
                    "--git-dir",
                    "--work-tree",
                    "--namespace",
                    "--exec-path",
                ],
            )?;
            let rest = &args[at + 1..];
            match sub {
                "clean" => {
                    let dry_run =
                        has_short_flag(rest, 'n') || rest.iter().any(|arg| arg == "--dry-run");
                    (!dry_run).then_some(ApprovalRisk::DeletesFiles)
                }
                "reset" => rest
                    .iter()
                    .any(|arg| arg == "--hard")
                    .then_some(ApprovalRisk::RewritesHistory),
                "filter-branch" => Some(ApprovalRisk::RewritesHistory),
                "push" => {
                    let force = has_short_flag(rest, 'f')
                        || rest.iter().any(|arg| {
                            arg == "--force"
                                || arg.starts_with("--force-with-lease")
                                || arg == "--force-if-includes"
                                || (arg.starts_with('+') && arg.len() > 1)
                        });
                    Some(if force {
                        ApprovalRisk::RewritesHistory
                    } else {
                        ApprovalRisk::UsesNetwork
                    })
                }
                "fetch" | "clone" | "pull" => Some(ApprovalRisk::UsesNetwork),
                _ => None,
            }
        }
        "jj" => {
            let (at, sub) = first_non_flag(
                args,
                &[
                    "-R",
                    "--repository",
                    "--at-op",
                    "--at-operation",
                    "--color",
                    "--config",
                    "--config-toml",
                    "--config-file",
                ],
            )?;
            match sub {
                "abandon" => Some(ApprovalRisk::RewritesHistory),
                "git" => matches!(
                    first_non_flag(&args[at + 1..], &[]).map(|(_, sub)| sub),
                    Some("push" | "fetch" | "clone")
                )
                .then_some(ApprovalRisk::UsesNetwork),
                _ => None,
            }
        }
        "npm" | "pnpm" | "yarn" | "bun" => first_non_flag(args, &[])
            .is_some_and(|(_, sub)| ["install", "i", "add", "ci"].contains(&sub))
            .then_some(ApprovalRisk::UsesNetwork),
        "cargo" => {
            let rest: Vec<String> = args
                .iter()
                .filter(|arg| !arg.starts_with('+'))
                .cloned()
                .collect();
            first_non_flag(&rest, &[])
                .is_some_and(|(_, sub)| sub == "install")
                .then_some(ApprovalRisk::UsesNetwork)
        }
        "pip" | "pip3" => first_non_flag(args, &[])
            .is_some_and(|(_, sub)| sub == "install")
            .then_some(ApprovalRisk::UsesNetwork),
        "python" | "python3" => args
            .windows(3)
            .any(|triple| triple[0] == "-m" && triple[1] == "pip" && triple[2] == "install")
            .then_some(ApprovalRisk::UsesNetwork),
        _ => None,
    }
}

/// The most severe risk the command line shows. `project` is the thread's working directory
/// and `run_in` where the command runs; without a project there is no outside-the-project tag.
pub(crate) fn command_risk(
    command: &str,
    project: Option<&Path>,
    run_in: Option<&Path>,
) -> Option<ApprovalRisk> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    command_risk_with(command, project, run_in, home.as_deref())
}
pub(crate) fn command_risk_with(
    command: &str,
    project: Option<&Path>,
    run_in: Option<&Path>,
    home: Option<&Path>,
) -> Option<ApprovalRisk> {
    let mut scan = Scan {
        project,
        home,
        found: None,
    };
    let mut base = run_in.or(project).map(Path::to_path_buf);
    scan.line(command, &mut base, 0);
    scan.found
}

/// A one-line sentence from the CLI's own description: first line, trimmed, capitalised, with
/// its trailing period removed.
pub(crate) fn sentence(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let line = match line.strip_suffix('.') {
        Some(rest) if !rest.ends_with('.') => rest.trim_end(),
        _ => line,
    };
    let mut chars = line.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

pub(crate) fn run_headline(command: Option<&str>) -> String {
    match command.and_then(first_program) {
        Some(program) => format!("Run {program}"),
        None => "Run a command".to_string(),
    }
}

pub(crate) fn file_name(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

pub(crate) fn host_of(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .split_once("://")
        .map_or(url.trim(), |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = match host_port.strip_prefix('[') {
        Some(inner) => inner.split(']').next()?,
        None => host_port.split(':').next()?,
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// `mcp__server__tool` as `(server, tool)`.
pub(crate) fn mcp_parts(tool_name: &str) -> Option<(&str, &str)> {
    let (server, tool) = tool_name.strip_prefix("mcp__")?.split_once("__")?;
    (!server.is_empty() && !tool.is_empty()).then_some((server, tool))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ApprovalRisk::*;

    fn risk(command: &str) -> Option<ApprovalRisk> {
        command_risk_with(
            command,
            Some(Path::new("/work/app")),
            None,
            Some(Path::new("/Users/me")),
        )
    }

    #[test]
    fn deleting_commands_are_tagged() {
        for command in [
            "rm a.txt",
            "rm -rf build",
            "/bin/rm -f a",
            "rmdir old",
            "unlink a",
            "git clean -fd",
            "git -C sub clean -fdx",
            "find . -name '*.tmp' -delete",
            "find . -type f -exec rm {} \\;",
            "cargo build && rm -rf target",
            "ls; rm a",
            "sudo rm -rf cache",
            "env FOO=1 rm a",
            "xargs rm",
            "FOO=1 rm a",
            "for f in *.log; do rm $f; done",
            "bash -c 'rm a'",
            "/bin/zsh -lc 'touch x && rm x'",
            "eval \"rm a\"",
            "echo $(rm a)",
        ] {
            assert_eq!(risk(command), Some(DeletesFiles), "{command}");
        }
    }

    #[test]
    fn history_rewrites_are_tagged_over_the_network_tag() {
        for command in [
            "git reset --hard",
            "git reset --hard HEAD~1",
            "git push --force",
            "git push -f origin main",
            "git push -uf origin main",
            "git push --force-with-lease",
            "git push origin +main",
            "git -c core.x=1 push --force",
            "jj abandon",
            "jj -R ../x abandon abc",
            "git filter-branch --all",
        ] {
            assert_eq!(risk(command), Some(RewritesHistory), "{command}");
        }
    }

    #[test]
    fn network_commands_are_tagged() {
        for command in [
            "curl https://example.com",
            "wget -q https://example.com/a.tgz",
            "npm install",
            "npm i left-pad",
            "pnpm add react",
            "yarn add x",
            "cargo install ripgrep",
            "cargo +nightly install x",
            "pip install requests",
            "pip3 install -r r.txt",
            "python3 -m pip install x",
            "git push",
            "git push origin main",
            "git fetch",
            "git clone https://example.com/r.git",
            "git pull --rebase",
            "jj git push",
            "jj git fetch",
        ] {
            assert_eq!(risk(command), Some(UsesNetwork), "{command}");
        }
    }

    #[test]
    fn paths_outside_the_working_directory_are_tagged() {
        for command in [
            "cat /etc/hosts",
            "ls ~/.ssh",
            "cat ~/.ssh/id_rsa",
            "cd /tmp && ls",
            "cd .. && ls",
            "cp a ../other/a",
            "echo hi > /tmp/out",
            "echo hi >> ../log",
            "ls /usr/bin | head",
            "cd sub/../.. && touch x",
            "tar --directory=/tmp x",
        ] {
            assert_eq!(risk(command), Some(OutsideProject), "{command}");
        }
    }

    #[test]
    fn deleting_outranks_other_tags() {
        assert_eq!(risk("rm /tmp/x"), Some(DeletesFiles));
        assert_eq!(risk("curl x | rm y"), Some(DeletesFiles));
        assert_eq!(risk("git push --force /tmp/x"), Some(RewritesHistory));
        assert_eq!(
            risk("cat /etc/hosts && curl example.com"),
            Some(OutsideProject)
        );
    }

    #[test]
    fn commands_without_a_clear_risk_get_no_tag() {
        for command in [
            "",
            "ls -la",
            "touch newjen_probe.txt",
            "cargo test --workspace",
            "git status",
            "git reset --soft HEAD~1",
            "git clean -n",
            "git clean -nfd",
            "jj log",
            "jj git remote list",
            "npm test",
            "pip list",
            "echo rm -rf /",
            "echo 'rm a' > notes.txt",
            "echo \"curl x\"",
            "cat <<EOF > a.txt\nrm -rf /\ncurl x\nEOF",
            "grep -r '/etc/passwd' src",
            "sed -n '/start/,/end/p' file",
            "echo /etc/hosts",
            "ls $HOME/x",
            "ls ~other/x",
            "cat /work/app/src/a.rs",
            "cat /work/app/../app/a.rs",
            "ls ./src ../app/src",
            "cmd 2>&1",
            "make 2>/dev/null",
            "cat a > /dev/null",
            "ls # rm a",
            "type rm",
            "which curl",
            "command -v rm",
            "bash script.sh",
            "echo $(( 1 + 2 ))",
            "git log --oneline",
            "git commit -m 'push --force then rm x'",
            "python3 script.py",
        ] {
            assert_eq!(risk(command), None, "{command}");
        }
    }

    #[test]
    fn working_directory_and_home_decide_what_is_outside() {
        let inside = command_risk_with(
            "cat ~/proj/a.rs",
            Some(Path::new("/Users/me/proj")),
            None,
            Some(Path::new("/Users/me")),
        );
        assert_eq!(inside, None);
        assert_eq!(
            command_risk_with("cat ~/x", Some(Path::new("/Users/me/proj")), None, None),
            None,
            "unknown home is unsure, not outside"
        );
        assert_eq!(
            command_risk_with("cat /etc/hosts", None, None, None),
            None,
            "no known working directory, no outside tag"
        );
        assert_eq!(
            command_risk_with(
                "cat ../a",
                Some(Path::new("/w/p")),
                Some(Path::new("/w/p/sub")),
                None
            ),
            None
        );
        assert_eq!(
            command_risk_with(
                "cat ../../a",
                Some(Path::new("/w/p")),
                Some(Path::new("/w/p/sub")),
                None
            ),
            Some(OutsideProject)
        );
        assert_eq!(
            command_risk_with("cd sub && cat ../a", Some(Path::new("/w/p")), None, None),
            None
        );
        assert_eq!(
            command_risk_with("cd ../.. && cat a", Some(Path::new("/w/p")), None, None),
            Some(OutsideProject)
        );
    }

    #[test]
    fn sentences_are_trimmed_capitalised_and_lose_one_trailing_period() {
        assert_eq!(
            sentence("  delete 3 files.  ").as_deref(),
            Some("Delete 3 files")
        );
        assert_eq!(
            sentence("run the tests\nsecond line").as_deref(),
            Some("Run the tests")
        );
        assert_eq!(sentence("\n\nlist files").as_deref(), Some("List files"));
        assert_eq!(sentence("wait...").as_deref(), Some("Wait..."));
        assert_eq!(
            sentence("écrire un fichier").as_deref(),
            Some("Écrire un fichier")
        );
        assert_eq!(sentence("."), None);
        assert_eq!(sentence("  \n "), None);
    }

    #[test]
    fn first_program_skips_wrappers_and_a_leading_cd() {
        for (command, program) in [
            ("touch a", "touch"),
            ("/bin/zsh -lc 'touch once.txt .'", "touch"),
            ("cd sub && cargo test", "cargo"),
            ("FOO=1 sudo -u me /usr/bin/make all", "make"),
            ("bash -c \"cd x; npm run build\"", "npm"),
            ("cd x", "cd"),
        ] {
            assert_eq!(
                first_program(command).as_deref(),
                Some(program),
                "{command}"
            );
        }
        assert_eq!(first_program("  "), None);
        assert_eq!(run_headline(None), "Run a command");
        assert_eq!(run_headline(Some("ls -la")), "Run ls");
    }

    #[test]
    fn hosts_ignore_credentials_ports_paths_and_case() {
        for (url, host) in [
            ("https://Example.com/a?b#c", "example.com"),
            ("http://localhost:3000/x", "localhost"),
            ("https://good.com@evil.example/x", "evil.example"),
            ("https://user:pw@example.org:8443", "example.org"),
            ("http://[::1]:80/x", "::1"),
            ("example.com/path", "example.com"),
        ] {
            assert_eq!(host_of(url).as_deref(), Some(host), "{url}");
        }
        assert_eq!(host_of("https:///x"), None);
        assert_eq!(host_of(""), None);
    }

    #[test]
    fn mcp_names_split_into_server_and_tool() {
        assert_eq!(
            mcp_parts("mcp__linear__list_issues"),
            Some(("linear", "list_issues"))
        );
        assert_eq!(mcp_parts("mcp__broken"), None);
        assert_eq!(mcp_parts("Bash"), None);
    }
}
