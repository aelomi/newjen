use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/providers");

fn fixture(name: &str) -> Value {
    serde_json::from_slice(&fs::read(Path::new(FIXTURES).join(name)).unwrap()).unwrap()
}

#[test]
fn real_reports_parse_without_credentials_or_model_requests() {
    let auth: ClaudeAuth = serde_json::from_value(fixture("claude-auth.json")).unwrap();
    assert!(auth.logged_in);
    let account = Account::from(auth);
    assert_eq!(account.billing, Billing::Subscription);
    assert_eq!(account.plan.as_deref(), Some("max"));
    assert_eq!(account.email.as_deref(), Some("user@example.com"));
    assert_eq!(
        parse_version(include_str!(
            "../../../tests/fixtures/providers/claude-version.txt"
        ))
        .unwrap(),
        "2.1.316"
    );
    let init: Initialize =
        serde_json::from_value(fixture("codex-initialize.json")["result"].clone()).unwrap();
    assert_eq!(parse_version(&init.user_agent).unwrap(), "0.166.0");
    let auth: CodexAuth =
        serde_json::from_value(fixture("codex-account.json")["result"].clone()).unwrap();
    assert!(auth.requires_openai_auth);
    assert_eq!(
        Account::from(auth.account.unwrap()).billing,
        Billing::Subscription
    );
    let page: ModelPage =
        serde_json::from_value(fixture("codex-models.json")["result"].clone()).unwrap();
    assert!(page.next_cursor.is_none());
    let model = ModelOption::from(page.data.into_iter().next().unwrap());
    assert_eq!(model.id, "gpt-6.1-sol");
    assert_eq!(model.fast.unwrap().id, "priority");
    assert_eq!(
        model.efforts,
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );
    assert_eq!(model.default_effort.as_deref(), Some("low"));
    let limits: RateLimits =
        serde_json::from_value(fixture("codex-rate-limits.json")["result"].clone()).unwrap();
    assert_eq!(limits.ordinary_usage_allowed, Some(true));
    assert_eq!(
        limits.rate_limits.primary.as_ref().unwrap().used_percent,
        17
    );
    assert_eq!(
        limits
            .rate_limits
            .secondary
            .as_ref()
            .unwrap()
            .window_duration_mins,
        Some(10080)
    );
    assert_eq!(
        limits.by_limit_id.as_ref().unwrap()["codex"],
        limits.rate_limits
    );
    assert_eq!(
        limits.reset_credits.unwrap().credits.unwrap()[0].id,
        "REDACTED_RESET_CREDIT"
    );
}

#[test]
fn claude_catalog_stays_with_documented_syntax() {
    let help = include_str!("../../../tests/fixtures/providers/claude-help.txt");
    for text in [
        "'fable'",
        "'opus'",
        "'sonnet'",
        "low, medium, high, xhigh, max",
        "--settings <file-or-json>",
    ] {
        assert!(help.contains(text));
    }
    let catalog = claude_catalog();
    assert_eq!(catalog.source, CatalogSource::CliAliases);
    assert_eq!(catalog.models.len(), 4);
    assert!(catalog.models.iter().all(|m| {
        m.fast
            .as_ref()
            .is_some_and(|fast| fast.description.contains("extra usage"))
            && m.default_effort.is_none()
    }));
}

fn script(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn claude_script(auth: &str, version: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = script(
        &dir,
        "claude",
        &format!("if [ \"$1\" = --version ]; then echo '{version}'; else {auth}; fi"),
    );
    (dir, path)
}

fn run(provider: Provider, binary: PathBuf, api: bool) -> Availability {
    detect_at(provider, binary, Duration::from_secs(2), api).availability
}

#[test]
fn not_installed_has_actionable_hints() {
    for provider in [Provider::Claude, Provider::Codex] {
        // Bypass real installation lookup to exercise the missing result deterministically.
        let status = run(provider, provider.cli().name().into(), false);
        let Availability::NotInstalled { hint } = status else {
            panic!("{status:?}")
        };
        assert!(hint.contains("https://"));
        if provider == Provider::Codex {
            assert!(hint.contains("desktop app"));
        }
    }
}

#[test]
fn claude_signed_out_nonzero_json_is_not_broken() {
    let (_dir, path) = claude_script("echo '{\"loggedIn\":false}'; exit 1", "2.1.316");
    let status = run(Provider::Claude, path, false);
    let Availability::SignedOut { hint, .. } = status else {
        panic!("{status:?}")
    };
    assert!(hint.contains("`claude`"));
}

#[test]
fn claude_reports_api_billing_and_old_version_without_blocking() {
    let (_dir, path) = claude_script(
        "echo '{\"loggedIn\":true,\"authMethod\":\"api_key\"}'",
        "2.1.100",
    );
    let status = run(Provider::Claude, path, true);
    let Availability::Ready {
        account, warnings, ..
    } = status
    else {
        panic!("{status:?}")
    };
    assert_eq!(account.billing, Billing::ApiKey);
    assert!(warnings.contains(&Warning::ApiKeyEnvironment {
        variable: "ANTHROPIC_API_KEY"
    }));
    assert!(warnings.contains(&Warning::VersionBelowFloor {
        found: "2.1.100".into(),
        minimum: "2.1.280"
    }));
    assert!(warnings.contains(&Warning::VersionBelowTested {
        found: "2.1.100".into(),
        tested: "2.1.316"
    }));
}

#[test]
fn claude_bad_json_nonzero_and_missing_override_are_broken() {
    for auth in [
        "echo garbage",
        "exit 3",
        "echo '{\"loggedIn\":true}'; exit 1",
        "echo '{\"loggedIn\":false}'; exit 2",
    ] {
        let (_dir, path) = claude_script(auth, "2.1.316");
        assert!(matches!(
            run(Provider::Claude, path, false),
            Availability::Broken { .. }
        ));
    }
    assert!(matches!(
        run(
            Provider::Claude,
            PathBuf::from("/nonexistent/newjen-test-cli"),
            false
        ),
        Availability::Broken { .. }
    ));
}

#[test]
fn both_providers_have_total_deadlines_and_kill_descendants() {
    for provider in [Provider::Claude, Provider::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let path = script(&dir, "hang", "sleep 30 & wait");
        let start = Instant::now();
        let status = detect_at(provider, path, Duration::from_millis(70), false);
        let Availability::Broken { error, .. } = status.availability else {
            panic!("{status:?}")
        };
        assert!(error.contains("deadline"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

// A strict peer checks the complete allowlist and handshake, then replays the real reports.
fn codex_script(mode: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let quote = |value: String| value.replace('\'', "'\\''");
    let initialize = quote(fixture("codex-initialize.json").to_string());
    let account = match mode {
        "signed-out" => {
            json!({"id":2,"result":{"account":null,"requiresOpenaiAuth":true}}).to_string()
        }
        "api" => json!({"id":2,"result":{"account":{"type":"apiKey"},"requiresOpenaiAuth":true}})
            .to_string(),
        _ => fixture("codex-account.json").to_string(),
    };
    let account = quote(account);
    let models = fixture("codex-models.json");
    let mut first = models.clone();
    first["result"]["data"][1]["hidden"] = json!(true);
    if mode == "pages" {
        first["result"]["nextCursor"] = json!("page-2");
    }
    let mut second = models.clone();
    second["id"] = json!(4);
    second["result"]["data"] = json!([]);
    let mut limits = fixture("codex-rate-limits.json");
    if mode == "pages" {
        limits["id"] = json!(5);
    }
    if mode == "rate-error" {
        limits = json!({"id":4,"error":{"code":-1,"message":"not available"}});
    }
    let first = quote(first.to_string());
    let second = quote(second.to_string());
    let limits = quote(limits.to_string());
    let path = script(
        &dir,
        "codex",
        &format!(
            r#"
[ "$#" = 1 ] && [ "$1" = app-server ] || exit 11
IFS= read -r line
case "$line" in *'"method":"initialize"'*'"name":"newjen"'*'"title":"Newjen"'*'"version":"{crate_version}"'*) ;; *) exit 12 ;; esac
printf '%s\n' '{initialize}'
IFS= read -r line
[ "$line" = '{{"method":"initialized"}}' ] || exit 13
IFS= read -r line
case "$line" in *'"method":"account/read"'*'"refreshToken":false'*) ;; *) exit 14 ;; esac
printf '%s\n' '{account}'
IFS= read -r line
case "$line" in *'"method":"model/list"'*) ;; *) exit 15 ;; esac
printf '%s\n' '{{"method":"account/updated","params":{{}}}}'
printf '%s\n' '{first}'
if [ '{mode}' = pages ]; then
 IFS= read -r line
 case "$line" in *'"method":"model/list"'*'"cursor":"page-2"'*) ;; *) exit 16 ;; esac
 printf '%s\n' '{second}'
fi
IFS= read -r line
case "$line" in *'"method":"account/rateLimits/read"'*) ;; *) exit 17 ;; esac
printf '%s\n' '{limits}'
sleep 30
"#,
            crate_version = env!("CARGO_PKG_VERSION")
        ),
    );
    (dir, path)
}

#[test]
fn codex_read_only_handshake_models_limits_and_api_warning() {
    for mode in ["happy", "api", "pages", "rate-error"] {
        let (_dir, path) = codex_script(mode);
        let status = run(Provider::Codex, path, true);
        let Availability::Ready {
            account,
            catalog,
            rate_limits,
            warnings,
            version,
            ..
        } = status
        else {
            panic!("{mode}: {status:?}")
        };
        assert_eq!(version, "0.166.0");
        assert_eq!(
            account.billing,
            if mode == "api" {
                Billing::ApiKey
            } else {
                Billing::Subscription
            }
        );
        assert!(warnings.contains(&Warning::ApiKeyEnvironment {
            variable: "OPENAI_API_KEY"
        }));
        assert!(!catalog.models.is_empty());
        assert!(!catalog.models.iter().any(|m| m.id == "gpt-6-astra"));
        if mode == "rate-error" {
            assert!(rate_limits.is_none());
            assert!(warnings.contains(&Warning::RateLimitsUnavailable));
        } else {
            assert!(rate_limits.is_some());
        }
    }
}

#[test]
fn codex_signed_out() {
    let (_dir, path) = codex_script("signed-out");
    let status = run(Provider::Codex, path, false);
    let Availability::SignedOut { hint, .. } = status else {
        panic!("{status:?}")
    };
    assert!(hint.contains("codex login"));
}

#[test]
fn codex_malformed_output_nonzero_and_rpc_errors_are_broken() {
    for body in [
        "echo garbage",
        "exit 2",
        "echo '{\"id\":1,\"error\":{\"code\":-1}}'",
        "echo '{\"method\":\"account/login\",\"id\":1}'",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = script(&dir, "codex", body);
        assert!(matches!(
            run(Provider::Codex, path, false),
            Availability::Broken { .. }
        ));
    }
}

// Validate recorded wire replies against the CLI-generated schema subset, without new deps.
fn conforms(value: &Value, schema: &Value, root: &Value) -> bool {
    if let Some(reference) = schema["$ref"].as_str() {
        return conforms(
            value,
            root.pointer(reference.trim_start_matches('#')).unwrap(),
            root,
        );
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(choices) = schema[key].as_array() {
            let count = choices.iter().filter(|s| conforms(value, s, root)).count();
            if (key == "anyOf" && count == 0) || (key == "oneOf" && count != 1) {
                return false;
            }
        }
    }
    if let Some(parts) = schema["allOf"].as_array()
        && !parts.iter().all(|s| conforms(value, s, root))
    {
        return false;
    }
    if let Some(choices) = schema["enum"].as_array()
        && !choices.contains(value)
    {
        return false;
    }
    let matches_type = |kind: &str| match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => panic!("unknown type {kind}"),
    };
    match &schema["type"] {
        Value::String(kind) if !matches_type(kind) => return false,
        Value::Array(kinds)
            if !kinds
                .iter()
                .any(|kind| matches_type(kind.as_str().unwrap())) =>
        {
            return false;
        }
        _ => {}
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema["required"].as_array()
            && !required
                .iter()
                .all(|key| object.contains_key(key.as_str().unwrap()))
        {
            return false;
        }
        for (key, value) in object {
            if let Some(property) = schema["properties"].get(key) {
                if !conforms(value, property, root) {
                    return false;
                }
            } else if schema["additionalProperties"].is_object()
                && !conforms(value, &schema["additionalProperties"], root)
            {
                return false;
            }
        }
    }
    if let Some(items) = value.as_array()
        && schema.get("items").is_some()
        && !items
            .iter()
            .all(|value| conforms(value, &schema["items"], root))
    {
        return false;
    }
    true
}

#[test]
fn read_only_request_parameters_match_pinned_schemas() {
    for (value, name) in [
        (
            json!({"clientInfo":{"name":"newjen","title":"Newjen","version":env!("CARGO_PKG_VERSION")}}),
            "InitializeParams.json",
        ),
        (json!({"refreshToken":false}), "GetAccountParams.json"),
        (json!({"cursor":null}), "ModelListParams.json"),
        (json!({"cursor":"page-2"}), "ModelListParams.json"),
    ] {
        let schema = fixture(&format!("schema/{name}"));
        assert!(conforms(&value, &schema, &schema), "{name}");
    }
}

#[test]
fn codex_versions_and_account_variants_are_conservative() {
    let mut warnings = Vec::new();
    version_warnings(Provider::Codex, "0.155.0", &mut warnings);
    assert!(warnings.contains(&Warning::VersionBelowFloor {
        found: "0.155.0".into(),
        minimum: "0.156.0"
    }));
    assert!(warnings.contains(&Warning::VersionBelowTested {
        found: "0.155.0".into(),
        tested: "0.166.0"
    }));
    for kind in ["amazonBedrock", "gcp"] {
        let account: CodexAccount = serde_json::from_value(json!({"type":kind})).unwrap();
        assert_eq!(Account::from(account).billing, Billing::Unknown);
    }
    assert!(serde_json::from_value::<CodexAccount>(json!({"type":"futureAuth"})).is_err());
    assert!(parse_version("unexpected version output").is_err());
}

#[test]
fn recorded_replies_match_pinned_cli_schemas() {
    for (name, schema) in [
        ("codex-initialize.json", "InitializeResponse.json"),
        ("codex-account.json", "GetAccountResponse.json"),
        ("codex-models.json", "ModelListResponse.json"),
        (
            "codex-rate-limits.json",
            "GetAccountRateLimitsResponse.json",
        ),
    ] {
        let schema = fixture(&format!("schema/{schema}"));
        assert!(
            conforms(&fixture(name)["result"], &schema, &schema),
            "{name}"
        );
        assert!(
            !conforms(&json!({}), &schema, &schema),
            "required fields: {name}"
        );
    }
}

#[test]
#[ignore = "usage-free live CLI detection; requires locally installed CLIs"]
fn live_provider_detection_usage_free() {
    let statuses = detect_all();
    assert_eq!(statuses.len(), 2);
    for status in statuses {
        match status.availability {
            Availability::Ready {
                version,
                account,
                catalog,
                rate_limits,
                warnings,
                ..
            } => {
                assert!(version_tuple(&version).is_some());
                assert!(!catalog.models.is_empty());
                eprintln!(
                    "{:?}: ready, version {version}, billing {:?}, {} models, rate limits {}, {} warnings",
                    status.provider,
                    account.billing,
                    catalog.models.len(),
                    rate_limits.is_some(),
                    warnings.len()
                );
            }
            Availability::SignedOut { version, .. } => {
                eprintln!("{:?}: signed out ({version})", status.provider)
            }
            Availability::NotInstalled { .. } => eprintln!("{:?}: not installed", status.provider),
            Availability::Broken { error, .. } => panic!("{:?}: {error}", status.provider),
        }
    }
}
