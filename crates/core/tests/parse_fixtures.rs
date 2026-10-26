//! Every line the real CLI printed must decode into a known message type.

mod common;

use newjen_core::protocol::{ContentBlock, Incoming, StreamEvent};

#[test]
fn every_out_line_of_every_fixture_parses_into_a_known_variant() {
    let names = common::all_fixture_names();
    assert!(names.len() >= 12, "expected the fixtures to be present");
    let mut checked = 0;
    for name in &names {
        for (number, line) in common::load(name).iter().enumerate() {
            if !line.out {
                continue;
            }
            let parsed = Incoming::from_value(line.msg.clone());
            assert!(
                !matches!(parsed, Incoming::Unknown(_)),
                "{name}.jsonl line {}: type {} parsed as Unknown",
                number + 1,
                line.msg["type"]
            );
            checked += 1;
        }
    }
    assert!(checked > 100);
}

#[test]
fn known_content_blocks_and_stream_events_are_recognised() {
    for name in common::all_fixture_names() {
        for line in common::load(&name).iter().filter(|line| line.out) {
            match Incoming::from_value(line.msg.clone()) {
                Incoming::Assistant(message) => {
                    for block in &message.message.content {
                        assert!(
                            !matches!(block, ContentBlock::Unknown(_)),
                            "{name}: unknown assistant block {block:?}"
                        );
                    }
                }
                Incoming::User(message) => {
                    for block in message.message.content.blocks() {
                        assert!(
                            !matches!(block, ContentBlock::Unknown(_)),
                            "{name}: unknown user block {block:?}"
                        );
                    }
                }
                Incoming::StreamEvent(message) => {
                    assert!(
                        !matches!(message.event, StreamEvent::Unknown(_)),
                        "{name}: unknown stream event {:?}",
                        message.event
                    );
                }
                _ => {}
            }
        }
    }
}

#[test]
fn permission_request_fields_are_decoded() {
    let line = common::load("tool_allow")
        .into_iter()
        .find(|line| line.out && line.msg["type"] == "control_request")
        .expect("tool_allow has a control_request");
    let Incoming::PermissionRequest(request) = Incoming::from_value(line.msg) else {
        panic!("expected a permission request");
    };
    assert_eq!(request.request_id, "1b0e90a0-57cc-4681-a14d-639b625b2dde");
    assert_eq!(request.tool_name, "Bash");
    assert_eq!(request.display_name, "Bash");
    assert_eq!(request.input["command"], "touch newjen_probe.txt");
    assert!(
        request
            .tool_use_id
            .as_deref()
            .unwrap()
            .starts_with("toolu_")
    );
    assert!(request.description.is_some());
    assert_eq!(request.suggestions.len(), 3);
}
