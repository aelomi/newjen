use super::*;
use gpui::{AppContext, TestAppContext, VisualTestContext, size};
use gpui_kit::base::Root;

fn setup<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    with_approvals: bool,
) -> (Entity<ChatView>, PathBuf, &'a mut VisualTestContext) {
    setup_in(cx, name, with_approvals, None)
}

/// `rift` is a script that runs `create` as `rift_create` says (see `fake_rift`), and the
/// project is `<path>/project`, so its copies go to `<path>/.rifts/project/`.
fn setup_in<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    with_approvals: bool,
    rift_create: Option<&str>,
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
    let project_dir = match rift_create {
        Some(create) => {
            // The copy job runs rift on threads of its own.
            cx.executor().allow_parking();
            tools.rift = fake_rift(&path, create);
            let project = path.join("project");
            std::fs::create_dir_all(&project).unwrap();
            project.canonicalize().unwrap()
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

/// What the fake `rift create` does: make the copy after printing two lines.
const CREATE_COPY: &str = r#"echo "copying files" >&2
    echo "postcreate: npm install" >&2
    mkdir -p "$into/$name" && printf 'copy01\n' > "$into/$name/.rift""#;
/// A `rift create` whose postcreate hook fails.
const FAIL_HOOK: &str = r#"echo "npm ERR! code ERESOLVE" >&2
    echo "postcreate hook failed (exit 1)" >&2
    exit 1"#;

/// A stand-in for `rift`: `init` marks the root, `create` runs `create`, and `remove` moves the
/// copy into the `.trash` folder beside it, as Rift does.
fn fake_rift(path: &Path, create: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let program = path.join("rift-test");
    std::fs::write(
        &program,
        format!(
            r#"#!/bin/sh
case "$1" in
  init) printf 'root01
' > "$2/.rift" ;;
  create)
    while [ $# -gt 1 ]; do
      case "$1" in --name) name=$2; shift ;; --into) into=$2; shift ;; esac
      shift
    done
    {create} ;;
  remove)
    trash="$(dirname "$2")/.trash"
    mkdir -p "$trash" && mv "$2" "$trash/$(cat "$2/.rift")-$(basename "$2")" ;;
esac
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
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
    click(cx, "thread-row");
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

/// Opens the composer's "Project folder" menu and picks its `nth` item with the keys.
fn choose(nth: usize, cx: &mut VisualTestContext) {
    click(cx, "works-in");
    assert!(cx.update(|window, _| overlay_focused(window)));
    for _ in 0..=nth {
        cx.simulate_keystrokes("down");
    }
    cx.simulate_keystrokes("enter");
}

#[gpui::test]
fn the_picker_chooses_a_rift_copy_and_hands_the_keys_back(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "picker", false, Some(CREATE_COPY));
    cx.simulate_input("half typed");
    click(cx, "works-in");
    assert!(cx.update(|window, _| overlay_focused(window)));
    cx.simulate_keystrokes("escape");
    assert!(!cx.update(|window, _| overlay_focused(window)));
    assert!(composer_focused(&chat, cx));
    assert_eq!(works_in(&chat, cx), WorksIn::ProjectFolder);

    choose(1, cx);
    assert!(!cx.update(|window, _| overlay_focused(window)));
    assert_eq!(works_in(&chat, cx), WorksIn::RiftCopy);
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
async fn a_rift_draft_sends_its_first_message_from_its_new_copy(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "rift-send", true, Some(CREATE_COPY));
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
    let copy = copy.expect("the thread works in its copy");
    let rifts = path.canonicalize().unwrap().join(".rifts/project");
    assert!(copy.path.starts_with(rifts), "{copy:?}");
    assert_eq!(cwd, copy.workdir);
    assert!(!draft);
    assert!(matches!(first, Some(Item::User { text, .. }) if text == "two approvals"));
    assert_eq!(cx.update(|_, cx| chat.read(cx).composer_text(0, cx)), "");
    assert!(cx.debug_bounds("rift-copy-tag").is_some());
    assert!(cx.debug_bounds("works-in").is_none());
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_failed_copy_puts_the_message_back_and_try_again_sends_it(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "rift-fail", true, Some(FAIL_HOOK));
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
    assert_eq!(output, ["npm ERR! code ERESOLVE"]);
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(0, cx)),
        "add a benchmark"
    );
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    assert_eq!(works_in(&chat, cx), WorksIn::RiftCopy);
    assert!(composer_focused(&chat, cx));

    cx.update(|_, cx| {
        chat.update(cx, |view, _| {
            view.tools.rift = fake_rift(&path, CREATE_COPY);
        })
    });
    cx.run_until_parked();
    click(cx, "retry");
    cx.condition(&chat, |view, cx| view.thread().read(cx).copy().is_some())
        .await;
    assert_eq!(notice_title(&chat, cx), None);
    assert_eq!(cx.update(|_, cx| chat.read(cx).composer_text(0, cx)), "");
    finish(&chat, path, cx);
}

#[gpui::test]
async fn a_copy_waits_while_another_thread_works_in_the_project_folder(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "rift-refused", true, Some(CREATE_COPY));
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    cx.simulate_keystrokes("cmd-n");
    choose(1, cx);
    cx.simulate_input("rename the keys");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        notice_title(&chat, cx).as_deref(),
        Some("Another thread is working in the project folder")
    );
    let (preparing, text) = cx.update(|_, cx| {
        let view = chat.read(cx);
        (
            view.thread().read(cx).preparing().is_some(),
            view.composer_text(view.active, cx),
        )
    });
    assert!(!preparing);
    assert_eq!(text, "rename the keys");
    assert!(!path.join(".rifts").exists());
    finish(&chat, path, cx);
}

#[gpui::test]
fn choosing_a_rift_copy_without_rift_says_how_to_install_it(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "rift-missing", false);
    cx.update(|_, cx| chat.update(cx, |view, _| view.tools.rift = PathBuf::from("rift")));
    choose(1, cx);
    assert_eq!(works_in(&chat, cx), WorksIn::RiftCopy);
    assert_eq!(
        notice_title(&chat, cx).as_deref(),
        Some("Rift isn't installed")
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
            thread.set_works_in(WorksIn::RiftCopy).unwrap();
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
    assert!(cx.debug_bounds("rift-copy-tag").is_some());
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
fn a_failed_copy_names_what_failed(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "failed-copy", false);
    let thread = cx.update(|_, cx| chat.read(cx).thread().clone());
    let job = cx.update(|_, cx| {
        thread.update(cx, |thread, cx| {
            thread.set_works_in(WorksIn::RiftCopy).unwrap();
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
        command: "rift create".to_string(),
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

/// Right click on the thread on screen's row, then "Archive thread".
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
    cx.simulate_keystrokes("down enter");
}

#[gpui::test]
async fn archiving_a_rift_thread_asks_then_deletes_its_copy(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "rift-archive", true, Some(CREATE_COPY));
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
fn project_send_must_wait_for_preparing_copy(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "review-reverse-exclusion", true);
    let copying = cx.update(|_, cx| chat.read(cx).thread().clone());
    let job = cx.update(|_, cx| {
        copying.update(cx, |thread, cx| {
            thread.set_works_in(WorksIn::RiftCopy).unwrap();
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
    let preparing = cx.update(|_, cx| copying.read(cx).preparing().is_some());
    let busy = cx.update(|_, cx| chat.read(cx).thread().read(cx).status().is_busy());
    println!(
        "copy preparing: {preparing}; project send accepted: {accepted}; project agent busy: {busy}"
    );
    assert!(
        !accepted,
        "project send must wait until the snapshot is done"
    );
    assert!(!busy);
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).composer_text(chat.read(cx).active, cx)),
        "edit project"
    );
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).slots[chat.read(cx).active]
            .notice
            .as_ref()
            .unwrap()
            .detail
            .clone()),
        "Newjen is taking a jj snapshot of the project to make the Rift copy for “copy work”."
    );
    cx.update(|_, cx| {
        copying.update(cx, |thread, _| {
            thread.note_copy_progress(rift::Progress::Line("still preparing".to_string()))
        })
    });
    assert!(cx.update(|_, cx| copying.read(cx).blocks_project_send()));
    cx.update(|_, cx| {
        copying.update(cx, |thread, _| {
            thread.note_copy_progress(rift::Progress::MainRepoReady)
        })
    });
    assert!(!cx.update(|_, cx| copying.read(cx).blocks_project_send()));
    assert!(
        cx.update(|window, cx| chat.update(cx, |view, cx| view.send_text(
            "edit project",
            window,
            cx
        )))
    );
    drop(job);
    finish(&chat, path, cx);
}

#[gpui::test]
async fn startup_restores_a_pending_copy_message_to_a_rift_draft(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup_in(cx, "recover-copy", true, Some(CREATE_COPY));
    let (job, tools) = cx.update(|_, cx| {
        let view = chat.read(cx);
        let tools = view.tools.clone();
        let thread = view.thread().clone();
        let job = thread.update(cx, |thread, _| {
            thread.set_works_in(WorksIn::RiftCopy).unwrap();
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
        assert_eq!(view.thread().read(cx).works_in(), WorksIn::RiftCopy);
        assert!(view.thread().read(cx).is_draft());
        assert!(!view.thread().read(cx).has_session());
        assert!(view.thread().read(cx).transcript().items().is_empty());
        assert_eq!(
            view.slots[0].notice.as_ref().unwrap().title,
            "Newjen quit while making a Rift copy. Your message is back."
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
            thread.set_works_in(WorksIn::RiftCopy).unwrap();
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
fn the_snapshot_notice_clears_when_the_guard_releases(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "snapshot-release", true);
    let copying = cx.update(|_, cx| chat.read(cx).thread().clone());
    let job = cx.update(|_, cx| {
        copying.update(cx, |thread, cx| {
            thread.set_works_in(WorksIn::RiftCopy).unwrap();
            let Sent::MakingCopy(job) = thread.send("copy work").unwrap() else {
                panic!()
            };
            cx.notify();
            job
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_input("edit project");
    cx.simulate_keystrokes("enter");
    assert_eq!(notice_title(&chat, cx).as_deref(), Some("Wait a moment"));
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-notice").is_some());
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).slots[chat.read(cx).active]
            .messages
            .read(cx)
            .item_count()),
        1
    );
    cx.update(|_, cx| {
        copying.update(cx, |thread, cx| {
            thread.note_copy_progress(rift::Progress::MainRepoReady);
            cx.notify();
        })
    });
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| copying.read(cx).blocks_project_send()));
    assert_eq!(notice_title(&chat, cx), None);
    assert!(
        cx.debug_bounds("chat-notice").is_none(),
        "the cleared notice is still drawn"
    );
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).slots[chat.read(cx).active]
            .messages
            .read(cx)
            .item_count()),
        0
    );
    drop(job);
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
            thread.set_works_in(WorksIn::RiftCopy).unwrap();
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
fn agent_picker_keys_switch_tabs_choose_effort_and_return_focus(cx: &mut TestAppContext) {
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
    cx.simulate_keystrokes("right");
    click(cx, "agent-row-1");
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
    click(cx, "effort-picker");
    cx.simulate_keystrokes("down down down down enter");
    assert_eq!(
        cx.update(|_, cx| chat
            .read(cx)
            .thread()
            .read(cx)
            .agent_config()
            .unwrap()
            .speed),
        newjen_core::Speed::Fast
    );
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
async fn a_busy_project_in_the_same_repository_blocks_a_peers_copy(cx: &mut TestAppContext) {
    let (busy, path, cx) = setup(cx, "copy-peer", true);
    std::fs::create_dir_all(path.join(".git")).unwrap();
    let sibling = path.join("subproject");
    std::fs::create_dir_all(&sibling).unwrap();
    let peer = cx.update(|window, cx| {
        let store = busy.read(cx).store.clone();
        let project = store.open_project(&sibling).unwrap();
        let startup = Startup {
            store,
            project,
            base_config: SessionConfig::new(&sibling),
            threads: vec![],
            problem: None,
            home: None,
            tools: Tools {
                rift: PathBuf::from("/bin/rift"),
                jj: PathBuf::from("/bin/jj"),
                env: vec![],
            },
        };
        let peer = cx.new(|cx| ChatView::new(startup, window, cx));
        peer.update(cx, |view, cx| {
            view.set_copy_peers(vec![busy.downgrade()]);
            view.thread().clone().update(cx, |thread, _| {
                thread.set_works_in(WorksIn::RiftCopy).unwrap()
            });
            view.set_visible(false, window, cx);
        });
        busy.update(cx, |view, cx| view.set_visible(true, window, cx));
        peer
    });
    cx.simulate_input("work in the repository");
    cx.simulate_keystrokes("enter");
    pending(&busy, cx).await;
    assert!(cx.update(|_, cx| {
        peer.read(cx)
            .copy_hold(peer.read(cx).thread(), cx)
            .is_some()
    }));
    std::fs::remove_dir_all(path.join(".git")).unwrap();
    assert!(cx.update(|_, cx| {
        peer.read(cx)
            .copy_hold(peer.read(cx).thread(), cx)
            .is_some()
    }));
    let unrelated_path = path.with_extension("unrelated");
    std::fs::create_dir_all(&unrelated_path).unwrap();
    let unrelated = cx.update(|window, cx| {
        let store = busy.read(cx).store.clone();
        let project = store.open_project(&unrelated_path).unwrap();
        let tools = peer.read(cx).tools.clone();
        let unrelated = cx.new(|cx| {
            ChatView::new(
                Startup {
                    store,
                    project,
                    base_config: SessionConfig::new(&unrelated_path),
                    threads: vec![],
                    problem: None,
                    home: None,
                    tools,
                },
                window,
                cx,
            )
        });
        unrelated.update(cx, |view, cx| {
            view.set_copy_peers(vec![busy.downgrade()]);
            view.thread().clone().update(cx, |thread, _| {
                thread.set_works_in(WorksIn::RiftCopy).unwrap();
            });
        });
        unrelated
    });
    assert!(cx.update(|_, cx| {
        unrelated
            .read(cx)
            .copy_hold(unrelated.read(cx).thread(), cx)
            .is_none()
    }));
    cx.update(|_, cx| unrelated.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&busy, path, cx);
    std::fs::remove_dir_all(unrelated_path).unwrap();
}

#[gpui::test]
fn a_project_folder_send_waits_for_an_overlapping_copy_job(cx: &mut TestAppContext) {
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
            thread.set_works_in(WorksIn::RiftCopy).unwrap();
            match thread.send("create isolated copy").unwrap() {
                Sent::MakingCopy(job) => job,
                _ => panic!("expected copy preparation"),
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
    assert!(!allowed);
    drop(job);
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&copying, path, cx);
    assert!(
        !started,
        "source writer must wait for overlapping copy preparation"
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

#[gpui::test]
async fn a_busy_rift_folder_cannot_be_copied_as_another_project(cx: &mut TestAppContext) {
    let (busy, path, cx) = setup_in(cx, "review-rift-peer", true, Some(CREATE_COPY));
    choose(1, cx);
    cx.simulate_input("two approvals");
    cx.simulate_keystrokes("enter");
    pending(&busy, cx).await;
    let peer = cx.update(|window, cx| {
        let view = busy.read(cx);
        let thread = view.thread().read(cx);
        assert!(thread.status().is_busy());
        let cwd = thread.cwd().to_path_buf();
        let store = view.store.clone();
        let tools = view.tools.clone();
        let project = store.open_project(&cwd).unwrap();
        let startup = Startup {
            store,
            project,
            base_config: SessionConfig::new(&cwd),
            threads: vec![],
            problem: None,
            home: None,
            tools,
        };
        let peer = cx.new(|cx| ChatView::new(startup, window, cx));
        peer.update(cx, |view, cx| {
            view.set_copy_peers(vec![busy.downgrade()]);
            view.thread().clone().update(cx, |thread, _| {
                thread.set_works_in(WorksIn::RiftCopy).unwrap();
            });
        });
        peer
    });
    let blocked = cx.update(|_, cx| {
        let view = peer.read(cx);
        let running = busy.read(cx).thread().read(cx);
        assert_eq!(rift::project_root(&view.project.path).0, running.cwd());
        view.copy_hold(view.thread(), cx).is_some()
    });
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&busy, path, cx);
    assert!(
        blocked,
        "must block copying a folder with a busy peer working in it"
    );
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

fn request_key(chat: &Entity<ChatView>, cx: &mut VisualTestContext) -> newjen_core::ApprovalKey {
    cx.update(|_, cx| {
        chat.read(cx)
            .thread()
            .read(cx)
            .transcript()
            .pending_approval()
            .unwrap()
            .key()
    })
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
        tools: Tools {
            rift: PathBuf::from("/bin/rift"),
            jj: PathBuf::from("/bin/jj"),
            env: vec![],
        },
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
async fn inbox_answers_the_captured_thread_in_any_project_without_selecting_it(
    cx: &mut TestAppContext,
) {
    let (chat, path, cx) = setup(cx, "inbox-routing", true);
    cx.simulate_input("first thread");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    let first = cx.update(|_, cx| chat.read(cx).thread().read(cx).id().to_string());
    let first_request = request_id(&chat, cx);
    let first_key = request_key(&chat, cx);
    // The second waiting thread is in another loaded project, out of sight.
    let peer = peer_project("other", true, vec![&chat], &path, cx);
    cx.update(|window, cx| {
        peer.update(cx, |view, cx| {
            assert!(view.send_text("second thread", window, cx));
        })
    });
    pending(&peer, cx).await;
    let second = cx.update(|_, cx| peer.read(cx).thread().read(cx).id().to_string());
    let second_key = request_key(&peer, cx);
    assert_ne!(second_key, first_key);
    let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let seen = opened.clone();
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.set_copy_peers(vec![peer.downgrade()]);
            view.set_thread_opener(std::rc::Rc::new(move |id, _, _| {
                seen.borrow_mut().push(id.to_string())
            }));
            assert_eq!(view.waiting_threads(cx), 1);
            assert_eq!(view.waiting_everywhere(cx), 2);
            assert_eq!(view.quit_blockers(cx).waiting, 1);
            view.open_inbox(window, cx);
            // Another request's key never answers this one.
            view.answer_thread(
                &first,
                Some(second_key),
                ApprovalIntent::AllowOnce,
                window,
                cx,
            );
        })
    });
    assert_eq!(answers(&path).len(), 0);
    cx.run_until_parked();
    let selector = |thread: &str, button: &str| -> &'static str {
        Box::leak(format!("inbox-{thread}-{button}").into_boxed_str())
    };
    assert!(
        cx.debug_bounds(selector(&first, "always")).is_none(),
        "the inbox offers no session grant"
    );
    click(cx, selector(&second, "once"));
    cx.update(|_, cx| {
        let view = chat.read(cx);
        assert_eq!(view.thread().read(cx).id(), first);
        assert_eq!(view.panel, Panel::Inbox);
    });
    wait_answers(cx, &path, 1).await;
    assert_eq!(
        answers(&path)[0]["response"]["response"]["behavior"],
        "allow"
    );
    assert_eq!(request_id(&chat, cx), first_request);
    assert_eq!(request_id(&peer, cx), "second-tool");
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(
        answers(&path).len(),
        1,
        "inbox keys must not answer the hidden dock"
    );
    click(cx, selector(&first, "deny"));
    wait_answers(cx, &path, 2).await;
    assert_eq!(
        answers(&path)[1]["response"]["response"]["behavior"],
        "deny"
    );
    cx.run_until_parked();
    assert_eq!(request_id(&chat, cx), "second-tool");
    // "Open thread" on another project's row goes through the window, which switches
    // projects; this project's own rows open here.
    click(cx, selector(&second, "open"));
    assert_eq!(opened.borrow().as_slice(), std::slice::from_ref(&second));
    assert!(cx.update(|_, cx| chat.read(cx).panel == Panel::Inbox));
    click(cx, selector(&first, "open"));
    assert_eq!(opened.borrow().len(), 1);
    assert!(cx.update(|_, cx| chat.read(cx).panel == Panel::Thread));
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
    cx.simulate_keystrokes("cmd-shift-a");
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(!window.has_active_sheet(cx));
        assert_eq!(chat.read(cx).panel, Panel::Inbox);
        assert!(chat.read(cx).root.is_focused(window));
    });
    cx.simulate_keystrokes("cmd-b");
    cx.simulate_keystrokes("cmd-`");
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(!window.has_active_sheet(cx));
        let view = chat.read(cx);
        assert_eq!(view.panel, Panel::Terminal);
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
fn archiving_the_selected_terminal_returns_to_a_visible_conversation(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "archive-terminal-panel", false);
    cx.executor().allow_parking();
    cx.simulate_keystrokes("cmd-`");
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let id = view.thread().read(cx).id().to_string();
            view.archive(&id, window, cx);
            assert_eq!(view.panel, Panel::Thread);
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
    cx.update(|_, cx| chat.update(cx, |view, _| view.tools.rift = PathBuf::from("/bin/rift")));
    cx.executor().allow_parking();
    cx.simulate_keystrokes("cmd-`");
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            view.toggle_terminal(window, cx);
            view.choose_works_in(WorksIn::RiftCopy, window, cx);
            assert_eq!(view.thread().read(cx).works_in(), WorksIn::ProjectFolder);
        })
    });
    cx.simulate_keystrokes("cmd-n");
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            assert_eq!(view.slots.len(), 2);
            assert!(view.slots[1].terminal.is_some());
            // The project-folder shell could change files under a copy's snapshot.
            view.choose_works_in(WorksIn::RiftCopy, window, cx);
            let hold = view.copy_hold(&view.thread().clone(), cx).unwrap();
            assert_eq!(hold.title, "Close the project terminal first");
        })
    });
    // A peer project in the same repository sees that terminal too.
    let peer = peer_project("sibling", false, vec![&chat], &path, cx);
    std::fs::create_dir_all(path.join(".git")).unwrap();
    cx.update(|_, cx| {
        peer.update(cx, |view, cx| {
            view.thread().clone().update(cx, |thread, _| {
                thread.set_works_in(WorksIn::RiftCopy).unwrap()
            });
            let hold = view.copy_hold(&view.thread().clone(), cx).unwrap();
            assert_eq!(hold.title, "Close the project terminal first");
        })
    });
    cx.update(|_, cx| peer.update(cx, |view, cx| view.quit(cx)))
        .unwrap();
    finish(&chat, path, cx);
}

/// The repository root and one of its subfolders are open as two projects. While a thread
/// works in the root's project folder, a project-folder send in the subfolder is refused,
/// as it would be in the same project.
#[gpui::test]
async fn a_second_project_folder_thread_in_the_same_repository_is_refused(cx: &mut TestAppContext) {
    let (chat, path, cx) = setup(cx, "two-projects-one-repo", true);
    std::fs::create_dir_all(path.join(".git")).unwrap();
    cx.simulate_input("thread A in the root");
    cx.simulate_keystrokes("enter");
    pending(&chat, cx).await;
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).owns_project_work()));
    let peer = peer_project("packages/web", true, vec![&chat], &path, cx);
    cx.update(|_, cx| chat.update(cx, |view, _| view.set_copy_peers(vec![peer.downgrade()])));
    let went_out = cx.update(|window, cx| {
        peer.update(cx, |view, cx| {
            view.send_text("thread B in the subfolder", window, cx)
        })
    });
    assert!(
        !went_out,
        "the subfolder must wait for the root's project thread"
    );
    assert_eq!(
        notice_title(&peer, cx).as_deref(),
        Some("Review the project thread first")
    );
    assert!(cx.update(|_, cx| {
        let thread = peer.read(cx).thread().read(cx);
        !thread.has_session() && thread.preparing().is_none()
    }));
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
    use newjen_core::landing::{LandingMode, Review};
    let (chat, path, cx) = setup(cx, "review-keys", false);
    cx.simulate_input("first thread");
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_input("second thread");
    assert_eq!(cx.update(|_, cx| chat.read(cx).active), 0);
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let id = view.thread().read(cx).id().to_string();
            let receipt = Review {
                base: "base".into(),
                head: "head".into(),
                patch: String::new(),
                truncated: false,
            };
            view.show_review(id, receipt, LandingMode::HarnessManaged, window, cx);
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
    use newjen_core::landing::{LandingMode, Review};
    let (chat, path, cx) = setup(cx, "review-close-focus", false);
    cx.simulate_input("a draft");
    cx.update(|window, cx| {
        chat.update(cx, |view, cx| {
            let id = view.thread().read(cx).id().to_string();
            let receipt = Review {
                base: "base".into(),
                head: "head".into(),
                patch: String::new(),
                truncated: false,
            };
            view.show_review(id, receipt, LandingMode::HarnessManaged, window, cx);
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
                landing_mode: newjen_core::landing::LandingMode::HarnessManaged,
                review_base: Some("a".repeat(40)),
                landing_resolved: false,
                snapshot_needed: false,
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
