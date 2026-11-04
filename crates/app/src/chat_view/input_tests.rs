use super::*;
use gpui::{AppContext, TestAppContext, VisualTestContext, size};
use gpui_kit::base::Root;
use newjen_core::vcs::Scope;

fn setup<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    with_approvals: bool,
) -> (Entity<ChatView>, PathBuf, &'a mut VisualTestContext) {
    setup_in(cx, name, with_approvals, None)
}

fn loaded(patch: &str) -> review::Loaded {
    review::Loaded {
        diff: newjen_core::vcs::Diff {
            patch: patch.into(),
            truncated: false,
        },
        facts: None,
    }
}

fn working(thread: &Thread) -> bool {
    is_busy(thread.status())
}

/// With a `checkout_hook`, the project is the git repository `<path>/project` with one commit,
/// whose `post-checkout` hook is that script (see `install_hook`), so its worktrees go to
/// `<path>/.newjen-worktrees/project/`. Without one it is the plain folder `<path>`.
fn setup_in<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    with_approvals: bool,
    checkout_hook: Option<&str>,
) -> (Entity<ChatView>, PathBuf, &'a mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);

        keys::bind(cx);
    });
    let path = std::env::temp_dir().join(format!(
        "newjen-input-{name}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let store = Store::open_in(&path).unwrap();
    let mut tools = Tools::find();
    let project_dir = match checkout_hook {
        Some(hook) => {
            // The worktree job runs git on threads of its own.
            cx.executor().allow_parking();
            tools = git_tools(&path);
            let project = path.join("project");
            std::fs::create_dir_all(&project).unwrap();
            let project = project.canonicalize().unwrap();
            git(
                &tools,
                &project,
                &["init", "--quiet", "--initial-branch=main"],
            );
            std::fs::write(project.join("a.txt"), "a\n").unwrap();
            git(&tools, &project, &["add", "--all"]);
            git(&tools, &project, &["commit", "--quiet", "-m", "base"]);
            install_hook(&project, hook);
            project
        }
        None => path.clone(),
    };
    let project = store.open_project(&project_dir).unwrap();
    let mut base_config = SessionConfig::new(&project_dir);
    if with_approvals {
        base_config.program = script(&path);
        cx.executor().allow_parking();
        cx.update(|cx| cx.set_reduce_motion(true));
    }
    let startup = Startup {
        store,
        project,
        base_config,
        threads: vec![],
        problem: None,
        home: None,
        tools,
    };
    let mut chat = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| ChatView::new(startup, window, cx));
        chat = Some(view.clone());
        Root::new(view, window, cx)
    });
    (chat.unwrap(), path, cx)
}

#[gpui::test]
fn escape_closes_the_drawer_before_the_field_handles_it(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "drawer", false);
    cx.simulate_resize(size(px(700.), px(600.)));
    cx.simulate_keystrokes("cmd-b");
    assert!(cx.update(|_, cx| chat.read(cx).drawer_open));
    assert!(cx.update(|window, cx| window.has_active_sheet(cx)));
    cx.update(|window, cx| {
        chat.read(cx).field_focus(cx).unwrap().focus(window, cx);
    });
    cx.simulate_input("keep this draft");
    cx.simulate_keystrokes("escape");
    assert!(!cx.update(|_, cx| chat.read(cx).drawer_open));
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "keep this draft"
    );
    cx.update(|_, cx| chat.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    std::fs::remove_dir_all(path).unwrap();
}
#[gpui::test]
fn kit_thread_rows_preserve_a_draft_when_clicked(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "kit-row-focus", false);
    cx.simulate_input("first draft");
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_input("second draft");
    click(cx, "thread-row");
    assert!(cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window)));
    cx.simulate_input(" stays editable");
    assert!(
        cx.update(|_, cx| chat.read(cx).composer_text(chat.read(cx).active, cx))
            .ends_with(" stays editable")
    );
    finish(&chat, path, cx);
}

#[gpui::test]
fn early_approval_keys_back_off_from_the_latest_press(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "guard", false);
    cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            view.keys_ready_at = Some(cx.background_executor().now() + KEY_ANSWER_DELAY);
        })
    });
    cx.executor().advance_clock(Duration::from_millis(200));
    assert!(!cx.update(|_, cx| chat.update(cx, |view, cx| view.keys_ready(cx))));
    cx.executor().advance_clock(Duration::from_millis(299));
    cx.update(|_, cx| {
        assert!(chat.read(cx).keys_ready_at.unwrap() > cx.background_executor().now())
    });
    cx.executor().advance_clock(Duration::from_millis(1));
    assert!(cx.update(|_, cx| chat.update(cx, |view, cx| view.keys_ready(cx))));
    cx.update(|_, cx| chat.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    std::fs::remove_dir_all(path).unwrap();
}

/// A checkout hook that prints two lines, as a project's setup might.
const CREATE_COPY: &str = r#"echo "copying files" >&2
echo "postcreate: npm install" >&2"#;
/// A checkout hook that fails, which fails `git worktree add`.
const FAIL_HOOK: &str = r#"echo "npm ERR! code ERESOLVE" >&2
echo "postcreate hook failed (exit 1)" >&2
exit 1"#;

/// The installed `git` with a `HOME` and an identity of its own, so no configuration of the
/// machine reaches the tests.
fn git_tools(path: &Path) -> Tools {
    let home = path.join("git-home");
    std::fs::create_dir_all(&home).unwrap();
    let mut tools = Tools::find();
    tools.env = vec![
        ("HOME".to_string(), home.display().to_string()),
        ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ("GIT_AUTHOR_NAME".to_string(), "Test".to_string()),
        (
            "GIT_AUTHOR_EMAIL".to_string(),
            "test@example.invalid".to_string(),
        ),
        ("GIT_COMMITTER_NAME".to_string(), "Test".to_string()),
        (
            "GIT_COMMITTER_EMAIL".to_string(),
            "test@example.invalid".to_string(),
        ),
    ];
    tools
}

fn git(tools: &Tools, cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new(&tools.git)
        .args(args)
        .current_dir(cwd)
        .envs(tools.env.iter().cloned())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Sets the project's `post-checkout` hook, which `git worktree add` runs in the new worktree.
fn install_hook(project: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    let hook = project.join(".git/hooks/post-checkout");
    std::fs::write(&hook, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
}

pub(crate) fn script(path: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let records: Vec<serde_json::Value> = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../core/tests/fixtures/tool_allow.jsonl"
    ))
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect();
    let mut output = Vec::new();
    for record in &records {
        if record["dir"] == "out" {
            output.push(record["msg"].clone());
            if record["msg"]["type"] == "control_request" {
                break;
            }
        }
    }
    let reply = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../core/tests/fixtures/text_reply.jsonl"
    ))
    .lines()
    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
    .find(|record| record["dir"] == "out" && record["msg"]["type"] == "assistant")
    .unwrap();
    output.insert(1, reply["msg"].clone());
    let mut second = output.last().unwrap().clone();
    second["request_id"] = "second-request".into();
    second["request"]["tool_use_id"] = "second-tool".into();
    second["request"]["input"]["command"] = "touch second.txt".into();
    output.push(second);
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let lines = output
        .iter()
        .map(|message| quote(&message.to_string()))
        .collect::<Vec<_>>()
        .join(" ");
    let log = quote(path.join("answers.jsonl").to_str().unwrap());
    let program = path.join("claude-test");
    std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nIFS= read -r prompt\nprintf '%s\\n' {lines}\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> {log}\n  case \"$line\" in\n    *'\"subtype\":\"interrupt\"'*) printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"terminal_reason\":\"aborted_streaming\"}}';;\n  esac\ndone\n"
            ),
        )
        .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

pub(crate) fn answers(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path.join("answers.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|value: &serde_json::Value| value["type"] == "control_response")
        .collect()
}

pub(crate) async fn wait_answers(cx: &VisualTestContext, path: &Path, count: usize) {
    for _ in 0..300 {
        if answers(path).len() >= count {
            assert_eq!(answers(path).len(), count);
            return;
        }
        cx.background_executor
            .timer(Duration::from_millis(10))
            .await;
    }
    panic!(
        "expected {count} approval responses, got {}",
        answers(path).len()
    );
}

pub(crate) async fn pending(chat: &Entity<ChatView>, cx: &mut VisualTestContext) {
    cx.condition(chat, |view, cx| {
        view.thread()
            .read(cx)
            .transcript()
            .pending_approval()
            .is_some()
    })
    .await;
    cx.run_until_parked();
}

fn request_id(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| {
        chat.read(cx)
            .thread()
            .read(cx)
            .transcript()
            .pending_approval()
            .unwrap()
            .view()
            .tool
            .id
            .clone()
    })
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    cx.simulate_click(bounds.center(), gpui::Modifiers::default());
}

fn let_motion_finish(chat: &Entity<ChatView>, cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.update(|_, cx| chat.update(cx, |_, cx| cx.notify()));
    cx.run_until_parked();
}

fn finish(chat: &Entity<ChatView>, path: PathBuf, cx: &mut VisualTestContext) {
    cx.update(|_, cx| chat.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    std::fs::remove_dir_all(path).unwrap();
}

#[gpui::test]
async fn held_allow_only_answers_once_and_dock_focus_returns(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "held", true);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    assert!(cx.update(|window, cx| chat.read(cx).root.is_focused(window)));
    cx.simulate_keystrokes("cmd-c");
    cx.executor().advance_clock(KEY_ANSWER_DELAY);
    cx.simulate_event(KeyDownEvent {
        keystroke: gpui::Keystroke::parse("cmd-enter").unwrap(),
        is_held: false,
        prefer_character_input: false,
    });
    assert_eq!(request_id(&chat, cx), "second-tool");
    cx.executor()
        .advance_clock(KEY_ANSWER_DELAY + Duration::from_millis(1));
    cx.simulate_event(KeyDownEvent {
        keystroke: gpui::Keystroke::parse("cmd-enter").unwrap(),
        is_held: true,
        prefer_character_input: false,
    });
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("cmd-enter").unwrap(),
    });
    assert_eq!(request_id(&chat, cx), "second-tool");
    wait_answers(cx, &path, 1).await;
    assert_eq!(
        answers(&path)[0]["response"]["response"]["behavior"],
        "allow"
    );
    cx.simulate_keystrokes("cmd-enter");
    wait_answers(cx, &path, 2).await;
    assert!(cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window)));
    cx.simulate_keystrokes("cmd-.");
    cx.condition(&chat, |view, cx| !view.is_busy(cx)).await;
    cx.simulate_keystrokes("cmd-n");
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    finish(&chat, path, cx);
}

#[gpui::test]
async fn escape_closes_a_drawer_before_denying_a_pending_request(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "pending-drawer", true);
    cx.simulate_resize(size(px(700.), px(600.)));
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let first = request_id(&chat, cx);
    cx.simulate_keystrokes("cmd-b");
    assert!(cx.update(|window, cx| window.has_active_sheet(cx)));
    assert!(cx.update(|window, cx| chat.read(cx).drawer_focus.is_focused(window)));
    cx.executor().advance_clock(KEY_ANSWER_DELAY);
    cx.simulate_keystrokes("escape");
    assert!(!cx.update(|_, cx| chat.read(cx).drawer_open));
    assert_eq!(request_id(&chat, cx), first);
    assert!(answers(&path).is_empty());
    cx.simulate_keystrokes("escape");
    assert_eq!(request_id(&chat, cx), "second-tool");
    wait_answers(cx, &path, 1).await;
    assert_eq!(
        answers(&path)[0]["response"]["response"]["behavior"],
        "deny"
    );
    finish(&chat, path, cx);
}

#[gpui::test]
async fn kit_sheet_routes_allow_and_thread_shortcuts(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "kit-sheet-keys", true);
    cx.simulate_resize(size(px(700.), px(600.)));
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    cx.simulate_keystrokes("cmd-b");
    assert!(cx.update(|window, cx| window.has_active_sheet(cx)));
    cx.executor().advance_clock(KEY_ANSWER_DELAY);
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(request_id(&chat, cx), "second-tool");
    wait_answers(cx, &path, 1).await;
    assert_eq!(
        answers(&path)[0]["response"]["response"]["behavior"],
        "allow"
    );
    cx.simulate_keystrokes("cmd-b");
    assert!(!cx.update(|window, cx| window.has_active_sheet(cx)));
    cx.simulate_keystrokes("cmd-b cmd-n");
    assert!(!cx.update(|window, cx| window.has_active_sheet(cx)));
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    assert!(cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window)));
    finish(&chat, path, cx);
}

#[gpui::test]
async fn archive_menu_keeps_approval_keys_and_returns_focus(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "archive-menu", true);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    cx.executor().advance_clock(KEY_ANSWER_DELAY);
    let first = request_id(&chat, cx);
    let bounds = cx.debug_bounds("thread-row").unwrap();
    cx.simulate_mouse_down(
        bounds.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(
        bounds.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    assert!(cx.update(|window, _| overlay_focused(window)));
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(request_id(&chat, cx), first);
    cx.simulate_keystrokes("escape");
    assert!(!cx.update(|window, _| overlay_focused(window)));
    assert_eq!(request_id(&chat, cx), first);
    assert!(answers(&path).is_empty());
    cx.simulate_keystrokes("cmd-n");
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    finish(&chat, path, cx);
}

#[gpui::test]
async fn grants_popover_keeps_approval_keys_and_closes_outside(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "grants-popover", true);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    click(cx, "always");
    wait_answers(cx, &path, 1).await;
    assert_eq!(request_id(&chat, cx), "second-tool");
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).thread().read(cx).grants().len()),
        1
    );
    cx.executor().advance_clock(KEY_ANSWER_DELAY);
    click(cx, "grants-chip");
    assert!(cx.update(|window, _| overlay_focused(window)));
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(request_id(&chat, cx), "second-tool");
    cx.simulate_keystrokes("escape");
    assert!(!cx.update(|window, _| overlay_focused(window)));
    assert_eq!(request_id(&chat, cx), "second-tool");
    assert_eq!(answers(&path).len(), 1);
    assert!(cx.update(|window, cx| chat.read(cx).root.is_focused(window)));
    cx.simulate_keystrokes("cmd-n");
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    cx.simulate_keystrokes("cmd-2");
    assert_eq!(request_id(&chat, cx), "second-tool");
    click(cx, "grants-chip");
    assert!(cx.update(|window, _| overlay_focused(window)));
    let outside = cx.debug_bounds("approval-dock").unwrap().center();
    cx.simulate_click(outside, gpui::Modifiers::default());
    assert!(!cx.update(|window, _| overlay_focused(window)));
    cx.simulate_keystrokes("cmd-n");
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    finish(&chat, path, cx);
}

#[gpui::test]
fn f6_and_tab_traverse_without_editing_the_composer(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "traversal", false);
    cx.simulate_input("keep this draft");
    let focused = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window))
    };
    assert!(focused(cx));
    for (next, previous) in [("f6", "shift-f6"), ("tab", "shift-tab")] {
        cx.simulate_keystrokes(next);
        assert!(!focused(cx));
        cx.simulate_keystrokes(previous);
        assert!(focused(cx));
        assert_eq!(
            cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
            "keep this draft"
        );
    }
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_clicked_answer_leaves_a_fading_card_that_blocks_nothing(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "dock-ghost", true);
    cx.update(|_, cx| cx.set_reduce_motion(false));
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    click(cx, "once");
    assert!(cx.update(|_, cx| chat.read(cx).dock_ghost.is_some()));
    // The next request's own buttons still take the click through the fading card.
    click(cx, "once");
    wait_answers(cx, &path, 2).await;
    cx.executor().advance_clock(crate::ui::EXIT_FADE);
    cx.run_until_parked();
    assert!(cx.update(|_, cx| chat.read(cx).dock_ghost.is_none()));
    finish(&chat, path, cx);
}

#[gpui::test]
async fn reply_copy_and_chrome_clicks_preserve_keyboard_behavior(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "click-focus", true);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    click(cx, "once");
    click(cx, "once");
    wait_answers(cx, &path, 2).await;
    let focused = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window))
    };
    assert!(focused(cx));
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("unchanged".into())));
    let reply = cx.debug_bounds("assistant-reply").unwrap();
    let start = reply.origin + gpui::point(px(2.), px(2.));
    let end = start + gpui::point(px(40.), px(0.));
    cx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_move(end, gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_keystrokes("cmd-c");
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "unchanged"
    );
    cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().focus(window, cx));
    click(cx, "sidebar-toggle");
    assert!(focused(cx));
    cx.simulate_keystrokes("cmd-b cmd-n");
    assert!(focused(cx));
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    let_motion_finish(&chat, cx);
    // The first thread is still finishing in the tray, or already in the list.
    let first = if cx.debug_bounds("tray-row").is_some() {
        "tray-row"
    } else {
        "thread-row"
    };
    click(cx, first);
    assert!(focused(cx));
    assert!(!cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    finish(&chat, path, cx);
}

fn works_in(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> WorksIn {
    cx.update(|_, cx| chat.read(cx).thread().read(cx).works_in())
}

fn notice_title(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| {
        let view = chat.read(cx);
        view.slots[view.active]
            .notice
            .as_ref()
            .map(|notice| notice.title.clone())
    })
}

fn composer_focused(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window))
}

/// Clicks the composer's Local (0) or worktree (1) segment.
fn choose(nth: usize, cx: &mut VisualTestContext) {
    click(cx, ["works-in-local", "works-in-worktree"][nth]);
}

#[gpui::test]
async fn the_new_thread_screen_centres_the_composer_and_a_working_thread_offers_stop(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "new-thread-screen", true);
    cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            view.picker.statuses = vec![picker_status(
                newjen_core::providers::Provider::Claude,
                view.base_config.program.clone(),
            )];
            view.picker.checked = Some(Instant::now());
            cx.notify();
        })
    });
    cx.simulate_resize(size(px(1100.), px(700.)));
    cx.run_until_parked();
    let placeholder = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            chat.read(cx).slots[chat.read(cx).active]
                .placeholder
                .to_string()
        })
    };
    assert_eq!(placeholder(cx), "Ask for a change…");
    let composer = cx.debug_bounds("composer").unwrap();
    assert!(
        composer.bottom() < px(560.) && composer.top() > px(200.),
        "{composer:?}"
    );
    assert!(cx.debug_bounds("works-in-local").is_some());
    assert!(cx.debug_bounds("works-in-worktree").is_some());
    assert!(cx.debug_bounds("agent-picker").is_some());
    assert!(cx.debug_bounds("effort-picker").is_some());
    cx.simulate_input("do a thing");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("stop").is_some());
    assert!(cx.debug_bounds("works-in").is_none());
    assert!(cx.debug_bounds("agent-picker").is_none());
    assert_eq!(placeholder(cx), "Claude is working…");
    finish(&chat, path, cx);
}

#[gpui::test]
fn the_works_in_control_chooses_a_worktree_and_hands_the_keys_back(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "picker", false, Some(CREATE_COPY));
    cx.simulate_input("half typed");
    assert!(cx.debug_bounds("works-in").is_some());
    assert_eq!(works_in(&chat, cx), WorksIn::ProjectFolder);

    choose(1, cx);
    assert_eq!(works_in(&chat, cx), WorksIn::Worktree);
    assert!(composer_focused(&chat, cx));
    cx.simulate_input(", still typing");
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "half typed, still typing"
    );
    choose(0, cx);
    assert_eq!(works_in(&chat, cx), WorksIn::ProjectFolder);
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_worktree_draft_sends_its_first_message_from_its_new_worktree(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "worktree-send", true, Some(CREATE_COPY));
    choose(1, cx);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let (copy, cwd, first, draft) = cx.update(|_, cx| {
        let thread = chat.read(cx).thread().read(cx);
        (
            thread.copy().cloned(),
            thread.cwd().to_path_buf(),
            thread.transcript().items().first().cloned(),
            thread.is_draft(),
        )
    });
    let copy = copy.expect("the thread works in its worktree");
    let worktrees = path
        .canonicalize()
        .unwrap()
        .join(".newjen-worktrees/project");
    assert!(copy.path.starts_with(worktrees), "{copy:?}");
    assert!(copy.branch.starts_with("newjen/"), "{copy:?}");
    assert_eq!(cwd, copy.workdir);
    assert!(!draft);
    assert!(matches!(first, Some(Item::User { text, .. }) if text == "two approvals"));
    assert_eq!(cx.update(|_, cx| chat.read(cx).composer_text(0, cx)), "");
    assert!(cx.debug_bounds("worktree-tag").is_some());
    assert!(cx.debug_bounds("works-in").is_none());
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_failed_worktree_puts_the_message_back_and_try_again_sends_it(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "worktree-fail", true, Some(FAIL_HOOK));
    choose(1, cx);
    cx.simulate_input("add a benchmark");
    cx.simulate_keystrokes("enter");
    cx.condition(&chat, |view, cx| {
        view.thread().read(cx).preparing().is_none() && view.slots[view.active].notice.is_some()
    })
    .await;
    assert_eq!(notice_title(&chat, cx).as_deref(), Some(COPY_FAILED));
    let (detail, output) = cx.update(|_, cx| {
        let view = chat.read(cx);
        let notice = view.slots[view.active].notice.as_ref().unwrap();
        let NoticeKind::CopyFailed { output } = &notice.kind else {
            panic!("not a copy failure");
        };
        (notice.detail.clone(), output.clone())
    });
    assert_eq!(detail, "postcreate hook failed (exit 1)");
    assert_eq!(
        output.last().map(String::as_str),
        Some("npm ERR! code ERESOLVE")
    );
    assert!(
        !path
            .join(".newjen-worktrees/project")
            .read_dir()
            .is_ok_and(|mut left| left.next().is_some()),
        "the half-made worktree is removed"
    );
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "add a benchmark"
    );
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    assert_eq!(works_in(&chat, cx), WorksIn::Worktree);
    assert!(composer_focused(&chat, cx));

    install_hook(&path.join("project"), CREATE_COPY);
    cx.run_until_parked();
    click(cx, "retry");
    cx.condition(&chat, |view, cx| view.thread().read(cx).copy().is_some())
        .await;
    assert_eq!(notice_title(&chat, cx), None);
    assert_eq!(cx.update(|_, cx| chat.read(cx).composer_text(0, cx)), "");
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_worktree_is_made_while_another_thread_works_in_the_project_folder(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup_in(cx, "worktree-beside-busy", true, Some(CREATE_COPY));
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    assert!(cx.update(|_, cx| working(chat.read(cx).thread().read(cx))));
    cx.simulate_keystrokes("cmd-n");
    choose(1, cx);
    cx.simulate_input("rename the keys");
    cx.simulate_keystrokes("enter");
    cx.condition(&chat, |view, cx| view.thread().read(cx).copy().is_some())
        .await;
    assert_eq!(notice_title(&chat, cx), None);
    assert_eq!(cx.update(|_, cx| chat.read(cx).composer_text(1, cx)), "");
    assert!(
        path.join(".newjen-worktrees/project").is_dir(),
        "the busy project folder did not hold it back"
    );
    finish(&chat, path, cx);
}

#[gpui::test]
fn choosing_a_worktree_outside_a_git_repository_says_what_it_needs(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "worktree-no-git", false);
    choose(1, cx);
    assert_eq!(works_in(&chat, cx), WorksIn::Worktree);
    assert_eq!(
        notice_title(&chat, cx).as_deref(),
        Some("Worktrees need a git repository")
    );
    cx.simulate_input("go");
    cx.simulate_keystrokes("enter");
    assert_eq!(cx.update(|_, cx| chat.read(cx).composer_text(0, cx)), "go");
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    choose(0, cx);
    assert_eq!(notice_title(&chat, cx), None);
    finish(&chat, path, cx);
}

#[gpui::test]
fn while_its_copy_is_made_the_thread_shows_it_and_cannot_be_archived(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "making-copy", false);
    let thread = cx.update(|_, cx| chat.read(cx).thread().clone());
    let job = cx.update(|_, cx| {
        thread.update(cx, |thread, cx| {
            thread.set_works_in(WorksIn::Worktree).unwrap();
            let sent = thread.send("fix the parser");
            cx.notify();
            match sent {
                Ok(Sent::MakingCopy(job)) => job,
                other => panic!("expected a copy job, got {other:?}"),
            }
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("making-copy").is_some());
    assert!(cx.debug_bounds("worktree-tag").is_some());
    assert!(cx.debug_bounds("works-in").is_none());
    assert!(cx.update(|window, cx| chat.read(cx).root.is_focused(window)));
    let look =
        cx.update(|_, cx| sidebar::row_look(&row_facts(thread.read(cx), Instant::now()), now_ms()));
    assert_eq!(look.detail, sidebar::MAKING_COPY_DETAIL);
    cx.simulate_input("more");
    cx.simulate_keystrokes("enter");
    assert_eq!(cx.update(|_, cx| chat.read(cx).composer_text(0, cx)), "");

    let id = cx.update(|_, cx| thread.read(cx).id().to_string());
    cx.update(|window, cx| chat.update(cx, |view, cx| view.archive(&id, window, cx)));
    assert_eq!(cx.update(|_, cx| chat.read(cx).slots.len()), 1);

    assert!(cx.debug_bounds("cancel-copy").is_some());
    cx.simulate_keystrokes("cmd-.");
    assert!(cx.update(|_, cx| thread.read(cx).preparing().unwrap().cancelled()));
    cx.run_until_parked();
    assert!(cx.debug_bounds("cancel-copy").is_none());
    let failed = job.run(&Tools::find(), |_| {}).unwrap_err();
    assert_eq!(failed, CopyError::Cancelled);
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.copy_made(&thread, Err(failed), window, cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "fix the parser"
    );
    assert_eq!(notice_title(&chat, cx), None);
    assert!(composer_focused(&chat, cx));
    assert!(cx.debug_bounds("works-in").is_some());
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_turns_tool_calls_fold_into_one_live_work_block(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "work-block", true);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    // The running turn shows its steps, and the call that waits for an answer is one of them.
    assert!(cx.debug_bounds("work-block").is_some());
    assert!(cx.debug_bounds("work-block-steps").is_some());
    assert!(cx.debug_bounds("tool-header").is_none());
    let (calls, hidden, blocks) = cx.update(|_, cx| {
        let view = chat.read(cx);
        let items = view.thread().read(cx).transcript().items();
        // A hidden item is no row of the list: a row of nothing would still take the list's gap.
        let hidden_items = (0..items.len())
            .filter(|ix| view.plan.slot(*ix) == rows::Slot::Hidden)
            .count();
        assert_eq!(
            view.slots[view.active].shown.len(),
            items.len() - hidden_items
        );
        assert!(
            view.slots[view.active]
                .shown
                .iter()
                .all(|ix| view.plan.slot(*ix) != rows::Slot::Hidden)
        );
        let calls = items
            .iter()
            .filter(|item| matches!(item, Item::Tool { .. }))
            .count();
        let slots = (0..items.len()).map(|ix| view.plan.slot(ix));
        (
            calls,
            slots
                .clone()
                .filter(|slot| *slot == rows::Slot::Hidden)
                .count(),
            slots
                .filter(|slot| matches!(slot, rows::Slot::Block(_)))
                .count(),
        )
    });
    assert!(calls >= 1);
    assert_eq!(blocks, 1);
    assert_eq!(hidden, calls - 1);
    // Closing the block hides the steps and opening it brings them back.
    click(cx, "work-block-header");
    cx.run_until_parked();
    assert!(cx.debug_bounds("work-block-steps").is_none());
    click(cx, "work-block-header");
    cx.run_until_parked();
    assert!(cx.debug_bounds("work-block-steps").is_some());
    click(cx, "once");
    click(cx, "once");
    wait_answers(cx, &path, 2).await;
    cx.simulate_keystrokes("cmd-.");
    cx.condition(&chat, |view, cx| !view.is_busy(cx)).await;
    finish(&chat, path, cx);
}

#[gpui::test]
fn the_copy_card_has_a_cancel_that_stops_the_copy(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "copy-card", false);
    let thread = cx.update(|_, cx| chat.read(cx).thread().clone());
    let job = cx.update(|_, cx| {
        thread.update(cx, |thread, cx| {
            thread.set_works_in(WorksIn::Worktree).unwrap();
            let sent = thread.send("fix the parser");
            cx.notify();
            match sent {
                Ok(Sent::MakingCopy(job)) => job,
                other => panic!("expected a copy job, got {other:?}"),
            }
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("making-copy").is_some());
    click(cx, "copy-cancel");
    assert!(cx.update(|_, cx| thread.read(cx).preparing().unwrap().cancelled()));
    let failed = job.run(&Tools::find(), |_| {}).unwrap_err();
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.copy_made(&thread, Err(failed), window, cx)
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("making-copy").is_none());
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "fix the parser"
    );
    finish(&chat, path, cx);
}

#[gpui::test]
fn a_failed_copy_notice_hides_its_log_until_asked(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "failed-log", false);
    let thread = cx.update(|_, cx| chat.read(cx).thread().clone());
    let job = cx.update(|_, cx| {
        thread.update(cx, |thread, _| {
            thread.set_works_in(WorksIn::Worktree).unwrap();
            match thread.send("fix the parser") {
                Ok(Sent::MakingCopy(job)) => job,
                other => panic!("expected a copy job, got {other:?}"),
            }
        })
    });
    drop(job);
    let failed = CopyError::Failed {
        command: "git worktree add".to_string(),
        detail: "postcreate hook failed (exit 1)".to_string(),
        output: vec!["npm ERR! code ERESOLVE".to_string()],
    };
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.copy_made(&thread, Err(failed), window, cx)
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("error-notice").is_some());
    assert!(cx.debug_bounds("retry").is_some());
    let closed = cx.debug_bounds("error-notice").unwrap().size.height;
    click(cx, "show-log");
    cx.run_until_parked();
    assert!(cx.debug_bounds("error-notice").unwrap().size.height > closed);
    click(cx, "show-log");
    cx.run_until_parked();
    assert_eq!(cx.debug_bounds("error-notice").unwrap().size.height, closed);
    finish(&chat, path, cx);
}

#[gpui::test]
fn a_failed_copy_names_what_failed(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "failed-copy", false);
    let thread = cx.update(|_, cx| chat.read(cx).thread().clone());
    let job = cx.update(|_, cx| {
        thread.update(cx, |thread, cx| {
            thread.set_works_in(WorksIn::Worktree).unwrap();
            let sent = thread.send("fix the parser");
            cx.notify();
            match sent {
                Ok(Sent::MakingCopy(job)) => job,
                other => panic!("expected a copy job, got {other:?}"),
            }
        })
    });
    drop(job);
    let failed = CopyError::Failed {
        command: "git worktree add".to_string(),
        detail: "postcreate hook failed (exit 1)".to_string(),
        output: vec!["postcreate hook failed (exit 1)".to_string()],
    };
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.copy_made(&thread, Err(failed), window, cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "fix the parser"
    );
    assert_eq!(notice_title(&chat, cx).as_deref(), Some(COPY_FAILED));
    assert!(composer_focused(&chat, cx));
    finish(&chat, path, cx);
}

/// Right click on the row of a thread that waits on an approval, then "Archive thread". The
/// menu skips Settle, which is disabled for such a thread.
fn archive_from_menu(cx: &mut VisualTestContext) {
    let bounds = cx.debug_bounds("thread-row").unwrap();
    for event in [true, false] {
        if event {
            cx.simulate_mouse_down(
                bounds.center(),
                gpui::MouseButton::Right,
                gpui::Modifiers::default(),
            );
        } else {
            cx.simulate_mouse_up(
                bounds.center(),
                gpui::MouseButton::Right,
                gpui::Modifiers::default(),
            );
        }
    }
    cx.simulate_keystrokes("down down down enter");
}

#[gpui::test]
async fn archiving_a_worktree_thread_asks_then_deletes_its_worktree(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "worktree-archive", true, Some(CREATE_COPY));
    choose(1, cx);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let copy = cx
        .update(|_, cx| chat.read(cx).thread().read(cx).copy().cloned())
        .unwrap();

    archive_from_menu(cx);
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).copy().is_some()));
    assert!(copy.path.exists());

    archive_from_menu(cx);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    assert!(copy.path.exists(), "Return keeps the copy");
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).copy().is_some()));

    archive_from_menu(cx);
    let delete = cx.debug_bounds("delete-copy").unwrap();
    cx.simulate_click(delete.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    for _ in 0..300 {
        if !copy.path.exists() {
            break;
        }
        cx.background_executor
            .timer(Duration::from_millis(10))
            .await;
    }
    assert!(!copy.path.exists(), "the copy was deleted");
    assert!(cx.update(|_, cx| chat.read(cx).save_problem.is_none()));
    assert!(composer_focused(&chat, cx));
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_project_folder_thread_archives_without_asking(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "plain-archive", true);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    archive_from_menu(cx);
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    finish(&chat, path, cx);
}

#[gpui::test]
fn a_project_folder_send_does_not_wait_for_a_worktree_being_made(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "send-beside-worktree", true);
    let copying = cx.update(|_, cx| chat.read(cx).thread().clone());
    let job = cx.update(|_, cx| {
        copying.update(cx, |thread, cx| {
            thread.set_works_in(WorksIn::Worktree).unwrap();
            let sent = thread.send("copy work").unwrap();
            cx.notify();
            match sent {
                Sent::MakingCopy(job) => job,
                _ => panic!("expected copy job"),
            }
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_input("edit project");
    let accepted = cx.update(|window, cx| {
        chat.update(cx, |view, cx| view.send_text("edit project", window, cx))
    });
    assert!(cx.update(|_, cx| copying.read(cx).preparing().is_some()));
    assert!(
        accepted,
        "making a worktree reads only committed history, so the project folder is free"
    );
    assert_eq!(notice_title(&chat, cx), None);
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).status().is_busy()));
    drop(job);
    finish(&chat, path, cx);
}

#[gpui::test]
async fn startup_restores_a_pending_copy_message_to_a_worktree_draft(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "recover-copy", true, Some(CREATE_COPY));
    let (job, tools) = cx.update(|_, cx| {
        let view = chat.read(cx);
        let tools = view.tools.clone();
        let thread = view.thread().clone();
        let job = thread.update(cx, |thread, _| {
            thread.set_works_in(WorksIn::Worktree).unwrap();
            let Sent::MakingCopy(job) = thread.send("my first message").unwrap() else {
                panic!()
            };
            job
        });
        (job, tools)
    });
    let copy = job.run(&tools, |_| {}).unwrap();
    let startup = cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            view.quit(cx).unwrap();
            Startup {
                store: view.store.clone(),
                project: view.project.clone(),
                base_config: view.base_config.clone(),
                threads: vec![],
                problem: None,
                home: None,
                tools: view.tools.clone(),
            }
        })
    });
    let recovered = cx.update(|window, cx| cx.new(|cx| ChatView::new(startup, window, cx)));
    for _ in 0..300 {
        if cx.update(|_, cx| recovered.read(cx).composer_text(0, cx) == "my first message") {
            break;
        }
        cx.background_executor
            .timer(Duration::from_millis(10))
            .await;
    }
    cx.update(|_, cx| {
        let view = recovered.read(cx);
        assert_eq!(view.slots.len(), 1);
        assert_eq!(view.composer_text(0, cx), "my first message");
        assert_eq!(view.thread().read(cx).works_in(), WorksIn::Worktree);
        assert!(view.thread().read(cx).is_draft());
        assert!(!view.thread().read(cx).has_session());
        assert!(view.thread().read(cx).transcript().items().is_empty());
        assert_eq!(
            view.slots[0].notice.as_ref().unwrap().title,
            "Newjen quit while making a worktree. Your message is back."
        );
    });
    assert!(!copy.path.exists());
    finish(&recovered, path, cx);
}

#[gpui::test]
async fn a_copy_completion_after_archive_is_removed_by_the_app(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "late-copy", true, Some(CREATE_COPY));
    let (thread, job, tools) = cx.update(|_, cx| {
        let view = chat.read(cx);
        let thread = view.thread().clone();
        let tools = view.tools.clone();
        let job = thread.update(cx, |thread, _| {
            thread.set_works_in(WorksIn::Worktree).unwrap();
            let Sent::MakingCopy(job) = thread.send("don't send after archive").unwrap() else {
                panic!()
            };
            job
        });
        (thread, job, tools)
    });
    let copy = job.run(&tools, |_| {}).unwrap();
    cx.update(|window, cx| {
        thread.update(cx, |thread, _| {
            assert!(thread.archive().is_none());
        });
        chat.update(cx, |view, cx| {
            view.copy_made(&thread, Ok(copy.clone()), window, cx)
        });
    });
    for _ in 0..300 {
        if !copy.path.exists() {
            break;
        }
        cx.background_executor
            .timer(Duration::from_millis(10))
            .await;
    }
    assert!(!copy.path.exists());
    assert!(!cx.update(|_, cx| thread.read(cx).has_session()));
    finish(&chat, path, cx);
}

#[gpui::test]
async fn an_emptied_recovered_draft_is_discarded_on_quit(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "recover-twice", true, Some(CREATE_COPY));
    let (job, tools) = cx.update(|_, cx| {
        let view = chat.read(cx);
        let tools = view.tools.clone();
        let thread = view.thread().clone();
        let job = thread.update(cx, |thread, _| {
            thread.set_works_in(WorksIn::Worktree).unwrap();
            let Sent::MakingCopy(job) = thread.send("my first message").unwrap() else {
                panic!()
            };
            job
        });
        (job, tools)
    });
    let _copy = job.run(&tools, |_| {}).unwrap();
    let startup = |view: &ChatView| Startup {
        store: view.store.clone(),
        project: view.project.clone(),
        base_config: view.base_config.clone(),
        threads: vec![],
        problem: None,
        home: None,
        tools: view.tools.clone(),
    };
    let s1 = cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            view.quit(cx).unwrap();
            startup(view)
        })
    });
    let second = cx.update(|window, cx| cx.new(|cx| ChatView::new(s1, window, cx)));
    for _ in 0..300 {
        if cx.update(|_, cx| second.read(cx).composer_text(0, cx) == "my first message") {
            break;
        }
        cx.background_executor
            .timer(Duration::from_millis(10))
            .await;
    }
    assert_eq!(
        cx.update(|_, cx| second.read(cx).composer_text(0, cx)),
        "my first message"
    );
    let restored = cx.update(|_, cx| {
        second.update(cx, |view, cx| {
            view.quit(cx).unwrap();
            startup(view)
        })
    });
    let second = cx.update(|window, cx| cx.new(|cx| ChatView::new(restored, window, cx)));
    for _ in 0..300 {
        if cx.update(|_, cx| second.read(cx).composer_text(0, cx) == "my first message") {
            break;
        }
        cx.background_executor
            .timer(Duration::from_millis(10))
            .await;
    }
    assert_eq!(
        cx.update(|_, cx| second.read(cx).composer_text(0, cx)),
        "my first message"
    );
    assert_eq!(notice_title(&second, cx), None);
    cx.update(|window, cx| {
        second.update(cx, |view, cx| {
            let field = view.ensure_field(window, cx);
            field.update(cx, |field, cx| field.set_value("", window, cx));
        })
    });
    let s2 = cx.update(|_, cx| {
        second.update(cx, |view, cx| {
            view.quit(cx).unwrap();
            startup(view)
        })
    });
    let third = cx.update(|window, cx| cx.new(|cx| ChatView::new(s2, window, cx)));
    for _ in 0..100 {
        cx.background_executor
            .timer(Duration::from_millis(10))
            .await;
        cx.run_until_parked();
    }
    assert_eq!(cx.update(|_, cx| third.read(cx).composer_text(0, cx)), "");
    assert_eq!(notice_title(&third, cx), None);
    finish(&third, path, cx);
}

fn picker_status(
    provider: newjen_core::providers::Provider,
    binary: PathBuf,
) -> newjen_core::providers::ProviderStatus {
    use newjen_core::providers::*;
    ProviderStatus {
        provider,
        availability: Availability::Ready {
            binary,
            version: "0.166.0".into(),
            account: Account {
                billing: Billing::Subscription,
                plan: Some("plus".into()),
                email: Some("hidden@example.com".into()),
            },
            catalog: ModelCatalog {
                source: CatalogSource::AccountModels,
                models: vec![ModelOption {
                    id: "sol".into(),
                    label: "GPT-6.1-Sol".into(),
                    description: Some(
                        "Latest workhorse model for coding and everyday work.".into(),
                    ),
                    efforts: vec!["low".into(), "high".into(), "xhigh".into()],
                    default_effort: Some("low".into()),
                    service_tiers: vec![],
                    default_service_tier: None,
                    fast: Some(ServiceTier {
                        id: "fast".into(),
                        label: "Fast".into(),
                        description: "2x speed, increased usage".into(),
                    }),
                }],
            },
            rate_limits: None,
            warnings: vec![],
        },
    }
}

#[gpui::test]
fn agent_picker_lists_every_provider_and_its_keys_choose_model_effort_and_fast(
    cx: &mut TestAppContext,
) {
    use newjen_core::providers::Provider;
    let (chat, path, cx) = setup(cx, "agent-picker", true);
    cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            view.picker.statuses = vec![
                picker_status(Provider::Claude, view.base_config.program.clone()),
                picker_status(Provider::Codex, view.base_config.program.clone()),
            ];
            view.picker.checked = Some(Instant::now());
            cx.notify();
        })
    });
    click(cx, "agent-picker");
    assert!(cx.debug_bounds("provider-menu").is_some());
    assert!(
        cx.debug_bounds("agent-row-3").is_some(),
        "both providers' models are in one list"
    );
    let fast = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            chat.read(cx)
                .thread()
                .read(cx)
                .agent_config()
                .unwrap()
                .speed
        })
    };
    assert!(
        !cx.update(|_, cx| chat
            .read(cx)
            .fast_supported(chat.read(cx).thread().read(cx).agent_config())),
        "the Claude default model has no Fast"
    );
    click(cx, "agent-row-3");
    cx.update(|_, cx| {
        let view = chat.read(cx);
        let thread = view.thread().read(cx);
        let config = thread.agent_config().unwrap();
        assert_eq!(config.provider, Provider::Codex);
        assert_eq!(
            config.model,
            newjen_core::providers::ModelSelection::Model { id: "sol".into() }
        );
    });
    assert!(cx.debug_bounds("provider-menu").is_none());
    click(cx, "effort-picker");
    cx.simulate_keystrokes("down down enter");
    assert_eq!(
        cx.update(|_, cx| chat
            .read(cx)
            .thread()
            .read(cx)
            .agent_config()
            .unwrap()
            .effort
            .clone()),
        newjen_core::EffortSelection::Chosen("high".into())
    );
    assert_eq!(fast(cx), newjen_core::Speed::Normal);
    click(cx, "agent-picker");
    cx.simulate_keystrokes("down enter");
    assert_eq!(fast(cx), newjen_core::Speed::Fast);
    click(cx, "agent-picker");
    cx.simulate_keystrokes("escape");
    assert!(cx.debug_bounds("provider-menu").is_none());
    finish(&chat, path, cx);
}

#[gpui::test]
fn provider_menu_fits_the_smallest_window_and_keeps_refresh_visible(cx: &mut TestAppContext) {
    use newjen_core::providers::Provider;
    let (chat, path, cx) = setup(cx, "agent-menu-room", false);
    cx.simulate_resize(size(px(640.), px(480.)));
    cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            let mut status = picker_status(Provider::Codex, view.base_config.program.clone());
            if let newjen_core::providers::Availability::Ready { catalog, .. } =
                &mut status.availability
            {
                catalog.models[0].label = "GPT-6.1-Sol-Codex-Max-Preview-2026-10-28".into();
                for ix in 0..30 {
                    let mut model = catalog.models[0].clone();
                    model.id = format!("model-{ix}");
                    catalog.models.push(model);
                }
            }
            view.picker.statuses = vec![status];
            view.picker.checked = Some(Instant::now());
            cx.notify();
        })
    });
    click(cx, "agent-picker");
    let menu = cx.debug_bounds("provider-menu").unwrap();
    assert!(menu.top() >= px(0.));
    assert!(menu.bottom() <= px(480.));
    cx.simulate_keystrokes("escape");
    finish(&chat, path, cx);
}

#[gpui::test]
async fn hidden_project_events_keep_the_other_projects_focus_and_unread(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "hidden-project", true);
    cx.simulate_input("approval after hiding the project");
    let other_focus = cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.send(window, cx);
            assert_eq!(view.composer_text(0, cx), "");
            view.set_visible(false, window, cx);
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        focus
    });
    pending(&chat, cx).await;
    assert!(cx.update(|window, _| other_focus.is_focused(window)));
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_unread()));
    cx.update(|window, cx| chat.update(cx, |view, cx| view.set_visible(true, window, cx)));
    assert!(cx.update(|window, cx| chat.read(cx).root.is_focused(window)));
    assert!(!cx.update(|_, cx| chat.read(cx).thread().read(cx).is_unread()));
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_busy_project_in_the_same_repository_does_not_hold_back_a_peers_worktree(
    cx: &mut TestAppContext,
) {
    let (busy, path, cx) = setup_in(cx, "worktree-peer", true, Some(CREATE_COPY));
    let base = path.canonicalize().unwrap();
    let peer = peer_project("project/subproject", false, vec![&busy], &base, cx);
    cx.update(|_, cx| {
        peer.update(cx, |view, cx| {
            view.thread().clone().update(cx, |thread, _| {
                thread.set_works_in(WorksIn::Worktree).unwrap()
            });
            view.set_copy_peers(vec![busy.downgrade()]);
        })
    });
    cx.simulate_input("work in the repository");
    cx.simulate_keystrokes("enter");
    pending(&busy, cx).await;
    assert!(cx.update(|_, cx| busy.read(cx).thread().read(cx).status().is_busy()));
    assert!(cx.update(|_, cx| {
        peer.read(cx)
            .copy_hold(peer.read(cx).thread(), cx)
            .is_none()
    }));
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&busy, path, cx);
}

#[gpui::test]
fn a_project_folder_send_does_not_wait_for_an_overlapping_worktree_job(cx: &mut TestAppContext) {
    let (copying, path, cx) = setup(cx, "review-copy-first", true);
    let child = path.join("child");
    std::fs::create_dir_all(&child).unwrap();
    let (peer, job) = cx.update(|window, cx| {
        let store = copying.read(cx).store.clone();
        let project = store.open_project(&child).unwrap();
        let mut config = SessionConfig::new(&child);
        config.program = copying.read(cx).base_config.program.clone();
        let peer = cx.new(|cx| {
            ChatView::new(
                Startup {
                    store,
                    project,
                    base_config: config,
                    threads: vec![],
                    problem: None,
                    home: None,
                    tools: Tools::find(),
                },
                window,
                cx,
            )
        });
        copying.update(cx, |view, cx| view.set_visible(false, window, cx));
        let job = copying.read(cx).thread().clone().update(cx, |thread, _| {
            thread.set_works_in(WorksIn::Worktree).unwrap();
            match thread.send("create isolated worktree").unwrap() {
                Sent::MakingCopy(job) => job,
                _ => panic!("expected worktree preparation"),
            }
        });
        peer.update(cx, |view, cx| {
            view.set_copy_peers(vec![copying.downgrade()]);
            view.set_visible(true, window, cx);
        });
        (peer, job)
    });
    let (preparing, allowed, started) = cx.update(|window, cx| {
        let preparing = copying.read(cx).thread().read(cx).preparing().is_some();
        let allowed = peer
            .read(cx)
            .copy_hold(peer.read(cx).thread(), cx)
            .is_none();
        let started = peer.update(cx, |view, cx| {
            view.send_text("edit the source now", window, cx)
        });
        (preparing, allowed, started)
    });
    assert!(preparing);
    assert!(allowed);
    drop(job);
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&copying, path, cx);
    assert!(
        started,
        "a worktree being made does not hold the source folder back"
    );
}

#[gpui::test]
async fn switching_projects_cancels_the_archive_dialog(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "review-hidden-archive", true, Some(CREATE_COPY));
    choose(1, cx);
    cx.simulate_input("prepare isolated work");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let id = cx.update(|_, cx| chat.read(cx).thread().read(cx).id().to_string());
    cx.update(|window, cx| chat.update(cx, |view, cx| view.ask_to_archive(&id, window, cx)));
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
    cx.run_until_parked();
    let dialog_focus = cx.update(|window, cx| window.focused(cx).unwrap());
    let other = cx.update(|window, cx| {
        chat.update(cx, |view, cx| view.set_visible(false, window, cx));
        let other = cx.focus_handle();
        window.focus(&other, cx);
        other
    });
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    drop(dialog_focus);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let (visible, draft, kept_focus) = cx.update(|window, cx| {
        (
            chat.read(cx).visible,
            chat.read(cx).thread().read(cx).is_draft(),
            other.is_focused(window),
        )
    });
    assert!(!visible);
    finish(&chat, path, cx);
    assert!(!draft, "switching cancels the archive confirmation");
    assert!(kept_focus, "hidden archive completion must not take focus");
}

fn open_copy_as_project(
    owner: &Entity<ChatView>,
    folder: &Path,
    cx: &mut VisualTestContext,
) -> Entity<ChatView> {
    cx.update(|window, cx| {
        let view = owner.read(cx);
        let store = view.store.clone();
        let tools = view.tools.clone();
        let mut config = SessionConfig::new(folder);
        config.program = view.base_config.program.clone();
        let project = store.open_project(folder).unwrap();
        let peer = cx.new(|cx| {
            ChatView::new(
                Startup {
                    store,
                    project,
                    base_config: config,
                    threads: vec![],
                    problem: None,
                    home: None,
                    tools,
                },
                window,
                cx,
            )
        });
        peer.update(cx, |view, cx| {
            view.set_copy_peers(vec![owner.downgrade()]);
            view.set_visible(false, window, cx);
        });
        owner.update(cx, |view, cx| {
            view.set_copy_peers(vec![peer.downgrade()]);
            view.set_visible(true, window, cx);
        });
        peer
    })
}

#[gpui::test]
async fn archive_preserves_a_copy_used_by_another_loaded_project(cx: &mut TestAppContext) {
    let (owner, path, cx) = setup_in(cx, "archive-copy-in-use", true, Some(CREATE_COPY));
    choose(1, cx);
    cx.simulate_input("prepare work in a copy");
    cx.simulate_keystrokes("enter");
    pending(&owner, cx).await;
    let (copy, id) = cx.update(|_, cx| {
        let thread = owner.read(cx).thread().read(cx);
        (thread.copy().cloned().unwrap(), thread.id().to_string())
    });
    let peer = open_copy_as_project(&owner, &copy.workdir, cx);
    cx.update(|window, cx| owner.update(cx, |view, cx| view.ask_to_archive(&id, window, cx)));
    cx.run_until_parked();
    assert!(
        !cx.update(|window, cx| window.has_active_dialog(cx)),
        "an idle loaded project keeps its folder"
    );
    cx.update(|window, cx| peer.update(cx, |view, cx| view.send_text("use the copy", window, cx)));
    pending(&peer, cx).await;
    cx.update(|window, cx| owner.update(cx, |view, cx| view.archive(&id, window, cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| owner.read(cx).thread().read(cx).id().to_string()),
        id
    );
    assert!(copy.path.is_dir());
    assert!(cx.update(|_, cx| peer.read(cx).thread().read(cx).status().is_busy()));
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&owner, path, cx);
}

#[gpui::test]
async fn archive_rechecks_copy_users_before_deleting(cx: &mut TestAppContext) {
    let (owner, path, cx) = setup_in(cx, "archive-copy-race", true, Some(CREATE_COPY));
    choose(1, cx);
    cx.simulate_input("prepare work in a copy");
    cx.simulate_keystrokes("enter");
    pending(&owner, cx).await;
    let (copy, id) = cx.update(|_, cx| {
        let thread = owner.read(cx).thread().read(cx);
        (thread.copy().cloned().unwrap(), thread.id().to_string())
    });
    cx.update(|window, cx| owner.update(cx, |view, cx| view.ask_to_archive(&id, window, cx)));
    cx.run_until_parked();
    let peer = open_copy_as_project(&owner, &copy.workdir, cx);
    cx.update(|window, cx| owner.update(cx, |view, cx| view.archive(&id, window, cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| owner.read(cx).thread().read(cx).id().to_string()),
        id
    );
    assert!(copy.path.is_dir());
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&owner, path, cx);
}

#[gpui::test]
async fn a_denial_note_is_sent_from_its_own_field_and_cleared_for_the_next_request(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "denial-note", true);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    cx.update(|window, cx| {
        let field = chat.read(cx).denial_note.clone().expect("denial field");
        field.read(cx).focus_handle(cx).focus(window, cx);
    });
    cx.simulate_input("Use a temporary folder instead.");
    click(cx, "deny");
    wait_answers(cx, &path, 1).await;
    assert_eq!(
        answers(&path)[0]["response"]["response"]["message"],
        "Use a temporary folder instead."
    );
    assert_eq!(request_id(&chat, cx), "second-tool");
    assert!(cx.update(|_, cx| {
        chat.read(cx)
            .denial_note
            .as_ref()
            .unwrap()
            .read(cx)
            .value()
            .is_empty()
    }));
    finish(&chat, path, cx);
}

/// A second project in the same window, as `ProjectView` makes one: it sees `peers` and is
/// out of sight until shown.
fn peer_project(
    name: &str,
    with_approvals: bool,
    peers: Vec<&Entity<ChatView>>,
    path: &Path,
    cx: &mut VisualTestContext,
) -> Entity<ChatView> {
    let folder = path.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let store = cx.update(|_, cx| peers[0].read(cx).store.clone());
    let project = store.open_project(&folder).unwrap();
    let mut base_config = SessionConfig::new(&folder);
    if with_approvals {
        base_config.program = script(path);
    }
    let startup = Startup {
        store,
        project,
        base_config,
        threads: vec![],
        problem: None,
        home: None,
        tools: Tools::find(),
    };
    cx.update(|window, cx| {
        let peer = cx.new(|cx| ChatView::new(startup, window, cx));
        peer.update(cx, |view, cx| {
            view.set_copy_peers(peers.iter().map(|peer| peer.downgrade()).collect());
            view.set_visible(false, window, cx);
        });
        peer
    })
}

#[gpui::test]
async fn cmd_j_goes_to_the_next_thread_that_needs_you_here_then_in_another_project(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "jump-needs-you", true);
    cx.simulate_input("first thread");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let first = cx.update(|_, cx| chat.read(cx).thread().read(cx).id().to_string());
    // The second waiting thread is in another loaded project, out of sight.
    let peer = peer_project("other", true, vec![&chat], &path, cx);
    cx.update(|window, cx| {
        peer.update(cx, |view, cx| {
            assert!(view.send_text("second thread", window, cx));
        })
    });
    pending(&peer, cx).await;
    let second = cx.update(|_, cx| peer.read(cx).thread().read(cx).id().to_string());
    let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let seen = opened.clone();
    cx.update(|_, cx| {
        chat.update(cx, |view, _| {
            view.set_copy_peers(vec![peer.downgrade()]);
            view.set_thread_opener(std::rc::Rc::new(move |id, _, _| {
                seen.borrow_mut().push(id.to_string())
            }));
        })
    });
    // Only the thread on screen needs you here: ⌘J goes to the other project's.
    cx.simulate_keystrokes("cmd-j");
    assert_eq!(opened.borrow().as_slice(), std::slice::from_ref(&second));
    // A new draft takes the screen: ⌘J goes back to the waiting thread, in this project.
    cx.simulate_keystrokes("cmd-n");
    cx.update(|_, cx| {
        let view = chat.read(cx);
        assert!(view.thread().read(cx).is_draft());
        assert_eq!(view.thread_order(cx), vec![0, 1]);
        assert_eq!(view.needs_you(cx), vec![false, true]);
    });
    cx.simulate_keystrokes("cmd-j");
    cx.update(|_, cx| {
        let view = chat.read(cx);
        assert_eq!(view.thread().read(cx).id(), first);
        assert!(!view.terminal_open);
    });
    assert_eq!(opened.borrow().len(), 1);
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&chat, path, cx);
}

#[gpui::test]
fn closing_the_drawer_restores_focus_to_the_visible_panel(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "drawer-panel-focus", false);
    cx.executor().allow_parking();
    cx.simulate_resize(size(px(700.), px(600.)));
    cx.simulate_keystrokes("cmd-b");
    assert!(cx.update(|window, cx| window.has_active_sheet(cx)));
    cx.simulate_keystrokes("cmd-`");
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(!window.has_active_sheet(cx));
        let view = chat.read(cx);
        assert!(view.terminal_open);
        assert!(
            view.slots[view.active]
                .terminal
                .as_ref()
                .unwrap()
                .focus_handle(cx)
                .is_focused(window)
        );
        assert_eq!(view.quit_blockers(cx).terminals, 1);
    });
    finish(&chat, path, cx);
}

#[gpui::test]
fn the_terminal_is_a_panel_under_the_chat_with_its_own_close_button(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "terminal-panel", false);
    cx.executor().allow_parking();
    cx.simulate_resize(size(px(1000.), px(700.)));
    cx.run_until_parked();
    // No tab row over the chat, no status pill; the header has the button.
    assert!(cx.debug_bounds("conversation-tab").is_none());
    assert!(cx.debug_bounds("terminal-tab").is_none());
    assert!(cx.debug_bounds("terminal-drawer").is_none());
    assert!(cx.debug_bounds("where-chip").is_some());
    let composer_before = cx.debug_bounds("composer").map(|bounds| bounds.size.width);
    click(cx, "terminal-toggle");
    cx.run_until_parked();
    let drawer = cx.debug_bounds("terminal-drawer").expect("the panel");
    assert!(drawer.size.height <= px(250.5), "{drawer:?}");
    assert!(drawer.size.height > px(150.), "{drawer:?}");
    // The chat stays above it, and the composer is still there to type in.
    assert!(cx.debug_bounds("where-chip").is_some());
    assert_eq!(
        cx.debug_bounds("composer").map(|bounds| bounds.size.width),
        composer_before
    );
    let composer = cx.debug_bounds("composer").expect("the composer stays");
    assert!(composer.bottom() <= drawer.top() + px(1.));
    cx.update(|window, cx| {
        let view = chat.read(cx);
        assert!(view.terminal_open);
        assert!(
            view.slots[view.active]
                .terminal
                .as_ref()
                .unwrap()
                .focus_handle(cx)
                .is_focused(window)
        );
    });
    // The same button, and ⌘`, hide it; the shell keeps running.
    click(cx, "terminal-toggle");
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminal-drawer").is_none());
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).quit_blockers(cx).terminals),
        1
    );
    cx.simulate_keystrokes("cmd-`");
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminal-drawer").is_some());
    // The × stops the shell and takes the panel away.
    click(cx, "close-terminal");
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminal-drawer").is_none());
    cx.update(|_, cx| {
        let view = chat.read(cx);
        assert!(!view.terminal_open);
        assert!(view.slots[view.active].terminal.is_none());
        assert_eq!(view.quit_blockers(cx).terminals, 0);
    });
    finish(&chat, path, cx);
}

#[gpui::test]
fn archiving_the_selected_terminal_returns_to_a_visible_conversation(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "archive-terminal-panel", false);
    cx.executor().allow_parking();
    cx.simulate_keystrokes("cmd-`");
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let id = view.thread().read(cx).id().to_string();
            view.archive(&id, window, cx);
            assert!(!view.terminal_open);
            assert!(view.slots[view.active].terminal.is_none());
        })
    });
    cx.run_until_parked();
    assert!(cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window)));
    finish(&chat, path, cx);
}

#[gpui::test]
fn a_terminal_keeps_its_draft_and_working_folder_until_closed(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "terminal-folder-retention", false);
    cx.executor().allow_parking();
    cx.simulate_keystrokes("cmd-`");
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.toggle_terminal(window, cx);
            view.choose_works_in(WorksIn::Worktree, window, cx);
            assert_eq!(view.thread().read(cx).works_in(), WorksIn::ProjectFolder);
            assert_eq!(
                view.slots[view.active].notice.as_ref().unwrap().title,
                "Close the terminal before changing folders"
            );
        })
    });
    cx.simulate_keystrokes("cmd-n");
    cx.update(|_, cx| {
        let view = chat.read(cx);
        assert_eq!(view.slots.len(), 2);
        assert!(view.slots[1].terminal.is_some());
    });
    finish(&chat, path, cx);
}

/// The repository root and one of its subfolders are open as two projects. Both may have a
/// thread working in the project folder at once, but neither may commit while the other works,
/// because a commit takes every file in the folder.
#[gpui::test]
async fn project_folder_threads_in_one_repository_work_together_but_commit_one_at_a_time(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup_in(cx, "two-projects-one-repo", true, Some(CREATE_COPY));
    cx.simulate_input("thread A in the root");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let base = path.canonicalize().unwrap();
    let peer = peer_project("project/packages/web", true, vec![&chat], &base, cx);
    cx.update(|_, cx| chat.update(cx, |view, _| view.set_copy_peers(vec![peer.downgrade()])));
    let b_id = cx.update(|_, cx| peer.read(cx).thread().read(cx).id().to_string());
    assert_eq!(
        cx.update(|_, cx| peer.read(cx).git_gate(&b_id, cx)),
        Some("Another thread is working in this folder")
    );
    let went_out = cx.update(|window, cx| {
        peer.update(cx, |view, cx| {
            view.send_text("thread B in the subfolder", window, cx)
        })
    });
    assert!(went_out, "a second project-folder thread may work too");
    assert_eq!(notice_title(&peer, cx), None);
    pending(&peer, cx).await;
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&chat, path, cx);
}

/// A stand-in for `codex app-server` that replays `fixture` (see `newjen-fake-codex`): one
/// line read per message Newjen sends, each reply printed in order. Newjen numbers its
/// requests from 1, as the fixtures do.
fn codex_script(path: &Path, project: &Path, fixture: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let records = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../core/tests/fixtures/codex")
            .join(fixture),
    )
    .unwrap();
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let mut lines = vec!["#!/bin/sh".to_string()];
    for record in records.lines().filter(|line| !line.trim().is_empty()) {
        let record: serde_json::Value = serde_json::from_str(record).unwrap();
        let message = record["msg"]
            .to_string()
            .replace("$CWD", project.to_str().unwrap());
        if record["dir"] == "in" {
            lines.push("IFS= read -r line || exit 0".to_string());
        } else {
            lines.push(format!("printf '%s\\n' {}", quote(&message)));
        }
    }
    lines.push("while IFS= read -r line; do :; done".to_string());
    let program = path.join("codex-test");
    std::fs::write(&program, lines.join("\n") + "\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

/// A `ChatView` whose threads talk to the Codex stand-in replaying `fixture`.
fn setup_codex<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    fixture: &str,
) -> (Entity<ChatView>, PathBuf, &'a mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        keys::bind(cx);
        cx.set_reduce_motion(true);
    });
    cx.executor().allow_parking();
    let path = std::env::temp_dir().join(format!(
        "newjen-input-{name}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let store = Store::open_in(&path).unwrap();
    let project = store.open_project(&path).unwrap();
    let mut base_config = SessionConfig::new(&path);
    base_config.agent =
        newjen_core::agent::AgentConfig::cli_default(newjen_core::providers::Provider::Codex);
    base_config.program = codex_script(&path, &path, fixture);
    let startup = Startup {
        store,
        project,
        base_config,
        threads: vec![],
        problem: None,
        home: None,
        tools: Tools::find(),
    };
    let mut chat = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| ChatView::new(startup, window, cx));
        chat = Some(view.clone());
        Root::new(view, window, cx)
    });
    (chat.unwrap(), path, cx)
}

/// Codex takes no text with a denial, so the note comes back to the composer. It must not
/// replace what the user had typed there meanwhile.
#[gpui::test]
async fn a_kept_denial_note_follows_the_composer_draft(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_codex(cx, "codex-kept-note", "runtime-command-decline.jsonl");
    cx.simulate_input("Do the command");
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.send(window, cx);
            assert_eq!(view.composer_text(0, cx), "");
            let field = view.ensure_field(window, cx);
            field.update(cx, |field, cx| {
                field.set_value("my half-written follow-up", window, cx)
            });
        })
    });
    pending(&chat, cx).await;
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "my half-written follow-up"
    );
    cx.update(|window, cx| {
        let field = chat.read(cx).denial_note.clone().expect("denial field");
        field.read(cx).focus_handle(cx).focus(window, cx);
    });
    cx.simulate_input("Not in this folder.");
    click(cx, "deny");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "my half-written follow-up\n\nNot in this folder."
    );
    assert_eq!(
        notice_title(&chat, cx).as_deref(),
        Some("The note was not sent")
    );
    finish(&chat, path, cx);
}

/// The review panel takes the composer's place, so the keyboard must stay with the view:
/// ⌘1–9 switch threads (closing the panel) as they do anywhere else.
#[gpui::test]
fn thread_jump_keys_work_while_the_review_panel_is_open(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "review-keys", false);
    cx.simulate_input("first thread");
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_input("second thread");
    assert_eq!(cx.update(|_, cx| chat.read(cx).active), 0);
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let id = view.thread().read(cx).id().to_string();
            view.show_review(id, Scope::All, loaded(""), window, cx);
        })
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| chat.read(cx).review_panel.is_some()));
    assert!(
        cx.update(|window, cx| chat.read(cx).root.is_focused(window)),
        "the root holds the keyboard while the diff replaces the composer"
    );
    cx.simulate_keystrokes("cmd-2");
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).active),
        1,
        "⌘2 switches threads with the review panel open"
    );
    assert!(cx.update(|_, cx| chat.read(cx).review_panel.is_none()));
    assert!(composer_focused(&chat, cx));
    cx.simulate_input(" continues");
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(1, cx)),
        "first thread continues"
    );
    finish(&chat, path, cx);
}

/// Closing the review panel gives the keyboard back to the composer, which took its place.
#[gpui::test]
fn closing_the_review_panel_returns_the_keyboard_to_the_composer(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "review-close-focus", false);
    cx.simulate_input("a draft");
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let id = view.thread().read(cx).id().to_string();
            view.show_review(id, Scope::All, loaded(""), window, cx);
        })
    });
    cx.run_until_parked();
    assert!(cx.update(|window, cx| chat.read(cx).root.is_focused(window)));
    click(cx, "close-review");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| chat.read(cx).review_panel.is_none()));
    assert!(
        composer_focused(&chat, cx),
        "the composer has the keyboard again once the panel closes"
    );
    cx.simulate_input(" continues");
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "a draft continues"
    );
    finish(&chat, path, cx);
}

#[gpui::test]
async fn review_completion_during_saving_does_not_disable_keep_open(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "review-during-quit", true);
    cx.executor().allow_parking();
    let quitting = cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let row = newjen_core::store::ThreadRow {
                id: "review-probe".into(),
                project_id: view.project.id,
                workdir: path.clone(),
                title: "Review probe".into(),
                provider: "claude".into(),
                conversation: newjen_core::agent::ConversationState::Established(
                    "6f1c1f4e-2b1e-4c8e-9a51-0d9a7c2f3b10".into(),
                ),
                model: newjen_core::providers::ModelSelection::CliDefault,
                effort: newjen_core::agent::EffortSelection::CliDefault,
                speed: newjen_core::agent::Speed::Normal,
                status: Status::Done,
                unread: false,
                created_at: 1,
                updated_at: 1,
                archived_at: None,
                copy: None,
                review_base: Some("a".repeat(40)),
                landing_resolved: false,
                snapshot_needed: false,
                list: Default::default(),
            };
            view.store.save_thread(row.clone());
            assert!(view.store.flush(Duration::from_secs(5)));
            let thread = Thread::load(
                view.store.clone(),
                &view.project,
                row,
                view.base_config.clone(),
            )
            .unwrap();
            view.slots[0].thread = cx.new(|_| thread);
            view.open_review(window, cx);
            assert!(view.review_loading);
            view.set_quit_saving(true, cx);
            view.prepare_quit(window, cx)
        })
    });
    let store = cx.update(|_, cx| chat.read(cx).store.clone());
    let (done, wait) = async_channel::bounded(1);
    std::thread::spawn(move || {
        done.send_blocking(thread::finish_quit(&store, quitting))
            .unwrap();
    });
    wait.recv().await.unwrap().unwrap();
    cx.run_until_parked();
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.set_quit_saving(false, cx);
            let sent = view.send_text("preserved user work", window, cx);
            assert!(sent, "the retained composer can send again");
            assert!(
                !view.review_loading,
                "Keep open must clear completed review loading so the composer can send again"
            );
        })
    });
    finish(&chat, path, cx);
}

/// The review lists every file of the patch and shows one at a time.
#[gpui::test]
fn the_review_shows_the_selected_file_only(cx: &mut TestAppContext) {
    let patch = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1,2 @@\n keep\n+added\n\
                 diff --git a/b.toml b/b.toml\n--- a/b.toml\n+++ b/b.toml\n@@ -1 +1 @@\n-old\n+new\n";
    let (chat, path, cx) = setup(cx, "review-files", false);
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let id = view.thread().read(cx).id().to_string();
            view.show_review(id, Scope::All, loaded(patch), window, cx);
        })
    });
    cx.run_until_parked();
    let shown = cx.update(|_, cx| chat.read(cx).review_shown(cx));
    assert_eq!(shown, Some((0, 2, vec!["src/a.rs".to_string()])));
    cx.update(|_, cx| chat.update(cx, |view, cx| view.select_review_file(1, cx)));
    cx.run_until_parked();
    let shown = cx.update(|_, cx| chat.read(cx).review_shown(cx));
    assert_eq!(shown, Some((1, 2, vec!["b.toml".to_string()])));
    finish(&chat, path, cx);
}

#[gpui::test]
async fn the_project_chevron_opens_a_menu_that_switches_projects(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "project-menu", false);
    let chosen = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = chosen.clone();
    cx.update(|_, cx| {
        chat.update(cx, |view, _| {
            view.set_project_chooser(std::rc::Rc::new(move |_, _| counted.set(counted.get() + 1)))
        })
    });
    cx.run_until_parked();
    click(cx, "project-menu");
    cx.run_until_parked();
    assert!(cx.update(|window, _| overlay_focused(window)));
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert_eq!(chosen.get(), 1);
    finish(&chat, path, cx);
}

#[gpui::test]
fn a_dismissed_notification_note_stays_dismissed(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "attention-note", false);
    assert!(cx.debug_bounds("attention-note").is_none());
    cx.update(|_, cx| chat.update(cx, |view, cx| view.set_attention_note(Some("Off"), cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("attention-note").is_some());
    click(cx, "dismiss-attention-note");
    cx.run_until_parked();
    assert!(cx.debug_bounds("attention-note").is_none());
    cx.update(|_, cx| chat.update(cx, |view, cx| view.set_attention_note(Some("Off"), cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("attention-note").is_none());
    cx.update(|_, cx| chat.update(cx, |view, cx| view.set_attention_note(Some("Other"), cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("attention-note").is_some());
    finish(&chat, path, cx);
}

#[gpui::test]
async fn the_sidebar_lists_threads_and_its_usage_row_opens_the_usage_view(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "sidebar-groups", true);
    cx.run_until_parked();
    for selector in ["switch-project", "new-thread", "usage-row", "thread-row"] {
        assert!(cx.debug_bounds(selector).is_some(), "missing {selector}");
    }
    // The first message starts a thread that waits for an approval. It is on screen, so it stays
    // where it was opened until the user leaves it, then it is in the Queue.
    cx.simulate_input("first thread");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    cx.update(|_, cx| {
        let view = chat.read(cx);
        assert_eq!(
            view.sidebar_layout(cx).entries,
            vec![sidebar::Entry::Thread(0)]
        );
    });
    cx.simulate_keystrokes("cmd-n");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let view = chat.read(cx);
        assert_eq!(
            view.sidebar_layout(cx).entries,
            vec![
                sidebar::Entry::Thread(0),
                sidebar::Entry::Header {
                    section: sidebar::Section::Queue,
                    count: 1,
                    needs: 1
                },
                sidebar::Entry::Thread(1)
            ]
        );
    });
    let_motion_finish(&chat, cx);
    assert!(cx.debug_bounds("thread-row").is_some());
    // The usage row opens the Usage view in place of the thread, and a thread row returns.
    assert!(cx.debug_bounds("usage-view").is_none());
    click(cx, "usage-row");
    cx.run_until_parked();
    assert!(cx.debug_bounds("usage-view").is_some());
    click(cx, "thread-row");
    cx.run_until_parked();
    assert!(cx.debug_bounds("usage-view").is_none());
    finish(&chat, path, cx);
}

/// Adds a saved thread that is idle, with a row of its own in the list, and returns its id.
/// `unread` puts it in the Queue, otherwise it is read.
fn add_idle_thread(
    chat: &Entity<ChatView>,
    cx: &mut VisualTestContext,
    title: &str,
    unread: bool,
    list: newjen_core::store::ListState,
) -> String {
    add_idle_thread_in(chat, cx, title, unread, list, None)
}

fn add_idle_thread_in(
    chat: &Entity<ChatView>,
    cx: &mut VisualTestContext,
    title: &str,
    unread: bool,
    list: newjen_core::store::ListState,
    copy: Option<newjen_core::worktree::Worktree>,
) -> String {
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let id = format!("5a0c1f4e-2b1e-4c8e-9a51-{:012}", view.slots.len());
            let row = newjen_core::store::ThreadRow {
                id: id.clone(),
                project_id: view.project.id,
                workdir: view.project.path.clone(),
                title: title.into(),
                provider: "claude".into(),
                conversation: newjen_core::agent::ConversationState::Established(
                    "6f1c1f4e-2b1e-4c8e-9a51-0d9a7c2f3b10".into(),
                ),
                model: newjen_core::providers::ModelSelection::CliDefault,
                effort: newjen_core::agent::EffortSelection::CliDefault,
                speed: newjen_core::agent::Speed::Normal,
                status: Status::Done,
                unread,
                created_at: 1,
                updated_at: 1,
                archived_at: None,
                copy,
                review_base: None,
                landing_resolved: false,
                snapshot_needed: false,
                list,
            };
            view.store.save_thread(row.clone());
            assert!(view.store.flush(Duration::from_secs(5)));
            let thread = Thread::load(
                view.store.clone(),
                &view.project,
                row,
                view.base_config.clone(),
            )
            .unwrap();
            let at = view.slots.len();
            view.add_slot(thread, at, window, cx);
            cx.notify();
            id
        })
    })
}

fn settled_list(at: i64) -> newjen_core::store::ListState {
    newjen_core::store::ListState {
        settled_at: Some(at),
        landed_at: at,
        ..Default::default()
    }
}

fn fox(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> (bool, String) {
    cx.update(|_, cx| {
        let fox = chat.read(cx).fox.read(cx);
        (fox.is_shown(), fox.title().to_string())
    })
}

#[gpui::test]
async fn the_fox_game_fills_a_list_with_nothing_to_read_and_yields_to_the_first_row(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "fox-game", true);
    cx.run_until_parked();
    assert_eq!(fox(&chat, cx), (true, "All clear".to_string()));
    assert!(cx.debug_bounds("fox-playfield").is_some());

    cx.simulate_input("a draft of its own");
    cx.simulate_keystrokes("cmd-n");
    let_motion_finish(&chat, cx);
    cx.update(|_, cx| assert_eq!(chat.read(cx).slots.len(), 2));
    assert_eq!(
        fox(&chat, cx),
        (true, "All clear".to_string()),
        "drafts are not rows to read"
    );

    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let_motion_finish(&chat, cx);
    assert!(!fox(&chat, cx).0, "the thread on screen is a row");
    cx.update(|_, cx| {
        let fox = chat.read(cx).fox.clone();
        fox.update(cx, |fox, _| fox.advance(1., true));
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("fox-playfield").is_none(),
        "the game has faded away"
    );

    click(cx, "once");
    click(cx, "once");
    wait_answers(cx, &path, 2).await;
    cx.simulate_keystrokes("cmd-n");
    let_motion_finish(&chat, cx);
    cx.update(|_, cx| {
        let layout = chat.read(cx).sidebar_layout(cx);
        assert_eq!(layout.in_flight.len(), 1);
        assert!(layout.entries.iter().all(|entry| matches!(
            entry,
            sidebar::Entry::Thread(ix) if chat.read(cx).slots[*ix].thread.read(cx).is_draft()
        )));
    });
    assert_eq!(
        fox(&chat, cx),
        (true, sidebar::NOTHING_NEEDS_YOU.to_string()),
        "a thread in flight leaves the list empty"
    );

    cx.simulate_input("second thread");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    cx.simulate_keystrokes("cmd-n");
    let_motion_finish(&chat, cx);
    cx.update(|_, cx| {
        let layout = chat.read(cx).sidebar_layout(cx);
        assert!(layout.entries.iter().any(|entry| matches!(
            entry,
            sidebar::Entry::Header {
                section: sidebar::Section::Queue,
                ..
            }
        )));
    });
    assert!(!fox(&chat, cx).0, "a Queue row hides the game");
    finish(&chat, path, cx);
}

#[gpui::test]
async fn the_fox_game_takes_the_keyboard_only_when_clicked_and_gives_it_back(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "fox-focus", true);
    cx.run_until_parked();
    let composer = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window))
    };
    let game = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            chat.read(cx)
                .fox
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        })
    };
    assert!(fox(&chat, cx).0);
    assert!(composer(cx) && !game(cx), "showing the game keeps the keys");
    cx.simulate_input("two approvals");
    assert!(composer(cx) && !game(cx));

    click(cx, "fox-playfield");
    assert!(game(cx) && !composer(cx), "a click gives it the keys");

    let row = sidebar::Listed {
        draft: false,
        placement: Placement::Queue,
        waits: false,
        list: Default::default(),
    };
    cx.update(|window, cx| chat.update(cx, |view, cx| view.sync_fox(&[row], window, cx)));
    assert!(
        composer(cx) && !game(cx),
        "the keys go back as soon as the game yields"
    );
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_settled_thread_waits_in_a_panel_that_esc_closes_and_archive_all_asks_first(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "settled-panel", true);
    cx.simulate_input("keep waiting");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let id = add_idle_thread(&chat, cx, "park me", false, Default::default());
    cx.update(|_, cx| chat.update(cx, |view, cx| view.settle_thread(&id, cx)));
    cx.run_until_parked();
    let_motion_finish(&chat, cx);
    cx.update(|_, cx| {
        let layout = chat.read(cx).sidebar_layout(cx);
        assert_eq!(layout.settled, vec![1]);
        assert_eq!(layout.entries, vec![sidebar::Entry::Thread(0)]);
    });
    assert!(cx.debug_bounds("sheet").is_none());

    click(cx, "disclosure-settled");
    cx.run_until_parked();
    assert!(cx.debug_bounds("sheet").is_some());
    assert!(cx.debug_bounds("sheet-row").is_some());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("sheet").is_none());
    assert!(
        cx.update(|_, cx| chat
            .read(cx)
            .thread()
            .read(cx)
            .transcript()
            .pending_approval()
            .is_some()),
        "Esc closed the panel and denied nothing"
    );

    click(cx, "disclosure-settled");
    cx.run_until_parked();
    click(cx, "sheet-bulk");
    cx.run_until_parked();
    assert!(cx.debug_bounds("sheet-archive").is_some());
    click(cx, "sheet-cancel");
    cx.run_until_parked();
    assert!(cx.debug_bounds("sheet-archive").is_none());
    assert!(cx.debug_bounds("sheet").is_some(), "cancel keeps the panel");
    assert!(cx.debug_bounds("sheet-bulk").is_some());
    click(cx, "sheet-bulk");
    cx.run_until_parked();
    click(cx, "sheet-archive");
    cx.run_until_parked();
    assert!(cx.debug_bounds("sheet").is_none());
    cx.update(|_, cx| {
        let view = chat.read(cx);
        assert_eq!(view.slots.len(), 1);
        assert!(
            view.thread()
                .read(cx)
                .transcript()
                .pending_approval()
                .is_some()
        );
        assert!(view.sidebar_layout(cx).settled.is_empty());
    });
    let_motion_finish(&chat, cx);
    assert!(cx.debug_bounds("disclosure-settled").is_none());
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_row_offers_settle_only_to_a_thread_that_can_be_put_away(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "settle-offer", true);
    cx.simulate_input("keep waiting");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    assert!(cx.debug_bounds("row-snooze").is_some());
    assert!(
        cx.debug_bounds("row-settle").is_none(),
        "a thread that waits on you stays in the list"
    );
    add_idle_thread(&chat, cx, "quiet", false, Default::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("row-settle").is_some());
    finish(&chat, path, cx);
}

#[gpui::test]
async fn archive_all_leaves_a_thread_that_left_settled_while_the_question_was_open(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "archive-all-race", true);
    let stays = add_idle_thread(&chat, cx, "comes back", false, settled_list(5));
    let goes = add_idle_thread(&chat, cx, "still put away", false, settled_list(6));
    let_motion_finish(&chat, cx);
    click(cx, "disclosure-settled");
    cx.run_until_parked();
    click(cx, "sheet-bulk");
    cx.run_until_parked();
    assert!(cx.debug_bounds("sheet-archive").is_some());

    cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            let ix = view.index_of(&stays, cx).unwrap();
            view.slots[ix].thread.update(cx, |thread, cx| {
                thread.unsettle();
                cx.notify();
            });
        })
    });
    cx.run_until_parked();
    click(cx, "sheet-archive");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let view = chat.read(cx);
        assert!(view.index_of(&stays, cx).is_some(), "it was not settled");
        assert!(view.index_of(&goes, cx).is_none());
    });
    finish(&chat, path, cx);
}

fn listed_ids(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| {
        let view = chat.read(cx);
        view.thread_order(cx)
            .into_iter()
            .map(|ix| view.slots[ix].thread.read(cx).id().to_string())
            .collect()
    })
}

fn read_list(landed_at: i64) -> newjen_core::store::ListState {
    newjen_core::store::ListState {
        landed_at,
        ..Default::default()
    }
}

#[gpui::test]
async fn the_thread_on_screen_keeps_its_exact_place_when_it_lands_and_moves_when_left(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "hold-order", false);
    let older = add_idle_thread(&chat, cx, "older", false, read_list(100));
    let mid = add_idle_thread(&chat, cx, "mid", false, read_list(200));
    let newest = add_idle_thread(&chat, cx, "newest", false, read_list(300));
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| view.show_thread(&mid, window, cx));
    });
    let_motion_finish(&chat, cx);
    let expected = vec![newest.clone(), mid.clone(), older.clone()];
    assert_eq!(listed_ids(&chat, cx), expected);

    cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            let ix = view.index_of(&mid, cx).unwrap();
            view.slots[ix].thread.update(cx, |thread, cx| {
                thread.snooze(i64::MAX);
                thread.wake(now_ms());
                cx.notify();
            });
        })
    });
    cx.run_until_parked();
    assert_eq!(
        listed_ids(&chat, cx),
        expected,
        "landing on screen does not move the row"
    );

    cx.update(|window, cx| {
        chat.update(cx, |view, cx| view.show_thread(&older, window, cx));
    });
    let_motion_finish(&chat, cx);
    assert_eq!(listed_ids(&chat, cx), vec![mid, newest, older]);
    finish(&chat, path, cx);
}

fn snoozed_list(until: i64) -> newjen_core::store::ListState {
    newjen_core::store::ListState {
        snoozed_until: Some(until),
        landed_at: until,
        ..Default::default()
    }
}

fn root_focused(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| chat.read(cx).root.is_focused(window))
}

fn filter_focused(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| {
        chat.read(cx)
            .sheet
            .as_ref()
            .is_some_and(|sheet| sheet.filter.read(cx).focus_handle(cx).is_focused(window))
    })
}

#[gpui::test]
async fn closing_a_panel_over_an_approval_gives_the_keyboard_to_the_dock(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "panel-focus", true);
    cx.update(|_, cx| cx.set_reduce_motion(false));
    add_idle_thread(&chat, cx, "later", false, snoozed_list(i64::MAX));
    cx.simulate_input("keep waiting");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let_motion_finish(&chat, cx);
    assert!(root_focused(&chat, cx));

    for close in ["escape", "outside"] {
        click(cx, "disclosure-snoozed");
        let_motion_finish(&chat, cx);
        assert!(filter_focused(&chat, cx), "the panel takes the keyboard");
        if close == "escape" {
            cx.simulate_keystrokes("escape");
        } else {
            let size = cx.update(|window, _| window.viewport_size());
            cx.simulate_click(
                gpui::point(size.width * 0.6, size.height * 0.3),
                gpui::Modifiers::default(),
            );
        }
        cx.run_until_parked();
        assert!(cx.update(|_, cx| chat.read(cx).sheet.is_none()));
        assert!(
            cx.update(|_, cx| chat.read(cx).sheet_exit.is_some()),
            "the panel is still lowering"
        );
        assert!(
            root_focused(&chat, cx),
            "closing by {close} leaves the keyboard on the dock"
        );
        let_motion_finish(&chat, cx);
    }
    assert!(
        cx.update(|_, cx| chat
            .read(cx)
            .thread()
            .read(cx)
            .transcript()
            .pending_approval()
            .is_some()),
        "nothing was denied"
    );
    finish(&chat, path, cx);
}

#[gpui::test]
async fn choosing_a_row_in_a_panel_gives_the_keyboard_to_its_thread(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "panel-row-focus", true);
    cx.simulate_input("keep waiting");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let waiting = cx.update(|_, cx| chat.read(cx).thread().read(cx).id().to_string());
    cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            view.slots[0].thread.update(cx, |thread, cx| {
                thread.snooze(i64::MAX);
                cx.notify();
            });
        })
    });
    let idle = add_idle_thread(&chat, cx, "quiet", false, snoozed_list(i64::MAX));
    cx.update(|window, cx| chat.update(cx, |view, cx| view.add_draft(window, cx)));
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.show(0, window, cx);
        })
    });
    let_motion_finish(&chat, cx);
    assert!(
        cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window)),
        "the draft has the keyboard"
    );

    for (shown, dock) in [(waiting, true), (idle, false)] {
        click(cx, "disclosure-snoozed");
        cx.run_until_parked();
        assert!(filter_focused(&chat, cx));
        cx.update(|window, cx| {
            chat.update(cx, |view, cx| view.show_thread(&shown, window, cx));
        });
        cx.run_until_parked();
        assert!(!filter_focused(&chat, cx));
        let composer =
            cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window));
        assert_eq!(root_focused(&chat, cx), dock);
        assert_eq!(composer, !dock);
        let_motion_finish(&chat, cx);
    }
    finish(&chat, path, cx);
}

fn panel_words(
    chat: &Entity<ChatView>,
    cx: &mut VisualTestContext,
) -> (Option<usize>, Option<sidebar::Confirm>) {
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let layout = view.sidebar_layout(cx);
            let sheet = view.sheet.take().unwrap();
            let panel = view.sheet_panel(&sheet, &layout, window, cx);
            view.sheet = Some(sheet);
            (panel.filtered, panel.confirming)
        })
    })
}

#[gpui::test]
async fn the_panel_names_the_filtered_count_and_the_copies_only_when_there_are_some(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "panel-words", true);
    let worktree = newjen_core::worktree::Worktree {
        path: path.join("copy"),
        workdir: path.join("copy"),
        source: path.clone(),
        branch: "newjen/test".into(),
        base: "0123456789abcdef0123456789abcdef01234567".into(),
    };
    add_idle_thread(&chat, cx, "alpha one", false, settled_list(5));
    add_idle_thread(&chat, cx, "alpha two", false, settled_list(6));
    add_idle_thread_in(&chat, cx, "beta", false, settled_list(7), Some(worktree));
    let_motion_finish(&chat, cx);
    click(cx, "disclosure-settled");
    cx.run_until_parked();
    assert_eq!(panel_words(&chat, cx), (None, None));

    cx.update(|window, cx| {
        let filter = chat.read(cx).sheet.as_ref().unwrap().filter.clone();
        filter.update(cx, |input, cx| input.set_value("alpha", window, cx));
    });
    assert_eq!(panel_words(&chat, cx).0, Some(2));
    click(cx, "sheet-bulk");
    cx.run_until_parked();
    assert_eq!(
        panel_words(&chat, cx).1,
        Some(sidebar::Confirm {
            count: 2,
            copies: false
        }),
        "no worktree goes with these two"
    );

    click(cx, "sheet-cancel");
    cx.update(|window, cx| {
        let filter = chat.read(cx).sheet.as_ref().unwrap().filter.clone();
        filter.update(cx, |input, cx| input.set_value("", window, cx));
    });
    assert_eq!(panel_words(&chat, cx).0, None);
    click(cx, "sheet-bulk");
    cx.run_until_parked();
    assert_eq!(
        panel_words(&chat, cx).1,
        Some(sidebar::Confirm {
            count: 3,
            copies: true
        })
    );
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_hidden_list_runs_no_motion_and_shows_where_it_is_when_it_returns(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "hidden-motion", false);
    add_idle_thread(&chat, cx, "before", true, Default::default());
    let_motion_finish(&chat, cx);
    let busy = |cx: &mut VisualTestContext| cx.update(|_, cx| chat.read(cx).frame.borrow().busy);
    assert!(!busy(cx));

    cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            view.sidebar_hidden = true;
            cx.notify();
        })
    });
    cx.run_until_parked();
    add_idle_thread(&chat, cx, "while hidden", true, Default::default());
    cx.run_until_parked();
    assert!(!busy(cx), "nothing animates in a list nobody sees");

    cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            view.sidebar_hidden = false;
            cx.notify();
        })
    });
    cx.run_until_parked();
    assert!(!busy(cx), "the list comes back as it is");
    finish(&chat, path, cx);
}

#[gpui::test]
async fn the_list_follows_the_interface_text_size(cx: &mut TestAppContext) {
    use newjen_core::appearance::{Appearance, TextSize};
    let (chat, path, cx) = setup(cx, "text-size", false);
    add_idle_thread(&chat, cx, "a row", true, Default::default());
    let_motion_finish(&chat, cx);
    let mut heights = Vec::new();
    for points in [14u8, 16, 12] {
        cx.update(|window, cx| {
            theme::set_appearance(
                Appearance {
                    interface_size: TextSize::new(points),
                    ..Appearance::default()
                },
                Some(window),
                cx,
            )
        });
        cx.update(|_, cx| chat.update(cx, |_, cx| cx.notify()));
        cx.run_until_parked();
        heights.push(cx.debug_bounds("thread-row").unwrap().size.height);
    }
    let grown = heights[1] - heights[0];
    let shrunk = heights[0] - heights[2];
    let step = px(sidebar::ROW_HEIGHT * (2. / sidebar::BASE_TEXT));
    assert!(
        (grown - step).abs() <= px(1.) && (shrunk - step).abs() <= px(1.),
        "a row grows and shrinks by {step:?} for two points, to the pixel: {heights:?}"
    );
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_settled_row_stays_on_screen_while_it_leaves(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "settle-motion", true);
    let id = add_idle_thread(&chat, cx, "first thread", true, Default::default());
    let_motion_finish(&chat, cx);
    let ghosts = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            chat.read(cx)
                .frame
                .borrow()
                .items
                .iter()
                .filter(|item| item.ghost)
                .count()
        })
    };
    assert_eq!(ghosts(cx), 0);
    cx.update(|_, cx| chat.update(cx, |view, cx| view.settle_thread(&id, cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).sidebar_layout(cx).settled.clone()),
        vec![1]
    );
    assert_eq!(ghosts(cx), 2, "the row and the Queue header are leaving");
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.update(|_, cx| chat.update(cx, |_, cx| cx.notify()));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("disclosure-settled").is_none(),
        "the dock counts it when the row has gone"
    );
    let_motion_finish(&chat, cx);
    assert_eq!(ghosts(cx), 0);
    assert!(cx.debug_bounds("disclosure-settled").is_some());
    finish(&chat, path, cx);
}

#[gpui::test]
async fn the_settled_panel_rises_and_lowers_instead_of_appearing_and_vanishing(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "panel-motion", true);
    add_idle_thread(&chat, cx, "park me", false, settled_list(5));
    cx.run_until_parked();
    let_motion_finish(&chat, cx);
    let lift = |cx: &mut VisualTestContext| cx.update(|_, cx| chat.read(cx).frame.borrow().raised);

    click(cx, "disclosure-settled");
    cx.run_until_parked();
    assert_eq!(lift(cx), Default::default());
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.update(|_, cx| chat.update(cx, |_, cx| cx.notify()));
    cx.run_until_parked();
    let rising = lift(cx);
    assert!(rising.panel > 0. && rising.panel < 1. && rising.dim > 0.);
    let_motion_finish(&chat, cx);
    let up = lift(cx);
    assert_eq!((up.panel, up.dim), (1., 1.));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("sheet").is_some(),
        "the panel stays while it lowers"
    );
    cx.executor().advance_clock(Duration::from_millis(80));
    cx.update(|_, cx| chat.update(cx, |_, cx| cx.notify()));
    cx.run_until_parked();
    let lowering = lift(cx);
    assert!(lowering.panel > 0. && lowering.panel < 1.);
    let_motion_finish(&chat, cx);
    assert!(cx.debug_bounds("sheet").is_none());
    assert_eq!(lift(cx), Default::default());
    finish(&chat, path, cx);
}

#[gpui::test]
async fn cmd_comma_opens_settings_where_a_click_changes_the_look_and_esc_leaves(
    cx: &mut TestAppContext,
) {
    use newjen_core::appearance::{Accent, Appearance, ChatWidth};

    let (chat, path, cx) = setup(cx, "settings", false);
    cx.simulate_input("keep this draft");
    assert!(cx.debug_bounds("settings-view").is_none());
    cx.simulate_keystrokes("cmd-,");
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-view").is_some());
    cx.update(|_, cx| assert_eq!(*theme::appearance(cx), Appearance::default()));

    click(cx, "accent-Blue");
    cx.run_until_parked();
    click(cx, "chat-width-Wide");
    cx.run_until_parked();
    let chosen = Appearance {
        accent: Accent::Blue,
        chat_width: ChatWidth::Wide,
        ..Appearance::default()
    };
    cx.update(|_, cx| assert_eq!(*theme::appearance(cx), chosen));
    let store = cx.update(|_, cx| chat.read(cx).store.clone());
    assert!(store.flush(Duration::from_secs(5)));
    assert_eq!(
        store.appearance().unwrap(),
        chosen,
        "kept for the next launch"
    );

    click(cx, "reset-appearance");
    cx.run_until_parked();
    cx.update(|_, cx| assert_eq!(*theme::appearance(cx), Appearance::default()));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-view").is_none());
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "keep this draft"
    );
    cx.simulate_keystrokes("cmd-,");
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-view").is_some());
    click(cx, "settings");
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-view").is_none());
    finish(&chat, path, cx);
}

#[gpui::test]
async fn esc_in_the_usage_view_returns_to_the_thread_and_its_composer(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "usage-esc", false);
    cx.simulate_input("keep this draft");
    click(cx, "usage-row");
    cx.run_until_parked();
    assert!(cx.debug_bounds("usage-view").is_some());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("usage-view").is_none());
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "keep this draft"
    );
    assert!(cx.update(|window, cx| chat.read(cx).field_focus(cx).unwrap().is_focused(window)));
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_project_shown_again_keeps_the_usage_view_and_the_threads_unread_mark(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "usage-reenter", true);
    cx.simulate_input("approval behind the usage view");
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.send(window, cx);
            view.show_usage(window, cx);
            view.set_visible(false, window, cx);
        })
    });
    pending(&chat, cx).await;
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_unread()));
    cx.update(|window, cx| chat.update(cx, |view, cx| view.set_visible(true, window, cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("usage-view").is_some());
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_unread()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("usage-view").is_none());
    assert!(!cx.update(|_, cx| chat.read(cx).thread().read(cx).is_unread()));
    finish(&chat, path, cx);
}

const IDLE_THREAD: &str = "6f1c1f4e-2b1e-4c8e-9a51-0d9a7c2f3b11";

/// Makes the thread on screen an idle project-folder thread with its starting checkpoint
/// recorded, and keeps the commit message from being written by a real `claude`.
fn idle_project_thread(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> PathBuf {
    cx.update(|_, cx| {
        chat.update(cx, |view, cx| {
            view.base_config.program = PathBuf::from("/nonexistent/claude");
            let dir = view.project.path.clone();
            let base = newjen_core::vcs::checkpoint(&view.tools, &dir, IDLE_THREAD).unwrap();
            let row = newjen_core::store::ThreadRow {
                id: IDLE_THREAD.into(),
                project_id: view.project.id,
                workdir: dir.clone(),
                title: "Port the parser".into(),
                provider: "claude".into(),
                conversation: newjen_core::agent::ConversationState::Established(
                    "6f1c1f4e-2b1e-4c8e-9a51-0d9a7c2f3b10".into(),
                ),
                model: newjen_core::providers::ModelSelection::CliDefault,
                effort: newjen_core::agent::EffortSelection::CliDefault,
                speed: newjen_core::agent::Speed::Normal,
                status: Status::Done,
                unread: false,
                created_at: 1,
                updated_at: 1,
                archived_at: None,
                copy: None,
                review_base: Some(base),
                landing_resolved: false,
                snapshot_needed: false,
                list: Default::default(),
            };
            view.store.save_thread(row.clone());
            assert!(view.store.flush(Duration::from_secs(5)));
            let thread = Thread::load(
                view.store.clone(),
                &view.project,
                row,
                view.base_config.clone(),
            )
            .unwrap();
            view.slots[0].thread = cx.new(|_| thread);
            dir
        })
    })
}

async fn open_changes(chat: &Entity<ChatView>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| chat.update(cx, |view, cx| view.open_review(window, cx)));
    cx.condition(chat, |view, _| view.review_panel.is_some())
        .await;
    settled(chat, cx).await;
}

async fn settled(chat: &Entity<ChatView>, cx: &mut VisualTestContext) {
    cx.condition(chat, |view, _| view.review_settled()).await;
    cx.run_until_parked();
}

async fn pick_scope(chat: &Entity<ChatView>, scope: Scope, cx: &mut VisualTestContext) {
    cx.update(|window, cx| chat.update(cx, |view, cx| view.select_scope(scope, window, cx)));
    settled(chat, cx).await;
}

fn review_files(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> (usize, Vec<String>) {
    let (_, rows, shown) = cx.update(|_, cx| chat.read(cx).review_shown(cx)).unwrap();
    (rows, shown)
}

#[gpui::test]
async fn the_git_button_commits_with_the_message_the_user_confirms(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "git-commit", false, Some(CREATE_COPY));
    let project = idle_project_thread(&chat, cx);
    let tools = cx.update(|_, cx| chat.read(cx).tools.clone());
    std::fs::write(project.join("notes.txt"), "hello\n").unwrap();
    open_changes(&chat, cx).await;
    assert_eq!(review_files(&chat, cx), (1, vec!["notes.txt".to_string()]));
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).review_primary()),
        Some(Ok("Commit".to_string())),
        "a repository with no remote can only commit"
    );

    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| window.has_active_dialog(cx)),
        "⌘↵ asks for the commit message first"
    );
    assert!(git(&tools, &project, &["log", "-1", "--format=%s"]) == "base");
    cx.condition(&chat, |view, _| {
        view.review_sheet().is_some_and(|(writing, _)| !writing)
    })
    .await;
    let field = cx.update(|_, cx| chat.read(cx).review_sheet().unwrap().1);
    assert_eq!(
        cx.update(|_, cx| field.read(cx).value().to_string()),
        "Port the parser",
        "without claude the thread's title is the message"
    );
    cx.update(|window, cx| {
        field.update(cx, |field, cx| {
            field.set_value("feat: add notes", window, cx)
        })
    });
    cx.simulate_keystrokes("cmd-enter");
    cx.condition(&chat, |view, _| {
        view.review_message_text().is_some() && view.review_settled()
    })
    .await;

    assert_eq!(
        git(&tools, &project, &["log", "-1", "--format=%s"]),
        "feat: add notes"
    );
    assert_eq!(git(&tools, &project, &["status", "--porcelain"]), "");
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).review_message_text()),
        Some(("Committed".to_string(), true))
    );
    assert_eq!(
        notice_title(&chat, cx).as_deref(),
        Some("Committed"),
        "the thread says so too"
    );
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).review_primary()),
        Some(Err("Up to date"))
    );
    assert_eq!(
        review_files(&chat, cx).0,
        1,
        "all changes still lists what was committed"
    );
    finish(&chat, path, cx);
}

#[gpui::test]
async fn the_scope_menu_picks_all_changes_uncommitted_work_or_one_turn(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "git-scope", false, Some(CREATE_COPY));
    let project = idle_project_thread(&chat, cx);
    let tools = cx.update(|_, cx| chat.read(cx).tools.clone());
    std::fs::write(project.join("one.txt"), "1\n").unwrap();
    newjen_core::vcs::checkpoint(&tools, &project, IDLE_THREAD).unwrap();
    std::fs::write(project.join("two.txt"), "2\n").unwrap();
    newjen_core::vcs::checkpoint(&tools, &project, IDLE_THREAD).unwrap();
    open_changes(&chat, cx).await;
    assert_eq!(cx.update(|_, cx| chat.read(cx).review_turns()), 2);
    assert_eq!(
        review_files(&chat, cx).0,
        2,
        "the default shows every change"
    );

    pick_scope(&chat, Scope::Turn(2), cx).await;
    assert_eq!(review_files(&chat, cx), (1, vec!["two.txt".to_string()]));
    pick_scope(&chat, Scope::Turn(1), cx).await;
    assert_eq!(review_files(&chat, cx), (1, vec!["one.txt".to_string()]));

    git(&tools, &project, &["add", "--all"]);
    git(&tools, &project, &["commit", "--quiet", "-m", "agent work"]);
    pick_scope(&chat, Scope::Uncommitted, cx).await;
    assert_eq!(review_files(&chat, cx).0, 0, "everything is committed");
    pick_scope(&chat, Scope::All, cx).await;
    assert_eq!(
        review_files(&chat, cx).0,
        2,
        "committed work stays in all changes"
    );
    finish(&chat, path, cx);
}

#[gpui::test]
async fn git_actions_wait_while_the_turn_runs(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "git-gated", true, Some(CREATE_COPY));
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let facts = git::GitFacts {
        status: newjen_core::vcs::Status {
            branch: Some("main".into()),
            dirty: true,
            ..Default::default()
        },
        gh: false,
        unpushed: false,
        source: None,
        turns: 0,
    };
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let id = view.thread().read(cx).id().to_string();
            let mut loaded = loaded("");
            loaded.facts = Some(facts);
            view.show_review(id, Scope::All, loaded, window, cx);
            view.run_primary_action(window, cx);
        })
    });
    cx.run_until_parked();
    assert!(
        !cx.update(|window, cx| window.has_active_dialog(cx)),
        "no commit sheet while the agent may still be writing files"
    );
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).review_message_text()),
        Some(("Wait for the turn to finish".to_string(), false))
    );
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_worktree_thread_commits_then_merges_into_the_project_branch(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "git-merge", true, Some(CREATE_COPY));
    choose(1, cx);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    cx.condition(&chat, |view, cx| view.thread().read(cx).copy().is_some())
        .await;
    pending(&chat, cx).await;
    let copy = cx
        .update(|_, cx| chat.read(cx).thread().read(cx).copy().cloned())
        .unwrap();
    for _ in 0..6 {
        if cx.update(|_, cx| !working(chat.read(cx).thread().read(cx))) {
            break;
        }
        cx.update(|window, cx| chat.update(cx, |view, cx| view.stop(window, cx)));
        cx.background_executor
            .timer(Duration::from_millis(100))
            .await;
        cx.run_until_parked();
    }
    cx.condition(&chat, |view, cx| {
        let thread = view.thread().read(cx);
        !is_busy(thread.status()) && !thread.work_pending()
    })
    .await;
    let tools = cx.update(|_, cx| chat.read(cx).tools.clone());
    cx.update(|_, cx| {
        chat.update(cx, |view, _| {
            view.base_config.program = PathBuf::from("/nonexistent/claude")
        })
    });
    std::fs::write(copy.workdir.join("feature.txt"), "feature\n").unwrap();
    open_changes(&chat, cx).await;
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).review_primary()),
        Some(Ok("Commit".to_string()))
    );

    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.start_git_action(git::GitAction::CommitMerge, window, cx)
        })
    });
    cx.condition(&chat, |view, _| {
        view.review_sheet().is_some_and(|(writing, _)| !writing)
    })
    .await;
    click(cx, "commit-confirm");
    cx.condition(&chat, |view, _| {
        view.review_message_text().is_some() && view.review_settled()
    })
    .await;

    let project = path.join("project").canonicalize().unwrap();
    assert!(
        project.join("feature.txt").exists(),
        "the merge brings the thread's work into the project's branch"
    );
    assert_eq!(git(&tools, &project, &["status", "--porcelain"]), "");
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).review_primary()),
        Some(Err("Up to date"))
    );
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).review_message_text().map(|m| m.1)),
        Some(true)
    );
    finish(&chat, path, cx);
}

/// The diff takes the pane's height: a percentage height under a row that centres its
/// children collapses to the content, and the diff shows nothing.
#[gpui::test]
fn the_review_diff_fills_the_pane(cx: &mut TestAppContext) {
    let patch =
        "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n keep\n+added\n";
    let (chat, path, cx) = setup(cx, "review-fill", false);
    cx.simulate_resize(size(px(1100.), px(800.)));
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let id = view.thread().read(cx).id().to_string();
            view.show_review(id, Scope::All, loaded(patch), window, cx);
        })
    });
    cx.run_until_parked();
    let pane = cx.debug_bounds("review-diff").unwrap();
    assert!(
        pane.size.height > px(500.),
        "the diff pane is {:?} tall in an 800 px window",
        pane.size.height
    );
    finish(&chat, path, cx);
}

#[gpui::test]
async fn merging_a_worktree_waits_while_a_thread_works_in_the_project_folder(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup_in(cx, "git-merge-gate", true, Some(CREATE_COPY));
    choose(1, cx);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    cx.condition(&chat, |view, cx| view.thread().read(cx).copy().is_some())
        .await;
    pending(&chat, cx).await;
    for _ in 0..6 {
        if cx.update(|_, cx| !working(chat.read(cx).thread().read(cx))) {
            break;
        }
        cx.update(|window, cx| chat.update(cx, |view, cx| view.stop(window, cx)));
        cx.background_executor
            .timer(Duration::from_millis(100))
            .await;
        cx.run_until_parked();
    }
    cx.condition(&chat, |view, cx| {
        let thread = view.thread().read(cx);
        !is_busy(thread.status()) && !thread.work_pending()
    })
    .await;
    let wt_id = cx.update(|_, cx| chat.read(cx).thread().read(cx).id().to_string());
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).git_gate(&wt_id, cx)),
        None,
        "the idle worktree thread may use the git button on its own"
    );

    let base = path.canonicalize().unwrap();
    let peer = peer_project("project/packages/web", true, vec![&chat], &base, cx);
    cx.update(|_, cx| chat.update(cx, |view, _| view.set_copy_peers(vec![peer.downgrade()])));
    cx.update(|window, cx| {
        peer.update(cx, |view, cx| {
            assert!(view.send_text("work in the project folder", window, cx));
        })
    });
    pending(&peer, cx).await;
    assert!(cx.update(|_, cx| peer.read(cx).thread().read(cx).status().is_busy()));

    let gate = cx.update(|_, cx| chat.read(cx).git_gate(&wt_id, cx));
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&chat, path, cx);
    assert_eq!(
        gate,
        Some("Another thread is working in this folder"),
        "the worktree thread's Merge into main would run `git merge` in the project folder while its agent is mid-turn"
    );
}
