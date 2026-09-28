use anyhow::{Context, Result, ensure};
use codex_sw::{
    config::{self, Credential, Preset},
    storage::Paths,
    switcher,
};
use std::io::{BufRead, Read, Write};
use toml_edit::{DocumentMut, value};

const INPUT_LIMIT: usize = 1024 * 1024;

fn line(input: &mut impl BufRead, limit: usize) -> Result<String> {
    let mut text = String::new();
    let count = input
        .take(limit as u64 + 1)
        .read_line(&mut text)
        .context("无法读取输入")?;
    ensure!(count != 0, "输入已结束，未保存预设");
    ensure!(text.len() <= limit, "输入超过大小限制，未保存预设");
    Ok(text)
}

fn block(input: &mut impl BufRead) -> Result<String> {
    let mut text = String::new();
    loop {
        let next = line(input, INPUT_LIMIT + 5)?;
        if next.trim_end_matches(['\r', '\n']) == "END" {
            return Ok(text);
        }
        ensure!(
            text.len() + next.len() <= INPUT_LIMIT,
            "输入超过大小限制，未保存预设"
        );
        text.push_str(&next);
    }
}

fn provider(text: &str) -> Result<(String, String, DocumentMut)> {
    let doc = config::parse(text)?;
    let id = if doc.get("model_provider").is_some() {
        config::provider_id(&doc)?.to_owned()
    } else {
        let definitions = doc
            .get("model_providers")
            .and_then(|v| v.as_table_like())
            .context("请粘贴 model_provider 或 [model_providers.<id>] 配置")?;
        ensure!(
            definitions.len() == 1,
            "配置含多个 provider；请用 model_provider 指定一个"
        );
        definitions.iter().next().unwrap().0.to_owned()
    };
    let mut selected = config::selected_config(&doc, &id)?;
    let name = config::definition(&selected, &id)
        .and_then(|definition| definition.get("name"))
        .and_then(|name| name.as_str())
        .context("所选 provider 缺少字符串字段 name")?
        .to_owned();
    config::validate_name(&name)?;
    if config::builtin(&id) {
        ensure!(id == "openai", "该内置 provider 不支持 auth.json 认证");
    } else {
        let definition = &mut selected["model_providers"][&id];
        for key in ["env_key", "experimental_bearer_token", "auth"] {
            ensure!(
                definition.get(key).is_none(),
                "provider 包含 {key}；使用 auth.json 时请移除该认证字段后重新粘贴"
            );
        }
        definition["requires_openai_auth"] = value(true);
    }
    config::validate_provider(&selected, &id, false)?;
    Ok((name, id, selected))
}

pub fn add(paths: &Paths, input: &mut impl BufRead, prompts: &mut impl Write) -> Result<String> {
    paths.require_clean()?;
    writeln!(
        prompts,
        "添加预设：provider 配置 → auth.json；预设名称取自 provider 的 name。"
    )?;
    let (name, id, selected) = loop {
        writeln!(
            prompts,
            "粘贴 provider 配置（TOML），单独输入一行 END 结束："
        )?;
        prompts.flush()?;
        let provider = provider(&block(input)?).and_then(|provider| {
            switcher::check_new_name(paths, &provider.0)?;
            Ok(provider)
        });
        match provider {
            Ok(provider) => break provider,
            Err(error) => writeln!(prompts, "{error}；请重新粘贴 provider 配置。")?,
        }
    };
    let auth = loop {
        writeln!(prompts, "粘贴 auth.json 内容，单独输入一行 END 结束：")?;
        prompts.flush()?;
        let bytes = block(input)?.into_bytes();
        match config::validate_auth(&bytes) {
            Ok(_) => break bytes,
            Err(error) => writeln!(prompts, "{error}；请重新粘贴 auth.json。")?,
        }
    };
    switcher::add(
        paths,
        Preset {
            format_version: 1,
            name: name.clone(),
            provider: id,
            config: selected.to_string(),
            credential: Credential::NativeFile,
            source: "interactive add".into(),
        },
        Some(auth),
    )?;
    Ok(name)
}
