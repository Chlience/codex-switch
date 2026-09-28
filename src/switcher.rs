use crate::{
    config::{self, Credential, Preset},
    storage::{self, Change, Journal, Paths, Snapshot},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use toml_edit::DocumentMut;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    Auto,
    Symlink,
    Copy,
}

impl AuthMode {
    fn resolved(self) -> Self {
        match self {
            Self::Auto => {
                if cfg!(windows) {
                    Self::Copy
                } else {
                    Self::Symlink
                }
            }
            other => other,
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub name: Option<String>,
    pub auth_owner: Option<String>,
    pub auth_identity: Option<serde_json::Value>,
    pub auth_mode: AuthMode,
}

#[derive(Default)]
pub struct ImportOptions {
    pub name: Option<String>,
    pub from: Option<PathBuf>,
    pub profile: Option<String>,
    pub provider: Option<String>,
    pub auth_file: Option<PathBuf>,
    pub all: bool,
    pub dry_run: bool,
}

#[derive(Serialize)]
pub struct ImportResult {
    pub imported: Vec<String>,
    pub unchanged: Vec<String>,
    pub warnings: Vec<String>,
    pub dry_run: bool,
}

pub fn load_config(paths: &Paths) -> Result<DocumentMut> {
    match fs::read_to_string(paths.config()) {
        Ok(text) => config::parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !paths.config().is_symlink() => {
            Ok(DocumentMut::new())
        }
        Err(e) => Err(e).context("无法读取 config.toml"),
    }
}

pub fn load_preset(paths: &Paths, name: &str) -> Result<Preset> {
    config::validate_name(name)?;
    let preset: Preset = storage::read_json(&paths.preset(name))?;
    ensure!(
        preset.format_version == 1 && preset.name == name,
        "预设版本或名称无效"
    );
    Ok(preset)
}

pub fn list(paths: &Paths) -> Result<Vec<Preset>> {
    let dir = paths.store.join("providers");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .context("预设文件名无效")?;
            entries.push(load_preset(paths, name)?);
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

pub fn state(paths: &Paths) -> Result<State> {
    if paths.state().exists() {
        storage::read_json(&paths.state())
    } else {
        Ok(State::default())
    }
}

fn register_changes(paths: &Paths, preset: &Preset, auth: Option<&[u8]>) -> Result<Vec<Change>> {
    if paths.preset(&preset.name).exists() {
        let old = load_preset(paths, &preset.name)?;
        let equal = old.provider == preset.provider
            && old.credential == preset.credential
            && config::semantic(&config::parse(&old.config)?, &old.provider)?
                == config::semantic(&config::parse(&preset.config)?, &preset.provider)?;
        let equal_auth = match auth {
            Some(bytes) => fs::read(paths.credential(&preset.name)).is_ok_and(|old| old == bytes),
            None => true,
        };
        ensure!(
            equal && equal_auth,
            "同名预设已存在且内容不同；请使用新名称导入，已有记录未覆盖"
        );
        return Ok(Vec::new());
    }
    // Case-insensitive collision prevention keeps registries portable to Windows/macOS.
    ensure!(
        !list(paths)?
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(&preset.name)),
        "预设名称存在大小写冲突"
    );
    let mut changes = Vec::new();
    if let Some(auth) = auth {
        if paths.credential(&preset.name).exists() {
            ensure!(
                fs::read(paths.credential(&preset.name))? == auth,
                "同名凭据文件已存在且内容不同；请使用其他名称"
            );
        } else {
            changes.push(Change::new(
                paths.credential(&preset.name),
                Snapshot::file(auth.to_vec()),
            )?);
        }
    }
    changes.push(Change::new(
        paths.preset(&preset.name),
        Snapshot::file(serde_json::to_vec_pretty(preset)?),
    )?);
    Ok(changes)
}

pub fn import(paths: &Paths, options: ImportOptions) -> Result<ImportResult> {
    let _lock = if options.dry_run {
        None
    } else {
        Some(paths.lock()?)
    };
    paths.require_clean()?;
    let source = options.from.unwrap_or_else(|| paths.config());
    let source = std::path::absolute(source)?;
    let parent = source.parent().context("配置文件路径无效")?;
    let mut doc = config::parse(&fs::read_to_string(&source).context("无法读取导入配置")?)?;
    let profile = options.profile.or_else(|| {
        doc.get("profile")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    });
    if let Some(profile) = &profile {
        config::validate_name(profile)?;
        let profile_path = parent.join(format!("{profile}.config.toml"));
        ensure!(
            !(profile_path.exists() && doc.get("profiles").and_then(|p| p.get(profile)).is_some()),
            "同时存在同名新旧 profile，无法确定导入来源；请先消除歧义"
        );
        let layer = if profile_path.exists() {
            config::parse(&fs::read_to_string(profile_path).context("无法读取 profile 文件")?)?
        } else {
            config::legacy_profile(&doc, profile)?
        };
        config::merge_layer(&mut doc, &layer);
    }
    let active = config::provider_id(&doc)?.to_owned();
    let ids = if options.all {
        let mut ids = doc
            .get("model_providers")
            .and_then(|v| v.as_table_like())
            .map(|t| t.iter().map(|(k, _)| k.to_owned()).collect::<Vec<_>>())
            .unwrap_or_default();
        if !ids.contains(&active) {
            ids.push(active.clone());
        }
        ids
    } else {
        vec![options.provider.unwrap_or_else(|| active.clone())]
    };
    let mut result = ImportResult {
        imported: Vec::new(),
        unchanged: Vec::new(),
        warnings: Vec::new(),
        dry_run: options.dry_run,
    };
    let mut changes = Vec::new();
    let mut names = std::collections::HashSet::new();
    for id in ids {
        let name = if options.all {
            id.clone()
        } else {
            options.name.clone().context("请指定预设名称")?
        };
        config::validate_name(&name)?;
        ensure!(
            names.insert(name.to_ascii_lowercase()),
            "导入名称存在大小写冲突；请逐个指定名称导入"
        );
        let selected = config::selected_config(&doc, &id)?;
        // Import preserves malformed/ambiguous values for review; activation validates them.
        if let Err(error) = config::validate_provider(&selected, &id, false) {
            result.warnings.push(format!(
                "{name}: {error}；已保留原文，激活前需修正来源并重新导入"
            ));
        }
        let mut auth = None;
        let credential = if config::needs_native(&doc, &id) {
            let store = doc
                .get("cli_auth_credentials_store")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            ensure!(
                matches!(store, "file" | "keyring" | "auto" | "ephemeral"),
                "无法识别 cli_auth_credentials_store"
            );
            if let Some(explicit) = &options.auth_file {
                auth = Some(fs::read(explicit).context("无法读取指定认证文件")?);
                Credential::NativeFile
            } else if store != "file" {
                Credential::External {
                    store: store.to_owned(),
                }
            } else if id == active && parent.join("auth.json").exists() {
                auth = Some(fs::read(parent.join("auth.json"))?);
                Credential::NativeFile
            } else if config::has_provider_credential(&doc, &id) {
                Credential::Provider
            } else {
                Credential::MissingNative
            }
        } else {
            ensure!(
                options.auth_file.is_none(),
                "该 provider 未使用原生认证，不能自动绑定 auth.json"
            );
            Credential::Provider
        };
        if let Some(bytes) = &auth {
            config::validate_auth(bytes)?;
        }
        if matches!(credential, Credential::MissingNative) {
            result.warnings.push(format!(
                "{name}: 原生凭据待关联；可使用 --auth-file 显式导入"
            ));
        }
        let preset = Preset {
            format_version: 1,
            name: name.clone(),
            provider: id,
            config: selected.to_string(),
            credential,
            source: format!(
                "{}{}",
                source.display(),
                profile
                    .as_ref()
                    .map(|p| format!(" [profile {p}]"))
                    .unwrap_or_default()
            ),
        };
        let new_changes = register_changes(paths, &preset, auth.as_deref())?;
        if new_changes.is_empty() {
            result.unchanged.push(name);
        } else {
            result.imported.push(name);
            changes.extend(new_changes);
        }
    }
    if !options.dry_run {
        storage::execute(paths, Journal::new(changes, false))?;
    }
    Ok(result)
}

pub fn check_new_name(paths: &Paths, name: &str) -> Result<()> {
    config::validate_name(name)?;
    ensure!(
        !list(paths)?
            .iter()
            .any(|preset| preset.name.eq_ignore_ascii_case(name)),
        "预设名称已存在（不区分大小写），请使用其他名称"
    );
    Ok(())
}

pub fn add(paths: &Paths, mut preset: Preset, auth: Option<Vec<u8>>) -> Result<()> {
    config::validate_name(&preset.name)?;
    let doc = config::parse(&preset.config)?;
    ensure!(
        config::provider_id(&doc)? == preset.provider,
        "预设的 provider 与配置不一致"
    );
    let selected = config::selected_config(&doc, &preset.provider)?;
    config::validate_provider(&selected, &preset.provider, false)?;
    preset.config = selected.to_string();
    if let Some(bytes) = &auth {
        config::validate_auth(bytes)?;
    }
    let _lock = paths.lock()?;
    paths.require_clean()?;
    check_new_name(paths, &preset.name)?;
    let changes = register_changes(paths, &preset, auth.as_deref())?;
    storage::execute(paths, Journal::new(changes, false))
}

#[derive(Serialize)]
pub struct SwitchResult {
    pub name: String,
    pub changed_keys: Vec<String>,
    pub auth_changed: bool,
    pub dry_run: bool,
    pub warnings: Vec<String>,
}

pub fn switch(paths: &Paths, name: &str, mode: AuthMode, dry_run: bool) -> Result<SwitchResult> {
    let _lock = if dry_run { None } else { Some(paths.lock()?) };
    paths.require_clean()?;
    let preset = load_preset(paths, name)?;
    let selected = config::parse(&preset.config)?;
    let warnings = config::validate_provider(&selected, &preset.provider, true)?;
    ensure!(
        !matches!(preset.credential, Credential::MissingNative),
        "该预设缺少原生凭据；请用 --auth-file 重新导入到新名称"
    );
    // Keep the snapshot that produced the edit, so a later read cannot hide a concurrent edit.
    let config_path = paths.config_target()?;
    let config_before = Snapshot::read(&config_path)?;
    let current = match config_before.bytes() {
        Some(bytes) => {
            config::parse(std::str::from_utf8(bytes).context("config.toml 不是有效 UTF-8")?)?
        }
        None => DocumentMut::new(),
    };
    let next = config::apply(&current, &preset)?;
    let mut next_state = state(paths)?;
    next_state.name = Some(name.to_owned());
    let mut changes = Vec::new();
    let mut auth_changed = false;
    if matches!(preset.credential, Credential::NativeFile) {
        let target = paths.credential(name);
        let mut data = fs::read(&target).context("无法读取预设认证文件")?;
        let active = Snapshot::read(&paths.auth())?;
        if next_state.auth_owner.as_deref() == Some(name)
            && matches!(next_state.auth_mode, AuthMode::Copy)
        {
            let bytes = active
                .bytes()
                .context("当前认证文件已消失；请重新导入当前状态")?;
            let parsed = config::validate_auth(bytes)?;
            ensure!(
                next_state.auth_identity.is_some()
                    && config::identity(&parsed) == next_state.auth_identity,
                "当前登录身份已变化，拒绝回存到旧预设；请先导入当前认证"
            );
            if data != bytes {
                data = bytes.to_vec();
                changes.push(Change::new(target.clone(), Snapshot::file(data.clone()))?);
            }
        }
        let target_auth = config::validate_auth(&data)?;
        let resolved_mode = mode.resolved();
        let after = match resolved_mode {
            AuthMode::Symlink => Snapshot::Link {
                target: PathBuf::from("codex-sw")
                    .join("credentials")
                    .join(format!("auth.json.{name}")),
                data: Some(data.clone()),
            },
            _ => Snapshot::file(data.clone()),
        };
        auth_changed = active != after;
        if auth_changed {
            // Copy mode must save a refreshed credential before activating another record.
            if matches!(next_state.auth_mode, AuthMode::Copy)
                && let Some(owner) = &next_state.auth_owner
            {
                let bytes = active
                    .bytes()
                    .context("当前认证文件已消失，拒绝覆盖归档；请重新导入当前状态")?;
                let parsed = config::validate_auth(bytes)?;
                ensure!(
                    next_state.auth_identity.is_some()
                        && config::identity(&parsed) == next_state.auth_identity,
                    "当前登录身份已变化，拒绝将它回存到旧预设；请先导入当前认证"
                );
                if owner != name {
                    changes.push(Change::new(
                        paths.credential(owner),
                        Snapshot::file(bytes.to_vec()),
                    )?);
                } else {
                    // Re-selecting a copied record must never restore a stale refresh token.
                    ensure!(
                        bytes == data,
                        "当前认证已刷新；请切换到其他预设后再返回，以先保存最新凭据"
                    );
                }
            }
            changes.push(Change {
                path: paths.auth(),
                before: active,
                after,
            });
        }
        next_state.auth_owner = Some(name.to_owned());
        next_state.auth_identity = config::identity(&target_auth);
        next_state.auth_mode = resolved_mode;
    }
    ensure!(
        paths.config_target()? == config_path,
        "config.toml 软链目标已变化，请重试"
    );
    changes.push(Change {
        path: config_path,
        before: config_before,
        after: Snapshot::file(next.to_string().into_bytes()),
    });
    changes.push(Change::new(
        paths.state(),
        Snapshot::file(serde_json::to_vec_pretty(&next_state)?),
    )?);
    let result = SwitchResult {
        name: name.to_owned(),
        changed_keys: config::changed_keys(&current, &next, &preset.provider),
        auth_changed,
        dry_run,
        warnings,
    };
    if !dry_run {
        storage::execute(paths, Journal::new(changes, true))?;
    }
    Ok(result)
}

pub fn undo(paths: &Paths) -> Result<()> {
    let _lock = paths.lock()?;
    paths.require_clean()?;
    let previous: Journal = storage::read_json(&paths.undo()).context("没有可撤销的切换")?;
    let mut reverse = Vec::new();
    for change in previous.changes.iter().rev() {
        ensure!(
            Snapshot::read(&change.path)? == change.after,
            "切换后文件已有修改或凭据刷新；拒绝用旧备份覆盖，请重新导入当前状态"
        );
        reverse.push(Change {
            path: change.path.clone(),
            before: change.after.clone(),
            after: change.before.clone(),
        });
    }
    reverse.push(Change::new(paths.undo(), Snapshot::Missing)?);
    storage::execute(paths, Journal::new(reverse, false))
}

pub fn remove(paths: &Paths, name: &str) -> Result<()> {
    config::validate_name(name)?;
    let _lock = paths.lock()?;
    paths.require_clean()?;
    let status = state(paths)?;
    ensure!(
        status.name.as_deref() != Some(name) && status.auth_owner.as_deref() != Some(name),
        "该预设或凭据仍在使用，请先切换"
    );
    load_preset(paths, name)?;
    // Credentials remain recoverable in the private archive; remove only the registration.
    storage::execute(
        paths,
        Journal::new(
            vec![Change::new(paths.preset(name), Snapshot::Missing)?],
            false,
        ),
    )
}

pub fn current(paths: &Paths) -> Result<serde_json::Value> {
    let doc = load_config(paths)?;
    let id = config::provider_id(&doc)?;
    let state = state(paths)?;
    let matched = state
        .name
        .as_ref()
        .and_then(|name| load_preset(paths, name).ok())
        .filter(|p| {
            let same_config = match (
                config::parse(&p.config).and_then(|d| config::semantic(&d, &p.provider)),
                config::semantic(&doc, id),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
            let store = doc
                .get("cli_auth_credentials_store")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            let same_auth = match &p.credential {
                Credential::NativeFile => {
                    let actual = fs::read(paths.auth())
                        .ok()
                        .and_then(|b| config::validate_auth(&b).ok())
                        .and_then(|v| config::identity(&v));
                    store == "file"
                        && state.auth_owner.as_deref() == Some(&p.name)
                        && actual.is_some()
                        && actual == state.auth_identity
                }
                Credential::External { store: expected } => expected == store,
                _ => true,
            };
            same_config && same_auth
        })
        .map(|p| p.name);
    Ok(
        serde_json::json!({"scope": "用户默认配置（启动参数、profile、项目或管理配置可能覆盖）", "config_path": paths.config(), "name": matched, "provider": id,
        "model": doc.get("model").and_then(|v| v.as_str()), "auth_sources": config::auth_sources(&doc, id), "auth_is_symlink": paths.auth().is_symlink(), "auth_exists": paths.auth().exists(), "pending_transaction": paths.pending().exists()}),
    )
}

pub fn doctor(paths: &Paths) -> Result<serde_json::Value> {
    let doc = load_config(paths)?;
    let id = config::provider_id(&doc)?;
    let mut issues = Vec::new();
    let mut warnings = Vec::new();
    match config::validate_provider(&doc, id, true) {
        Ok(w) => warnings.extend(w),
        Err(e) => issues.push(e.to_string()),
    }
    if config::needs_native(&doc, id) {
        let store = doc
            .get("cli_auth_credentials_store")
            .and_then(|v| v.as_str())
            .unwrap_or("file");
        if store == "file" {
            match fs::read(paths.auth())
                .context("原生认证文件不存在或无法读取")
                .and_then(|b| config::validate_auth(&b))
            {
                Ok(_) => {}
                Err(e) => {
                    if config::has_provider_credential(&doc, id) {
                        warnings.push(e.to_string());
                    } else {
                        issues.push(e.to_string());
                    }
                }
            }
        } else {
            warnings.push(format!("凭据由外部存储 {store} 管理，未验证其可用性"));
        }
    }
    if doc.get("profile").is_some() {
        issues.push("仍有旧版 profile 选择器，当前 Codex 不支持".into());
    }
    if doc
        .get("models")
        .and_then(|v| v.get("new_thread"))
        .is_some()
    {
        warnings.push("models.new_thread 可能覆盖默认模型，请检查新会话实际选择".into());
    }
    if paths.pending().exists() {
        issues.push("存在未完成事务，请运行 recover".into());
    }
    for key in [
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
        "OPENAI_FEDERATION_RULE_ID",
        "OPENAI_IDENTITY_TOKEN_FILE",
    ] {
        if std::env::var_os(key).is_some() {
            warnings.push(format!("检测到 {key}，可能影响原生认证选择（不显示值）"));
        }
    }
    let version = std::process::Command::new("codex")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    if version.is_none() {
        warnings.push("PATH 中未找到可运行的 Codex".into());
    }
    Ok(
        serde_json::json!({"ok":issues.is_empty(), "config_path":paths.config(), "codex_version":version, "issues":issues, "warnings":warnings, "validation":"本地检查；未执行认证命令或发送模型请求"}),
    )
}

pub fn recover(paths: &Paths) -> Result<bool> {
    let _lock = paths.lock()?;
    storage::recover(paths)
}
