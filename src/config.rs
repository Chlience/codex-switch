use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use toml_edit::{DocumentMut, Item, Table, value};

// Only provider routing settings belong to a preset. Legacy model fields are ignored.
pub const SETTINGS: &[&str] = &["model_provider", "openai_base_url"];

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    Provider,
    NativeFile,
    External { store: String },
    MissingNative,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Preset {
    pub format_version: u32,
    // Version-1 storage key, retained so existing credential links and journals stay valid.
    // User-facing commands select `provider`, never this internal key.
    pub name: String,
    pub provider: String,
    pub config: String,
    pub credential: Credential,
    pub source: String,
}

pub fn validate_provider_id(id: &str) -> Result<()> {
    ensure!(
        !id.trim().is_empty() && !id.chars().any(char::is_control),
        "Provider ID 不能为空或包含控制字符"
    );
    Ok(())
}

pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "名称只能包含 ASCII 字母、数字、横线、下划线，长度为 1–64"
    );
    let upper = name.to_ascii_uppercase();
    ensure!(
        ![
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"
        ]
        .contains(&upper.as_str()),
        "名称与 Windows 保留文件名冲突"
    );
    Ok(())
}

pub fn parse(text: &str) -> Result<DocumentMut> {
    // Parser diagnostics may include a complete source line containing a secret.
    text.parse()
        .map_err(|_| anyhow::anyhow!("TOML 配置无效；请检查语法（为保护凭据，不显示原文）"))
}

pub fn provider_id(doc: &DocumentMut) -> Result<&str> {
    match doc.get("model_provider") {
        None => Ok("openai"),
        Some(item) => item.as_str().context("model_provider 必须是字符串"),
    }
}

pub fn definition<'a>(doc: &'a DocumentMut, id: &str) -> Option<&'a Item> {
    doc.get("model_providers")?.get(id)
}

pub fn builtin(id: &str) -> bool {
    matches!(id, "openai" | "ollama" | "lmstudio" | "amazon-bedrock")
}

pub fn needs_native(doc: &DocumentMut, id: &str) -> bool {
    id == "openai"
        || definition(doc, id)
            .and_then(|d| d.get("requires_openai_auth"))
            .and_then(Item::as_bool)
            == Some(true)
}

pub fn has_provider_credential(doc: &DocumentMut, id: &str) -> bool {
    definition(doc, id).is_some_and(|d| {
        [
            "env_key",
            "experimental_bearer_token",
            "auth",
            "http_headers",
            "env_http_headers",
        ]
        .iter()
        .any(|k| d.get(k).is_some())
    })
}

pub fn selected_config(doc: &DocumentMut, id: &str) -> Result<DocumentMut> {
    let mut selected = DocumentMut::new();
    for key in SETTINGS {
        if let Some(item) = doc.get(key) {
            selected[key] = item.clone();
        }
    }
    selected["model_provider"] = value(id);
    if let Some(def) = definition(doc, id) {
        ensure!(def.is_table_like(), "provider 定义必须是 TOML 表");
        let mut providers = Table::new();
        providers.insert(id, def.clone());
        selected["model_providers"] = Item::Table(providers);
    } else {
        ensure!(builtin(id), "找不到指定的 provider 定义");
    }
    Ok(selected)
}

pub fn merge_layer(base: &mut DocumentMut, layer: &DocumentMut) {
    fn merge(to: &mut Table, from: &Table) {
        for (key, val) in from {
            if let (Some(existing), Some(incoming)) =
                (to.get_mut(key).and_then(Item::as_table_mut), val.as_table())
            {
                merge(existing, incoming);
            } else {
                to.insert(key, val.clone());
            }
        }
    }
    merge(base.as_table_mut(), layer.as_table());
}

pub fn legacy_profile(doc: &DocumentMut, name: &str) -> Result<DocumentMut> {
    let table = doc
        .get("profiles")
        .and_then(|p| p.get(name))
        .and_then(Item::as_table)
        .context("找不到指定 profile")?;
    let mut result = DocumentMut::new();
    *result.as_table_mut() = table.clone();
    Ok(result)
}

fn expand_inline_table(key: &mut toml_edit::KeyMut<'_>, item: &mut Item) {
    if let Some(inline) = item.as_inline_table() {
        let prefix = key.leaf_decor().prefix().cloned().unwrap_or_default();
        let suffix = inline.decor().suffix().cloned().unwrap_or_default();
        let mut table = inline.clone().into_table();
        table.decor_mut().set_prefix(prefix);
        table.decor_mut().set_suffix(suffix);
        key.leaf_decor_mut().clear();
        *item = Item::Table(table);
    }
}

fn format_providers(doc: &mut DocumentMut, managed_id: Option<&str>) -> Result<()> {
    let Some((mut key, item)) = doc.as_table_mut().get_key_value_mut("model_providers") else {
        return Ok(());
    };
    expand_inline_table(&mut key, item);
    let providers = item
        .as_table_mut()
        .context("model_providers 必须是 TOML 表")?;
    providers.set_dotted(false);
    for (mut id, item) in providers.iter_mut() {
        expand_inline_table(&mut id, item);
        let Some(table) = item.as_table_mut() else {
            continue;
        };
        table.set_dotted(false);
        table.set_implicit(false);
        let prefix = table
            .decor()
            .prefix()
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let mut prefix = prefix.trim_start_matches(['\r', '\n']).to_owned();
        if managed_id.is_some_and(|selected| id == selected) {
            // Replace only our own marker; keep the provider's other comments.
            prefix = prefix
                .split_inclusive('\n')
                .filter(|line| {
                    line.trim() != "# Managed by codex-sw"
                        && !line
                            .trim()
                            .strip_prefix("# Managed by codex-sw (preset: ")
                            .and_then(|s| s.strip_suffix(')'))
                            .is_some_and(|s| validate_name(s).is_ok())
                })
                .collect();
            prefix = format!("# Managed by codex-sw\n{prefix}");
        }
        table.decor_mut().set_prefix(format!("\n{prefix}"));
    }
    Ok(())
}

pub fn apply(current: &DocumentMut, preset: &Preset) -> Result<DocumentMut> {
    ensure!(preset.format_version == 1, "不支持的预设版本");
    ensure!(
        current.get("profile").is_none(),
        "当前 config.toml 仍有旧 profile 选择器；请先迁移到独立 profile 文件"
    );
    let selected = parse(&preset.config)?;
    ensure!(
        provider_id(&selected)? == preset.provider,
        "预设的 provider 与配置不一致"
    );
    let mut next = current.clone();
    for key in SETTINGS {
        if let Some(item) = selected.get(key) {
            if let Some(old) = next.get(key).and_then(Item::as_value) {
                let mut replacement = item.clone();
                if let Some(val) = replacement.as_value_mut() {
                    *val.decor_mut() = old.decor().clone();
                }
                next[key] = replacement;
            } else {
                next[key] = item.clone();
            }
        } else {
            next.remove(key);
        }
    }
    if let Some(def) = definition(&selected, &preset.provider) {
        if next.get("model_providers").is_none() {
            next["model_providers"] = Item::Table(Table::new());
        }
        let providers = next
            .get_mut("model_providers")
            .and_then(Item::as_table_like_mut)
            .context("当前 model_providers 必须是 TOML 表")?;
        let mut replacement = def.clone();
        if let Some(old) = providers.get(&preset.provider).and_then(Item::as_table) {
            let mut table = replacement
                .into_table()
                .map_err(|_| anyhow::anyhow!("provider 定义必须是 TOML 表"))?;
            *table.decor_mut() = old.decor().clone();
            table.set_position(old.position());
            replacement = Item::Table(table);
        }
        if let Some(existing) = providers.get_mut(&preset.provider) {
            *existing = replacement;
        } else {
            providers.insert(&preset.provider, replacement);
        }
    }
    format_providers(
        &mut next,
        definition(&selected, &preset.provider).map(|_| preset.provider.as_str()),
    )?;
    match &preset.credential {
        Credential::NativeFile => next["cli_auth_credentials_store"] = value("file"),
        Credential::External { store } => {
            next["cli_auth_credentials_store"] = value(store.as_str())
        }
        _ => {}
    }
    Ok(next)
}

pub fn auth_sources(doc: &DocumentMut, id: &str) -> Vec<String> {
    let mut sources = Vec::new();
    if let Some(def) = definition(doc, id) {
        for (key, label) in [
            ("env_key", "env_key"),
            ("experimental_bearer_token", "直接 token"),
            ("auth", "认证命令"),
            ("http_headers", "固定请求头"),
            ("env_http_headers", "环境请求头"),
        ] {
            if def.get(key).is_some() {
                sources.push(label.to_owned());
            }
        }
    }
    if needs_native(doc, id) {
        sources.push("Codex 原生认证".to_owned());
    }
    if sources.is_empty() {
        sources.push("未指定认证".to_owned());
    }
    sources
}

pub fn validate_provider(doc: &DocumentMut, id: &str, check_env: bool) -> Result<Vec<String>> {
    validate_provider_id(id)?;
    let mut warnings = Vec::new();
    for key in SETTINGS {
        if let Some(item) = doc.get(key) {
            ensure!(
                item.as_str().is_some_and(|s| !s.trim().is_empty()),
                "{key} 必须是非空字符串"
            );
        }
    }
    let Some(def) = definition(doc, id) else {
        ensure!(builtin(id), "找不到 provider 定义");
        return Ok(warnings);
    };
    ensure!(def.is_table_like(), "provider 定义必须是 TOML 表");
    ensure!(
        !matches!(id, "openai" | "ollama" | "lmstudio"),
        "当前 Codex 不允许覆盖内置 provider 定义"
    );
    for key in [
        "name",
        "base_url",
        "env_key",
        "experimental_bearer_token",
        "wire_api",
    ] {
        if let Some(item) = def.get(key) {
            let text = item
                .as_str()
                .with_context(|| format!("{key} 必须是字符串"))?;
            ensure!(!text.trim().is_empty(), "{key} 不能为空");
        }
    }
    if let Some(url) = def.get("base_url").and_then(Item::as_str) {
        ensure!(
            url.starts_with("http://") || url.starts_with("https://"),
            "base_url 必须使用 http:// 或 https://"
        );
    }
    if let Some(wire) = def.get("wire_api").and_then(Item::as_str) {
        ensure!(wire == "responses", "本版本只支持 wire_api = responses");
    }
    if let Some(item) = def.get("requires_openai_auth") {
        ensure!(
            item.as_bool().is_some(),
            "requires_openai_auth 必须是布尔值"
        );
    }
    if def.get("auth").is_some() {
        ensure!(
            def.get("env_key").is_none()
                && def.get("experimental_bearer_token").is_none()
                && !needs_native(doc, id),
            "认证命令不能与其他 token 来源组合使用"
        );
        ensure!(
            def.get("auth")
                .and_then(|a| a.get("command"))
                .and_then(Item::as_str)
                .is_some_and(|s| !s.trim().is_empty()),
            "认证命令缺少 command"
        );
    }
    if let Some(env) = def.get("env_key").and_then(Item::as_str) {
        if check_env {
            ensure!(
                std::env::var(env).is_ok_and(|v| !v.trim().is_empty()),
                "provider 所需环境变量缺失或为空：{env}"
            );
        }
        if def.get("experimental_bearer_token").is_some() || needs_native(doc, id) {
            warnings.push("配置含多个凭据来源；0.156.1 实测 env_key 优先，其他版本需验证".into());
        }
    }
    if let Some(headers) = def.get("env_http_headers").and_then(Item::as_table_like) {
        for (_, env) in headers.iter() {
            let env = env.as_str().context("环境请求头引用必须是字符串")?;
            if check_env && std::env::var(env).map_or(true, |v| v.trim().is_empty()) {
                warnings.push(format!("请求头环境变量缺失或为空：{env}"));
            }
        }
    }
    Ok(warnings)
}

pub fn validate_auth(bytes: &[u8]) -> Result<serde_json::Value> {
    let data: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("认证文件不是有效 JSON（不显示原文）"))?;
    ensure!(data.is_object(), "认证文件必须是 JSON 对象");
    if let Some(mode) = data.get("auth_mode") {
        ensure!(mode.is_string(), "auth_mode 必须是字符串");
    }
    let mode = data.get("auth_mode").and_then(|v| v.as_str());
    match mode {
        Some("apikey") => ensure!(
            data.get("OPENAI_API_KEY")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty()),
            "认证文件的 API key 缺失或为空"
        ),
        Some("chatgpt") => ensure!(
            data.get("tokens")
                .and_then(|v| v.get("access_token"))
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty()),
            "认证文件的 OAuth access_token 缺失或为空"
        ),
        None => {
            let key = data.get("OPENAI_API_KEY").and_then(|v| v.as_str());
            if let Some(key) = key {
                ensure!(!key.trim().is_empty(), "认证文件的 API key 为空");
            } else {
                ensure!(
                    data.get("tokens")
                        .and_then(|v| v.get("access_token"))
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| !s.trim().is_empty()),
                    "无法识别认证文件；缺少有效的 API key 或 OAuth access_token"
                );
            }
        }
        Some(_) => bail!("暂不支持该原生认证模式；请保留外部认证方式"),
    }
    Ok(data)
}

pub fn identity(data: &serde_json::Value) -> Option<serde_json::Value> {
    if data.get("auth_mode").and_then(|v| v.as_str()) != Some("chatgpt")
        && let Some(key) = data.get("OPENAI_API_KEY").and_then(|v| v.as_str())
    {
        return Some(serde_json::json!({"key": key}));
    }
    data.get("tokens")?
        .get("account_id")
        .filter(|v| v.is_string())
        .cloned()
}

pub fn semantic(doc: &DocumentMut, id: &str) -> Result<serde_json::Value> {
    // Parsing through toml_edit's value tree avoids formatting-dependent matching.
    fn val(item: &Item) -> serde_json::Value {
        if let Some(table) = item.as_table_like() {
            return serde_json::Value::Object(
                table.iter().map(|(k, v)| (k.to_owned(), val(v))).collect(),
            );
        }
        if let Some(a) = item.as_array() {
            return serde_json::Value::Array(
                a.iter().map(|v| val(&Item::Value(v.clone()))).collect(),
            );
        }
        if let Some(s) = item.as_str() {
            return s.into();
        }
        if let Some(b) = item.as_bool() {
            return b.into();
        }
        if let Some(i) = item.as_integer() {
            return i.into();
        }
        if let Some(f) = item.as_float() {
            return serde_json::json!(f);
        }
        item.to_string().into()
    }
    let selected = selected_config(doc, id)?;
    Ok(val(&Item::Table(selected.as_table().clone())))
}

pub fn changed_keys(before: &DocumentMut, after: &DocumentMut, id: &str) -> Vec<String> {
    let mut changes = Vec::new();
    for key in SETTINGS
        .iter()
        .copied()
        .chain(["cli_auth_credentials_store"])
    {
        if before.get(key).map(ToString::to_string) != after.get(key).map(ToString::to_string) {
            changes.push(key.to_owned());
        }
    }
    if definition(before, id).map(ToString::to_string)
        != definition(after, id).map(ToString::to_string)
    {
        changes.push("model_providers.<selected>".to_owned());
    }
    changes
}

pub fn summary(preset: &Preset) -> Result<BTreeMap<String, serde_json::Value>> {
    validate_provider_id(&preset.provider)?;
    let doc = parse(&preset.config)?;
    let endpoint = definition(&doc, &preset.provider)
        .and_then(|def| def.get("base_url"))
        .and_then(Item::as_str)
        .or_else(|| {
            (preset.provider == "openai")
                .then(|| doc.get("openai_base_url").and_then(Item::as_str))
                .flatten()
        })
        .map(endpoint_summary);
    Ok(BTreeMap::from([
        ("provider".into(), preset.provider.clone().into()),
        ("endpoint".into(), endpoint.into()),
        (
            "auth_sources".into(),
            serde_json::json!(auth_sources(&doc, &preset.provider)),
        ),
        (
            "credential".into(),
            serde_json::to_value(&preset.credential)?,
        ),
    ]))
}

fn endpoint_summary(url: &str) -> String {
    // User information, queries and fragments can contain credentials.
    let (base, suffix) = url
        .split_once(['?', '#'])
        .map_or((url, ""), |(base, _)| (base, " [参数已隐藏]"));
    let base = if let Some((scheme, rest)) = base.split_once("://") {
        let (authority, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
        let authority = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        format!("{scheme}://{authority}{path}")
    } else {
        base.to_owned()
    };
    let mut safe = String::with_capacity(base.len() + suffix.len());
    for c in base.chars() {
        if c.is_control() {
            safe.extend(c.escape_default());
        } else {
            safe.push(c);
        }
    }
    safe.push_str(suffix);
    safe
}
