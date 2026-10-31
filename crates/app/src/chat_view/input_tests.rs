use gpui_kit::component::WindowExt as _;

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
    cx.update(|_, cx| chat.update(cx, |view, cx| view.quit(cx)));
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
    cx.update(|_, cx| chat.update(cx, |view, cx| view.quit(cx)));
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

fn script(path: &Path) -> PathBuf {
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

fn answers(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path.join("answers.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|value: &serde_json::Value| value["type"] == "control_response")
        .collect()
}

async fn wait_answers(cx: &VisualTestContext, path: &Path, count: usize) {
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

async fn pending(chat: &Entity<ChatView>, cx: &mut VisualTestContext) {
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
            .request_id
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
    cx.update(|_, cx| chat.update(cx, |view, cx| view.quit(cx)));
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
    assert_eq!(request_id(&chat, cx), "second-request");
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
    assert_eq!(request_id(&chat, cx), "second-request");
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
    assert_eq!(request_id(&chat, cx), "second-request");
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
    assert_eq!(request_id(&chat, cx), "second-request");
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
    assert_eq!(request_id(&chat, cx), "second-request");
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).thread().read(cx).grants().len()),
        1
    );
    cx.executor().advance_clock(KEY_ANSWER_DELAY);
    click(cx, "grants-chip");
    assert!(cx.update(|window, _| overlay_focused(window)));
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(request_id(&chat, cx), "second-request");
    cx.simulate_keystrokes("escape");
    assert!(!cx.update(|window, _| overlay_focused(window)));
    assert_eq!(request_id(&chat, cx), "second-request");
    assert_eq!(answers(&path).len(), 1);
    assert!(cx.update(|window, cx| chat.read(cx).root.is_focused(window)));
    cx.simulate_keystrokes("cmd-n");
    assert!(cx.update(|_, cx| chat.read(cx).thread().read(cx).is_draft()));
    cx.simulate_keystrokes("cmd-2");
    assert_eq!(request_id(&chat, cx), "second-request");
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
    let (chat, path, cx) = setup_in(cx, "rift-fail", false, Some(FAIL_HOOK));
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
    assert!(cx.debug_bounds("works-in").is_some());
    finish(&chat, path, cx);
}
