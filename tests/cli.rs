// Keep subprocess checks separate from tests holding file locks. On Unix, a fork
// can inherit another test thread's lock until the child reaches exec.
use codex_sw::{config, storage::Paths, switcher};
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    process::{Command, Output, Stdio},
};
use tempfile::TempDir;

fn cli(paths: &Paths, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_codex-sw"))
        .arg("--codex-home")
        .arg(&paths.home)
        .args(args)
        .output()
        .unwrap()
}

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

fn native_auth(key: &str) -> Vec<u8> {
    serde_json::to_vec(
        &json!({"auth_mode":"apikey", "OPENAI_API_KEY":key, "unknown_field":{"retained":true}}),
    )
    .unwrap()
}

fn interactive_cli(paths: &Paths, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_codex-sw"))
        .arg("--codex-home")
        .arg(&paths.home)
        .args(["add", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

fn pasted_provider(name: &str) -> String {
    format!(
        "[model_providers.gateway]\nname='{name}'\nbase_url='https://provider.example/v1'\nwire_api='responses'\n"
    )
}

#[test]
fn interactive_add_uses_provider_id_and_preserves_auth_bytes() {
    let original = inline_config("dummy-active");
    let (_temp, paths) = setup(&original);
    let active_auth = native_auth("dummy-active");
    fs::write(paths.auth(), &active_auth).unwrap();
    let auth = "{\r\n  \"OPENAI_API_KEY\": \"dummy-pasted-secret\",\r\n  \"extra\": {\"preserved\": true}\r\n}\r\n";
    let input = format!(
        "model='unused-model'\nmodel_reasoning_effort='low'\n{}END\n{auth}END\r\n",
        pasted_provider("office")
    );
    let output = interactive_cli(&paths, &input);
    assert!(output.status.success(), "{:?}", output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result, json!({"added":"gateway"}));
    let preset = switcher::load_provider(&paths, "gateway").unwrap();
    assert_eq!(preset.provider, "gateway");
    assert!(matches!(preset.credential, config::Credential::NativeFile));
    let doc = config::parse(&preset.config).unwrap();
    assert!(doc.get("model").is_none());
    assert!(doc.get("model_reasoning_effort").is_none());
    assert_eq!(
        doc["model_providers"]["gateway"]["requires_openai_auth"].as_bool(),
        Some(true)
    );
    assert_eq!(
        fs::read(paths.credential("gateway")).unwrap(),
        auth.as_bytes()
    );
    assert_eq!(fs::read(paths.auth()).unwrap(), active_auth);
    assert_eq!(fs::read_to_string(paths.config()).unwrap(), original);
    assert!(!paths.state().exists());
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("dummy-pasted-secret"));
    }
    assert!(!String::from_utf8_lossy(&output.stderr).contains("预设名称："));
}

#[test]
fn interactive_add_checks_duplicate_ids_before_requesting_auth() {
    let (_temp, paths) = setup(&inline_config("dummy-active"));
    let args = ["add", "work", "--base-url", "https://provider.example/v1"];
    assert!(cli(&paths, &args).status.success());
    let saved = fs::read(paths.preset("work")).unwrap();
    assert!(!cli(&paths, &args).status.success());
    let input = format!(
        "{}END\n{}END\n{{\"OPENAI_API_KEY\":\"dummy-new\"}}\nEND\n",
        pasted_provider("Any display name")
            .replace("model_providers.gateway", "model_providers.work"),
        pasted_provider("office")
    );
    let output = interactive_cli(&paths, &input);
    assert!(output.status.success());
    let prompts = String::from_utf8_lossy(&output.stderr);
    assert!(prompts.find("已存在").unwrap() < prompts.find("粘贴 auth.json").unwrap());
    assert_eq!(fs::read(paths.preset("work")).unwrap(), saved);
    assert!(paths.preset("gateway").exists());
}

#[test]
fn interactive_add_retries_invalid_blocks_and_honors_provider_selector() {
    let (_temp, paths) = setup(&inline_config("dummy-active"));
    let multi = format!(
        "{}[model_providers.other]\nname='other'\nbase_url='https://other.example/v1'\n",
        pasted_provider("office")
    );
    let input = format!(
        "name='dummy-invalid-secret\nEND\n{multi}END\n{}END\nmodel_provider='gateway'\n{multi}END\n{{\"OPENAI_API_KEY\":\"dummy-invalid-json-secret\",}}\nEND\n{{\"OPENAI_API_KEY\":\"\"}}\nEND\n{{\"OPENAI_API_KEY\":\"dummy-valid-secret\"}}\nEND\n",
        pasted_provider("office").replace("https://provider.example/v1", "invalid-url")
    );
    let output = interactive_cli(&paths, &input);
    assert!(output.status.success());
    let prompts = String::from_utf8_lossy(&output.stderr);
    assert!(prompts.contains("配置含多个 provider"));
    assert!(prompts.contains("base_url 必须使用"));
    assert!(prompts.contains("API key 为空"));
    assert!(!prompts.contains("dummy-invalid-secret"));
    assert!(!prompts.contains("dummy-invalid-json-secret"));
    assert!(!prompts.contains("dummy-valid-secret"));
    assert_eq!(
        switcher::load_provider(&paths, "gateway").unwrap().provider,
        "gateway"
    );
    assert!(!paths.preset("other").exists());
}

#[test]
fn interactive_add_rejects_incomplete_or_oversized_input_without_writes() {
    let source = inline_config("dummy-active");
    for input in [
        String::new(),
        pasted_provider("office"),
        format!("{}END\n", pasted_provider("office")),
        format!(
            "{}END\n{{\"OPENAI_API_KEY\":\"dummy\"}}",
            pasted_provider("office")
        ),
        "x".repeat(1024 * 1024 + 1),
    ] {
        let (_temp, paths) = setup(&source);
        let output = interactive_cli(&paths, &input);
        assert!(!output.status.success());
        assert!(!paths.store.exists());
        assert_eq!(fs::read_to_string(paths.config()).unwrap(), source);
    }
    let (_temp, paths) = setup(&source);
    let output = cli(
        &paths,
        &["add", "--base-url", "https://provider.example/v1"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(!paths.store.exists());
}

#[test]
fn interactive_add_rechecks_provider_when_saving() {
    let (_temp, paths) = setup(&inline_config("dummy-active"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_codex-sw"))
        .arg("--codex-home")
        .arg(&paths.home)
        .args(["add", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{}END", pasted_provider("office")).unwrap();
    let mut prompts = BufReader::new(child.stderr.take().unwrap());
    loop {
        let mut line = String::new();
        assert!(prompts.read_line(&mut line).unwrap() > 0);
        if line.contains("粘贴 auth.json") {
            break;
        }
    }
    assert!(
        cli(
            &paths,
            &["add", "gateway", "--base-url", "https://other.example/v1"]
        )
        .status
        .success()
    );
    let saved = fs::read(paths.preset("gateway")).unwrap();
    writeln!(input, "{{\"OPENAI_API_KEY\":\"dummy-race\"}}\nEND").unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(paths.preset("gateway")).unwrap(), saved);
    assert!(!paths.credential("gateway").exists());
}

#[test]
fn cli_add_and_list_only_manage_provider_fields() {
    let original = inline_config("dummy-active");
    let (_temp, paths) = setup(&original);
    let run = |args: &[&str]| cli(&paths, args);
    let added = run(&[
        "add",
        "work",
        "--base-url",
        "https://provider.example/v1",
        "--env-key",
        "WORK_API_KEY",
    ]);
    assert!(added.status.success());
    let preset = switcher::load_preset(&paths, "work").unwrap();
    assert!(
        config::parse(&preset.config)
            .unwrap()
            .get("model")
            .is_none()
    );
    let listed = run(&["list"]);
    assert!(listed.status.success());
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        "Provider  端点\n────────  ────\nwork      https://provider.example/v1\n"
    );
    let listed_json = run(&["list", "--json"]);
    assert!(listed_json.status.success());
    let records: serde_json::Value = serde_json::from_slice(&listed_json.stdout).unwrap();
    assert!(records[0].get("name").is_none());
    assert_eq!(records[0]["endpoint"], "https://provider.example/v1");
    assert_eq!(records[0]["provider"], "work");
    assert!(records[0].get("model").is_none());
    let added = run(&["add", "示例", "--base-url", "https://provider.example/v1"]);
    assert!(added.status.success());
    let listed = run(&["list"]);
    assert!(listed.status.success());
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        "Provider  端点\n────────  ────\nwork      https://provider.example/v1\n示例      https://provider.example/v1\n"
    );
    let rejected = run(&[
        "add",
        "with-model",
        "--base-url",
        "https://provider.example/v1",
        "--model",
        "unexpected-model",
    ]);
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("--model"));
    assert!(!paths.preset("with-model").exists());
    assert_eq!(fs::read_to_string(paths.config()).unwrap(), original);
    let removed = run(&["remove", "示例", "--json"]);
    assert!(removed.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&removed.stdout).unwrap()["removed"],
        "示例"
    );
    assert!(switcher::load_provider(&paths, "示例").is_err());
    assert_eq!(run(&["import", "unused-alias"]).status.code(), Some(2));
    assert_eq!(
        run(&[
            "add",
            "unused-alias",
            "--provider",
            "work",
            "--base-url",
            "https://example.invalid"
        ])
        .status
        .code(),
        Some(2)
    );
}

#[test]
fn auth_command_is_not_executed_during_import_or_doctor() {
    let (_temp, paths) = setup(
        "model_provider='proxy'\n[model_providers.proxy]\nname='Proxy'\n[model_providers.proxy.auth]\ncommand='codex-sw-this-command-must-not-run'\n",
    );
    assert!(cli(&paths, &["import"]).status.success());
    let output = cli(&paths, &["doctor", "--json"]);
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], true);
}

#[test]
fn cli_switch_always_reminds_to_restart_without_process_details() {
    let (_temp, paths) = setup(native_config());
    fs::write(paths.auth(), native_auth("dummy-a")).unwrap();
    assert!(cli(&paths, &["import"]).status.success());
    fs::write(paths.auth(), native_auth("dummy-b")).unwrap();
    let output = cli(&paths, &["--json", "use", "proxy", "--auth-mode", "copy"]);
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "提示：已有 Codex 实例需要重启后使用新配置。\n"
    );
    assert_eq!(fs::read(paths.auth()).unwrap(), native_auth("dummy-a"));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["auth_changed"], true);
    assert!(json.get("active_instances").is_none());
    let undo = cli(&paths, &["undo"]);
    assert!(undo.status.success(), "{:?}", undo);
    assert_eq!(undo.stderr, output.stderr);
    assert_eq!(fs::read(paths.auth()).unwrap(), native_auth("dummy-b"));
}

#[test]
fn cli_read_only_and_failed_commands_do_not_request_a_restart() {
    let (_temp, paths) = setup(native_config());
    fs::write(paths.auth(), native_auth("dummy-a")).unwrap();
    assert!(cli(&paths, &["import"]).status.success());
    for args in [
        vec!["use", "proxy", "--dry-run"],
        vec!["doctor", "--json"],
        vec!["recover"],
    ] {
        let output = cli(&paths, &args);
        assert!(output.status.success(), "{:?}", output);
        assert!(output.stderr.is_empty(), "{:?}", output);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(!text.contains("active_instances"));
        assert!(!text.contains("PID"));
        assert!(!text.contains("重启"));
    }
    let output = cli(&paths, &["use", "missing"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("重启"));
}
