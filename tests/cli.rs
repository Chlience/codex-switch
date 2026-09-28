// Keep subprocess checks separate from tests holding file locks. On Unix, a fork
// can inherit another test thread's lock until the child reaches exec.
use codex_sw::{config, storage::Paths, switcher};
use serde_json::json;
use std::{
    fs,
    process::{Child, Command, Output, Stdio},
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
        "预设名称  Provider\n────────  ────────\nwork      work\n"
    );
    let listed_json = run(&["list", "--json"]);
    assert!(listed_json.status.success());
    let records: serde_json::Value = serde_json::from_slice(&listed_json.stdout).unwrap();
    assert_eq!(records[0]["name"], "work");
    assert_eq!(records[0]["provider"], "work");
    assert!(records[0].get("model").is_none());
    let added = run(&[
        "add",
        "work-longer",
        "--provider",
        "示例",
        "--base-url",
        "https://provider.example/v1",
    ]);
    assert!(added.status.success());
    let listed = run(&["list"]);
    assert!(listed.status.success());
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        "预设名称     Provider\n───────────  ────────\nwork         work\nwork-longer  示例\n"
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
}

#[test]
fn auth_command_is_not_executed_during_import_or_doctor() {
    let (_temp, paths) = setup(
        "model_provider='proxy'\n[model_providers.proxy]\nname='Proxy'\n[model_providers.proxy.auth]\ncommand='codex-sw-this-command-must-not-run'\n",
    );
    assert!(cli(&paths, &["import", "saved"]).status.success());
    let output = cli(&paths, &["doctor", "--json"]);
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], true);
}

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn cli_warns_for_real_live_process_and_still_switches_credentials() {
    let (temp, paths) = setup(native_config());
    fs::write(paths.auth(), native_auth("dummy-a")).unwrap();
    assert!(cli(&paths, &["import", "a"]).status.success());
    fs::write(paths.auth(), native_auth("dummy-b")).unwrap();
    let fake = temp
        .path()
        .join(if cfg!(windows) { "codex.exe" } else { "codex" });
    fs::copy(env!("CARGO_BIN_EXE_codex-sw"), &fake).unwrap();
    // Our own copied binary waits on stdin; no actual Codex session is started.
    let _running = Running(
        Command::new(&fake)
            .env("CODEX_HOME", &paths.home)
            .args([
                "add",
                "waiting",
                "--base-url",
                "https://example.invalid",
                "--bearer-token-stdin",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_codex-sw"))
        .arg("--codex-home")
        .arg(&paths.home)
        .args(["--json", "use", "a", "--auth-mode", "copy"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("活跃 Codex 实例"), "{text}");
    assert!(text.contains("重启"));
    assert_eq!(fs::read(paths.auth()).unwrap(), native_auth("dummy-a"));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        !json["active_instances"]["instances"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
