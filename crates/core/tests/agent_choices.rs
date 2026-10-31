use std::time::Duration;

use newjen_core::agent::{AgentConfig, ConversationState, EffortSelection, Speed};
use newjen_core::providers::*;
use newjen_core::{SessionConfig, Store, Thread};

fn detected(provider: Provider) -> ProviderStatus {
    ProviderStatus {
        provider,
        availability: Availability::Ready {
            binary: "/usr/bin/false".into(),
            version: "test".into(),
            account: Account {
                billing: Billing::Subscription,
                plan: None,
                email: None,
            },
            catalog: ModelCatalog {
                source: CatalogSource::AccountModels,
                models: vec![ModelOption {
                    id: "chosen".into(),
                    label: "Chosen".into(),
                    efforts: vec!["low".into()],
                    default_effort: None,
                    service_tiers: vec![],
                    default_service_tier: None,
                    fast: Some(ServiceTier {
                        id: "priority".into(),
                        label: "Fast".into(),
                        description: "Extra usage".into(),
                    }),
                }],
            },
            rate_limits: None,
            warnings: vec![],
        },
    }
}

#[test]
fn choices_require_the_right_ready_catalog_and_supported_settings() {
    let status = detected(Provider::Codex);
    let mut config = AgentConfig::cli_default(Provider::Codex);
    config.validate(&status).unwrap();
    config.effort = EffortSelection::Chosen("low".into());
    assert!(config.validate(&status).is_err());
    config.model = ModelSelection::Model {
        id: "chosen".into(),
    };
    config.speed = Speed::Fast;
    config.validate(&status).unwrap();
    config.effort = EffortSelection::Chosen("invented".into());
    assert!(config.validate(&status).is_err());
    assert!(config.validate(&detected(Provider::Claude)).is_err());
}

#[test]
fn drafts_use_the_detected_binary_and_codex_has_no_invented_conversation() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(home.path()).unwrap();
    let thread = Thread::draft_with_agent(
        store.clone(),
        &project,
        AgentConfig::cli_default(Provider::Codex),
        &detected(Provider::Codex),
    )
    .unwrap();
    assert_eq!(thread.conversation(), ConversationState::Uncreated);
    assert_eq!(thread.agent_config().unwrap().provider, Provider::Codex);
    assert_eq!(
        thread.session_config().program,
        std::path::Path::new("/usr/bin/false")
    );
    assert!(store.threads(project.id).unwrap().is_empty());
}

#[test]
fn saved_choices_survive_relaunch_and_unknown_providers_stay_unavailable() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(home.path()).unwrap();
    let mut transport = SessionConfig::new(home.path());
    transport.program = "/bin/cat".into();
    // cat reads the input without creating a provider turn; quit settles the saved message.
    transport.extra_args = vec![];
    let mut thread = Thread::new_draft(store.clone(), &project, transport);
    thread.send("saved message").unwrap();
    thread.shutdown_for_quit();
    assert!(store.flush(Duration::from_secs(5)));
    let mut row = store.thread(thread.id()).unwrap().unwrap();
    row.model = ModelSelection::Model {
        id: "chosen".into(),
    };
    row.effort = EffortSelection::Chosen("low".into());
    row.speed = Speed::Fast;
    store.save_thread(row.clone());
    assert!(store.flush(Duration::from_secs(5)));
    let loaded = Thread::load(
        store.clone(),
        &project,
        store.thread(thread.id()).unwrap().unwrap(),
        SessionConfig::new(home.path()),
    )
    .unwrap();
    let config = loaded.session_config();
    assert_eq!(config.agent.model, row.model);
    assert_eq!(config.agent.effort, row.effort);
    assert_eq!(config.agent.speed, row.speed);
    let args = config.args();
    assert!(args.windows(2).any(|pair| pair == ["--model", "chosen"]));
    assert!(args.windows(2).any(|pair| pair == ["--effort", "low"]));
    assert!(args.windows(2).any(|pair| pair == ["--fast-mode", "on"]));
    let mut codex = row.clone();
    codex.provider = "codex".into();
    codex.conversation = ConversationState::Uncreated;
    store.save_thread(codex.clone());
    assert!(store.flush(Duration::from_secs(5)));
    assert_eq!(
        store.thread(thread.id()).unwrap().unwrap().conversation,
        ConversationState::Uncreated
    );
    let mut invalid = codex;
    invalid.conversation = ConversationState::Reserved("not-a-codex-id".into());
    assert!(
        Thread::load(
            store.clone(),
            &project,
            invalid,
            SessionConfig::new(home.path())
        )
        .is_err()
    );
    row.provider = "future-agent".into();
    let mut unknown = Thread::load(
        store.clone(),
        &project,
        row,
        SessionConfig::new(home.path()),
    )
    .unwrap();
    assert!(unknown.agent_config().is_none());
    assert!(
        unknown
            .unavailable_reason()
            .unwrap()
            .contains("future-agent")
    );
    assert!(unknown.send("must not launch Claude").is_err());
    assert!(!unknown.has_session());
    assert!(!unknown.transcript().items().is_empty());
}
