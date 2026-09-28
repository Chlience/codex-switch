use codex_sw::{
    config,
    process::{Instance, Scan},
    storage::{self, Change, Journal, Paths, Snapshot},
    switcher::{self, AuthMode, ImportOptions},
};
use serde_json::json;
use std::fs;
#[cfg(unix)]
use std::path::Path;
use tempfile::TempDir;

fn setup(config: &str) -> (TempDir, Paths) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("codex home 中文");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), config).unwrap();
    (temp, Paths::new(Some(home)).unwrap())
}

fn inline_config(token: &str) -> String {
    format!(
        "# user comment\nmodel_provider = \"proxy\" # provider note\nmodel = \"model-a\"\n[model_providers.proxy]\nname = \"Proxy\"\nbase_url = \"https://example.invalid/v1\"\nexperimental_bearer_token = \"{token}\"\n[mcp_servers.unchanged]\ncommand = \"example-tool\" # retained\n[features]\nshell_snapshot = false\n"
    )
}

fn native_config() -> &'static str {
    "model_provider = \"proxy\"\nmodel = \"model-a\"\n[model_providers.proxy]\nname = \"Proxy\"\nbase_url = \"https://example.invalid/v1\"\nrequires_openai_auth = true\n"
}

fn import(paths: &Paths, name: &str) {
    switcher::import(
        paths,
        ImportOptions {
            name: Some(name.into()),
            ..Default::default()
        },
    )
    .unwrap();
}

fn quiet_scan() -> Scan {
    Scan {
        available: true,
        instances: vec![],
    }
}

fn native_auth(key: &str) -> Vec<u8> {
    serde_json::to_vec(
        &json!({"auth_mode":"apikey", "OPENAI_API_KEY":key, "unknown_field":{"retained":true}}),
    )
    .unwrap()
}

#[test]
fn import_preserves_source_and_switch_preserves_unrelated_settings() {
    let original = inline_config("dummy-a");
    let (_temp, paths) = setup(&original);
    import(&paths, "work");
    assert_eq!(fs::read_to_string(paths.config()).unwrap(), original);
    fs::write(
        paths.config(),
        original
            .replace("model-a", "other-model")
            .replace("dummy-a", "other-key"),
    )
    .unwrap();
    switcher::switch(&paths, "work", AuthMode::Auto, false, quiet_scan()).unwrap();
    let switched = fs::read_to_string(paths.config()).unwrap();
    assert!(switched.contains("model = \"other-model\""));
    assert!(switched.contains("experimental_bearer_token = \"dummy-a\""));
    assert!(switched.contains("# user comment"));
    assert!(switched.contains("# provider note"));
    assert!(switched.contains("command = \"example-tool\" # retained"));
    assert!(switched.contains("shell_snapshot = false"));
    assert_eq!(switcher::current(&paths).unwrap()["name"], "work");
}

#[test]
fn new_and_legacy_presets_preserve_current_and_absent_model_settings() {
    let saved_settings = r#"model = "saved-model"
model_reasoning_effort = "low"
model_reasoning_summary = "auto"
model_verbosity = "low"
model_context_window = 100000
model_auto_compact_token_limit = 80000
model_supports_reasoning_summaries = false
model_catalog_json = "./saved-catalog.json"
service_tier = "priority"
"#;
    let current_settings = r#"model = "current-model" # keep model comment
model_reasoning_effort = "high"
model_reasoning_summary = "detailed"
model_verbosity = "high"
model_context_window = 200000
model_auto_compact_token_limit = 160000
model_supports_reasoning_summaries = true
model_catalog_json = "./current-catalog.json"
service_tier = "flex"
"#;
    let source = format!(
        "{saved_settings}{}",
        inline_config("dummy-saved").replace("model = \"model-a\"\n", "")
    );
    let saved_doc = config::parse(saved_settings).unwrap();
    for legacy in [false, true] {
        for settings in [current_settings, ""] {
            let (_temp, paths) = setup(&source);
            import(&paths, "work");
            let mut preset = switcher::load_preset(&paths, "work").unwrap();
            let selected = config::parse(&preset.config).unwrap();
            for (key, _) in saved_doc.iter() {
                assert!(selected.get(key).is_none(), "import captured {key}");
            }
            if legacy {
                // Existing version-1 records may contain stale or invalid model values.
                preset.config = format!("{saved_settings}{}", preset.config)
                    .replace("model = \"saved-model\"", "model = \"\"");
                fs::write(paths.preset("work"), serde_json::to_vec(&preset).unwrap()).unwrap();
            }
            assert!(!config::summary(&preset).unwrap().contains_key("model"));
            let saved_bytes = fs::read(paths.preset("work")).unwrap();
            let active = format!(
                "{settings}model_provider = 'other'\n[model_providers.other]\nname='Other'\nbase_url='https://other.invalid/v1'\n[models.new_thread]\nmodel='future-model'\n"
            );
            fs::write(paths.config(), &active).unwrap();
            let before = config::parse(&active).unwrap();
            let preview =
                switcher::switch(&paths, "work", AuthMode::Auto, true, quiet_scan()).unwrap();
            assert_eq!(
                preview.changed_keys,
                ["model_provider", "model_providers.<selected>"]
            );
            assert_eq!(fs::read_to_string(paths.config()).unwrap(), active);
            switcher::switch(&paths, "work", AuthMode::Auto, false, quiet_scan()).unwrap();
            let mut after = switcher::load_config(&paths).unwrap();
            for (key, _) in saved_doc.iter() {
                assert_eq!(
                    after.get(key).map(ToString::to_string),
                    before.get(key).map(ToString::to_string),
                    "switch changed {key} (legacy={legacy})"
                );
            }
            assert_eq!(after["models"].to_string(), before["models"].to_string());
            assert_eq!(
                after["model_providers"]["other"].to_string(),
                before["model_providers"]["other"].to_string()
            );
            assert_eq!(after["model_provider"].as_str(), Some("proxy"));
            assert_eq!(
                after["model_providers"]["proxy"]["experimental_bearer_token"].as_str(),
                Some("dummy-saved")
            );
            after["model"] = toml_edit::value("user-selected-model");
            fs::write(paths.config(), after.to_string()).unwrap();
            let current = switcher::current(&paths).unwrap();
            assert_eq!(current["name"], "work");
            assert_eq!(current["model"], "user-selected-model");
            let reimport = switcher::import(
                &paths,
                ImportOptions {
                    name: Some("work".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(reimport.unchanged, ["work"]);
            assert_eq!(fs::read(paths.preset("work")).unwrap(), saved_bytes);
        }
    }
}

#[test]
fn add_only_saves_provider_settings_and_switches_its_route() {
    let source = format!(
        "openai_base_url='https://saved.example/v1'\n{}",
        inline_config("dummy-saved")
    );
    let active = source
        .replace("saved.example", "current.example")
        .replace("model-a", "current-model")
        .replace("dummy-saved", "dummy-current");
    let (_temp, paths) = setup(&active);
    switcher::add(
        &paths,
        config::Preset {
            format_version: 1,
            name: "work".into(),
            provider: "proxy".into(),
            config: source,
            credential: config::Credential::Provider,
            source: "test".into(),
        },
        None,
    )
    .unwrap();
    let preset = switcher::load_preset(&paths, "work").unwrap();
    let saved = config::parse(&preset.config).unwrap();
    assert!(saved.get("model").is_none());
    assert!(saved.get("features").is_none());
    assert!(saved.get("mcp_servers").is_none());
    assert_eq!(fs::read_to_string(paths.config()).unwrap(), active);
    let result = switcher::switch(&paths, "work", AuthMode::Auto, false, quiet_scan()).unwrap();
    assert_eq!(
        result.changed_keys,
        ["openai_base_url", "model_providers.<selected>"]
    );
    let after = switcher::load_config(&paths).unwrap();
    assert_eq!(after["model"].as_str(), Some("current-model"));
    assert_eq!(
        after["openai_base_url"].as_str(),
        Some("https://saved.example/v1")
    );
    assert_eq!(after["features"]["shell_snapshot"].as_bool(), Some(false));
}

#[test]
fn dry_run_does_not_create_store_or_modify_files() {
    let original = inline_config("dummy-a");
    let (_temp, paths) = setup(&original);
    let result = switcher::import(
        &paths,
        ImportOptions {
            name: Some("work".into()),
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.imported, ["work"]);
    assert!(!paths.store.exists());
    import(&paths, "work");
    let state_before = Snapshot::read(&paths.state()).unwrap();
    let result = switcher::switch(&paths, "work", AuthMode::Auto, true, quiet_scan()).unwrap();
    assert!(result.dry_run);
    assert_eq!(Snapshot::read(&paths.state()).unwrap(), state_before);
    assert_eq!(fs::read_to_string(paths.config()).unwrap(), original);
    assert!(!paths.auth().exists());
}

#[test]
fn duplicate_import_is_idempotent_and_different_content_is_preserved() {
    let (_temp, paths) = setup(&inline_config("dummy-a"));
    import(&paths, "work");
    let saved = fs::read(paths.preset("work")).unwrap();
    let result = switcher::import(
        &paths,
        ImportOptions {
            name: Some("work".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.unchanged, ["work"]);
    fs::write(paths.config(), inline_config("dummy-b")).unwrap();
    assert!(
        switcher::import(
            &paths,
            ImportOptions {
                name: Some("work".into()),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert_eq!(fs::read(paths.preset("work")).unwrap(), saved);
}

#[test]
fn process_warning_does_not_block_native_switch() {
    let (_temp, paths) = setup(native_config());
    let bytes = native_auth("dummy-a");
    fs::write(paths.auth(), &bytes).unwrap();
    import(&paths, "work");
    fs::write(paths.auth(), native_auth("dummy-b")).unwrap();
    let scan = Scan {
        available: true,
        instances: vec![Instance {
            pid: 12345,
            scope: "相同 Codex 目录".into(),
        }],
    };
    let result = switcher::switch(&paths, "work", AuthMode::Copy, false, scan).unwrap();
    assert!(result.auth_changed);
    assert_eq!(result.active_instances.instances[0].pid, 12345);
    assert_eq!(fs::read(paths.auth()).unwrap(), bytes);
}

#[test]
fn native_import_preserves_every_byte_and_unknown_fields() {
    let (_temp, paths) = setup(native_config());
    let bytes =
        b"{\n  \"auth_mode\": \"apikey\", \"OPENAI_API_KEY\": \"dummy-key\", \"future\": 123\n}\n";
    fs::write(paths.auth(), bytes).unwrap();
    import(&paths, "work");
    assert_eq!(fs::read(paths.credential("work")).unwrap(), bytes);
    assert_eq!(fs::read(paths.auth()).unwrap(), bytes);
}

#[test]
fn all_import_does_not_bind_current_key_to_inactive_provider() {
    let (_temp, paths) = setup(&format!(
        "{}\n[model_providers.other]\nname=\"Other\"\nrequires_openai_auth=true\n",
        native_config()
    ));
    fs::write(paths.auth(), native_auth("dummy-a")).unwrap();
    switcher::import(
        &paths,
        ImportOptions {
            all: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        switcher::load_preset(&paths, "proxy").unwrap().credential,
        config::Credential::NativeFile
    ));
    assert!(matches!(
        switcher::load_preset(&paths, "other").unwrap().credential,
        config::Credential::MissingNative
    ));
    assert!(!paths.credential("other").exists());
}

#[test]
fn import_retains_environment_reference_without_capturing_value() {
    let (_temp, paths) = setup(
        "model_provider='proxy'\n[model_providers.proxy]\nname='Proxy'\nenv_key='CODEX_SW_TEST_MISSING_KEY_79337'\n",
    );
    import(&paths, "work");
    let preset = switcher::load_preset(&paths, "work").unwrap();
    assert!(preset.config.contains("CODEX_SW_TEST_MISSING_KEY_79337"));
    let before = fs::read(paths.config()).unwrap();
    let error = switcher::switch(&paths, "work", AuthMode::Auto, false, quiet_scan())
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("环境变量"));
    assert_eq!(fs::read(paths.config()).unwrap(), before);
}

#[test]
fn empty_token_import_can_be_reviewed_but_cannot_be_activated() {
    let (_temp, paths) = setup(&inline_config(""));
    let result = switcher::import(
        &paths,
        ImportOptions {
            name: Some("empty".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!result.warnings.is_empty());
    assert!(
        switcher::load_preset(&paths, "empty")
            .unwrap()
            .config
            .contains("experimental_bearer_token = \"\"")
    );
    assert!(switcher::switch(&paths, "empty", AuthMode::Auto, false, quiet_scan()).is_err());
}

#[test]
fn bearer_with_native_flag_and_no_auth_file_remains_usable() {
    let (_temp, paths) = setup(&format!(
        "{}experimental_bearer_token='dummy-token'\n",
        native_config()
    ));
    import(&paths, "work");
    switcher::switch(&paths, "work", AuthMode::Auto, false, quiet_scan()).unwrap();
    assert!(!paths.auth().exists());
}

#[test]
fn profiles_merge_provider_overrides_and_keep_source_untouched() {
    let original = inline_config("dummy-base");
    let (_temp, paths) = setup(&original);
    fs::write(paths.home.join("work.config.toml"), "model='profile-model'\n[model_providers.proxy]\nexperimental_bearer_token='dummy-profile'\n").unwrap();
    switcher::import(
        &paths,
        ImportOptions {
            name: Some("saved".into()),
            profile: Some("work".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let preset = switcher::load_preset(&paths, "saved").unwrap();
    let doc = config::parse(&preset.config).unwrap();
    assert!(doc.get("model").is_none());
    assert_eq!(
        doc["model_providers"]["proxy"]["base_url"].as_str(),
        Some("https://example.invalid/v1")
    );
    assert_eq!(
        doc["model_providers"]["proxy"]["experimental_bearer_token"].as_str(),
        Some("dummy-profile")
    );
    assert_eq!(fs::read_to_string(paths.config()).unwrap(), original);
}

#[test]
fn legacy_profile_import_does_not_copy_legacy_selector_to_preset() {
    let (_temp, paths) = setup(
        "profile='old'\nmodel_provider='proxy'\n[model_providers.proxy]\nname='Proxy'\n[profiles.old]\nmodel='old-model'\n",
    );
    import(&paths, "saved");
    let preset = switcher::load_preset(&paths, "saved").unwrap();
    let doc = config::parse(&preset.config).unwrap();
    assert!(doc.get("model").is_none());
    assert!(doc.get("profile").is_none());
    assert!(doc.get("profiles").is_none());
}

#[test]
fn undo_restores_config_and_rejects_external_edits() {
    let (_temp, paths) = setup(&inline_config("dummy-a"));
    import(&paths, "work");
    let original = inline_config("dummy-b");
    fs::write(paths.config(), &original).unwrap();
    switcher::switch(&paths, "work", AuthMode::Copy, false, quiet_scan()).unwrap();
    switcher::undo(&paths).unwrap();
    assert_eq!(fs::read_to_string(paths.config()).unwrap(), original);
    switcher::switch(&paths, "work", AuthMode::Copy, false, quiet_scan()).unwrap();
    fs::write(paths.config(), "# changed externally\n").unwrap();
    assert!(switcher::undo(&paths).is_err());
    assert_eq!(
        fs::read_to_string(paths.config()).unwrap(),
        "# changed externally\n"
    );
}

#[test]
fn prepared_transaction_rolls_back_only_known_changes() {
    let (_temp, paths) = setup("model='old'\n");
    let _lock = paths.lock().unwrap();
    let change = Change::new(paths.config(), Snapshot::file(b"model='new'\n".to_vec())).unwrap();
    let journal = Journal::new(vec![change.clone()], true);
    storage::atomic_write(
        &paths.pending(),
        &serde_json::to_vec(&journal).unwrap(),
        None,
    )
    .unwrap();
    storage::atomic_write(&paths.config(), b"model='new'\n", Some(0o600)).unwrap();
    assert!(storage::recover(&paths).unwrap());
    assert_eq!(Snapshot::read(&paths.config()).unwrap(), change.before);
    storage::atomic_write(
        &paths.pending(),
        &serde_json::to_vec(&journal).unwrap(),
        None,
    )
    .unwrap();
    fs::write(paths.config(), "model='external'\n").unwrap();
    assert!(storage::recover(&paths).is_err());
    assert!(paths.pending().exists());
    assert_eq!(
        fs::read_to_string(paths.config()).unwrap(),
        "model='external'\n"
    );
}

#[test]
fn committed_transaction_finishes_history_on_recovery() {
    let (_temp, paths) = setup("model='old'\n");
    let _lock = paths.lock().unwrap();
    let mut journal = Journal::new(
        vec![Change::new(paths.config(), Snapshot::file(b"model='new'\n".to_vec())).unwrap()],
        true,
    );
    journal.committed = true;
    storage::atomic_write(&paths.config(), b"model='new'\n", Some(0o600)).unwrap();
    storage::atomic_write(
        &paths.pending(),
        &serde_json::to_vec(&journal).unwrap(),
        None,
    )
    .unwrap();
    storage::recover(&paths).unwrap();
    assert!(paths.undo().exists());
    assert!(
        paths
            .store
            .join("history")
            .join(format!("{}.json", journal.id))
            .exists()
    );
    assert!(!paths.pending().exists());
}

#[test]
fn transaction_refuses_changes_since_the_planning_snapshot() {
    let (_temp, paths) = setup("model='old'\n");
    let _lock = paths.lock().unwrap();
    let journal = Journal::new(
        vec![
            Change::new(
                paths.config(),
                Snapshot::file(b"model='planned'\n".to_vec()),
            )
            .unwrap(),
        ],
        true,
    );
    fs::write(paths.config(), "model='external'\n").unwrap();
    assert!(storage::execute(&paths, journal).is_err());
    assert!(!paths.pending().exists());
    assert_eq!(
        fs::read_to_string(paths.config()).unwrap(),
        "model='external'\n"
    );
}

#[test]
fn current_detects_a_changed_native_login() {
    let (_temp, paths) = setup(native_config());
    fs::write(paths.auth(), native_auth("dummy-a")).unwrap();
    import(&paths, "a");
    switcher::switch(&paths, "a", AuthMode::Copy, false, quiet_scan()).unwrap();
    assert_eq!(switcher::current(&paths).unwrap()["name"], "a");
    fs::write(paths.auth(), native_auth("dummy-changed")).unwrap();
    assert_eq!(
        switcher::current(&paths).unwrap()["name"],
        serde_json::Value::Null
    );
}

#[test]
fn rejects_malformed_native_credentials_without_leaking_contents() {
    for bytes in [
        b"{\"tokens\":{}}".as_slice(),
        b"{\"auth_mode\":123,\"OPENAI_API_KEY\":\"secret\"}".as_slice(),
    ] {
        let error = config::validate_auth(bytes).err().unwrap();
        assert!(!error.to_string().contains("secret"));
    }
}

fn oauth(refresh: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"auth_mode":"chatgpt", "tokens":{"account_id":"account-a", "access_token":"dummy-access", "refresh_token":refresh}})).unwrap()
}

#[test]
fn copy_mode_saves_refreshed_credentials_before_switching() {
    let (_temp, paths) = setup(native_config());
    fs::write(paths.auth(), oauth("old-refresh")).unwrap();
    import(&paths, "a");
    fs::write(paths.auth(), native_auth("dummy-b")).unwrap();
    import(&paths, "b");
    switcher::switch(&paths, "a", AuthMode::Copy, false, quiet_scan()).unwrap();
    fs::write(paths.auth(), oauth("new-refresh")).unwrap();
    switcher::switch(&paths, "b", AuthMode::Copy, false, quiet_scan()).unwrap();
    assert_eq!(
        fs::read(paths.credential("a")).unwrap(),
        oauth("new-refresh")
    );
    switcher::switch(&paths, "a", AuthMode::Copy, false, quiet_scan()).unwrap();
    assert_eq!(fs::read(paths.auth()).unwrap(), oauth("new-refresh"));
    fs::write(paths.auth(), oauth("newest-refresh")).unwrap();
    switcher::switch(&paths, "a", AuthMode::Copy, false, quiet_scan()).unwrap();
    assert_eq!(fs::read(paths.auth()).unwrap(), oauth("newest-refresh"));
    assert_eq!(
        fs::read(paths.credential("a")).unwrap(),
        oauth("newest-refresh")
    );
}

#[test]
fn parse_errors_and_summaries_do_not_leak_secrets() {
    let (_temp, paths) = setup("experimental_bearer_token = \"super-secret-raw\n");
    let error = switcher::import(
        &paths,
        ImportOptions {
            name: Some("a".into()),
            ..Default::default()
        },
    )
    .err()
    .unwrap();
    assert!(!format!("{error:#}").contains("super-secret-raw"));
    fs::write(paths.config(), inline_config("super-secret-raw")).unwrap();
    import(&paths, "a");
    let summary = serde_json::to_string(
        &config::summary(&switcher::load_preset(&paths, "a").unwrap()).unwrap(),
    )
    .unwrap();
    assert!(!summary.contains("super-secret-raw"));
}

#[test]
fn rejects_traversal_and_portability_conflicts() {
    for name in ["../escape", "a/b", "a\\b", "", "CON", "nul", "x."] {
        assert!(config::validate_name(name).is_err());
    }
    let (_temp, paths) = setup(&inline_config("dummy"));
    import(&paths, "work");
    assert!(
        switcher::import(
            &paths,
            ImportOptions {
                name: Some("WORK".into()),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn process_classifier_ignores_prompts_and_recognizes_node_wrapper() {
    use std::ffi::OsStr;
    assert!(codex_sw::process::is_codex(
        OsStr::new("codex.exe"),
        None,
        &[]
    ));
    assert!(codex_sw::process::is_codex(
        OsStr::new("node"),
        None,
        &[
            "node".into(),
            "/opt/node_modules/@openai/codex/bin/codex.js".into()
        ]
    ));
    assert!(!codex_sw::process::is_codex(
        OsStr::new("bash"),
        None,
        &["bash".into(), "-c".into(), "echo codex".into()]
    ));
    assert!(!codex_sw::process::is_codex(
        OsStr::new("codex-sw"),
        None,
        &[]
    ));
}

#[cfg(unix)]
#[test]
fn unix_links_and_private_file_modes() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, paths) = setup(native_config());
    fs::write(paths.auth(), native_auth("dummy-key")).unwrap();
    import(&paths, "a");
    switcher::switch(&paths, "a", AuthMode::Symlink, false, quiet_scan()).unwrap();
    assert!(paths.auth().is_symlink());
    assert_eq!(
        fs::read_link(paths.auth()).unwrap(),
        Path::new("codex-sw/credentials/auth.json.a")
    );
    assert_eq!(
        fs::metadata(paths.credential("a"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&paths.store).unwrap().permissions().mode() & 0o777,
        0o700
    );
    fs::write(paths.auth(), native_auth("updated-key")).unwrap();
    assert_eq!(
        fs::read(paths.credential("a")).unwrap(),
        native_auth("updated-key")
    );
}

#[cfg(unix)]
#[test]
fn config_symlink_is_preserved_and_external_target_is_edited() {
    let (temp, paths) = setup(&inline_config("dummy-a"));
    import(&paths, "a");
    let target = temp.path().join("actual-config.toml");
    fs::rename(paths.config(), &target).unwrap();
    std::os::unix::fs::symlink(&target, paths.config()).unwrap();
    fs::write(&target, inline_config("dummy-b")).unwrap();
    switcher::switch(&paths, "a", AuthMode::Auto, false, quiet_scan()).unwrap();
    assert!(paths.config().is_symlink());
    assert!(fs::read_to_string(target).unwrap().contains("dummy-a"));
}
