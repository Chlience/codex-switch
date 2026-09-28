use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use codex_sw::{
    config::{self, Credential, Preset},
    storage::Paths,
    switcher::{self, AuthMode, ImportOptions},
};
use std::{io::Read, path::PathBuf};
use toml_edit::{DocumentMut, Item, Table, value};

mod interactive;

const RESTART_NOTICE: &str = "提示：已有 Codex 实例需要重启后使用新配置。";

#[derive(Parser)]
#[command(
    name = "codex-sw",
    version,
    about = "跨平台 Codex provider 切换器",
    after_help = "凭据保存在本机受保护的文件中。切换后，已有 Codex 实例需要重启后使用新配置。"
)]
struct Cli {
    /// Codex 配置目录；默认读取 CODEX_HOME 或 ~/.codex
    #[arg(long, global = true)]
    codex_home: Option<PathBuf>,
    /// 输出 JSON；警告写入 stderr
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 从已有 config.toml 或 profile 导入，保持原认证方式
    Import {
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        name: Option<String>,
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(long, conflicts_with = "all")]
        profile: Option<String>,
        #[arg(long, conflicts_with = "all")]
        provider: Option<String>,
        /// 显式关联原生 auth.json；不会自动分配给所有 provider
        #[arg(long, conflicts_with = "all")]
        auth_file: Option<PathBuf>,
        /// 导入配置中的所有 provider；仅当前 provider 自动关联原生凭据
        #[arg(long)]
        all: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// 添加 provider 预设；不带参数时逐步粘贴配置和 auth.json
    Add {
        name: Option<String>,
        #[arg(long, requires = "name")]
        provider: Option<String>,
        #[arg(long, requires = "name")]
        base_url: Option<String>,
        #[arg(long, requires = "name", conflicts_with_all = ["bearer_token_stdin", "auth_file"])]
        env_key: Option<String>,
        #[arg(long, requires = "name", conflicts_with = "auth_file")]
        bearer_token_stdin: bool,
        #[arg(long, requires = "name")]
        auth_file: Option<PathBuf>,
    },
    /// 显示预设名称和 provider 表格，不显示凭据
    List,
    /// 切换默认 provider，并提醒已有实例重启
    Use {
        name: String,
        #[arg(long, value_enum, default_value = "auto")]
        auth_mode: AuthMode,
        #[arg(long)]
        dry_run: bool,
    },
    /// 显示用户配置中的默认 provider
    Current,
    /// 检查配置和凭据引用，不发起模型请求
    Doctor,
    /// 撤销最近一次切换；外部修改或刷新后拒绝覆盖
    Undo,
    /// 恢复中断的切换事务；保留存在外部修改的文件
    Recover,
    /// 移除未使用的预设登记；凭据及历史备份保留
    Remove { name: String },
}

fn output(json: bool, value: serde_json::Value, message: impl AsRef<str>) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("{}", message.as_ref());
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    let paths = Paths::new(cli.codex_home)?;
    match cli.command {
        Commands::Import {
            name,
            from,
            profile,
            provider,
            auth_file,
            all,
            dry_run,
        } => {
            let result = switcher::import(
                &paths,
                ImportOptions {
                    name,
                    from,
                    profile,
                    provider,
                    auth_file,
                    all,
                    dry_run,
                },
            )?;
            for warning in &result.warnings {
                eprintln!("警告：{warning}");
            }
            let text = format!(
                "{} {} 个预设；{} 个已有相同记录。{}",
                if dry_run { "预计导入" } else { "已导入" },
                result.imported.len(),
                result.unchanged.len(),
                if result.imported.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", result.imported.join("\n"))
                }
            );
            output(cli.json, serde_json::to_value(result)?, text)
        }
        Commands::Add {
            name,
            provider,
            base_url,
            env_key,
            bearer_token_stdin,
            auth_file,
        } => {
            let Some(name) = name else {
                let name = interactive::add(
                    &paths,
                    &mut std::io::stdin().lock(),
                    &mut std::io::stderr().lock(),
                )?;
                return output(
                    cli.json,
                    serde_json::json!({"added":name}),
                    format!("已保存预设 {name}。使用 codex-sw use {name} 激活。"),
                );
            };
            config::validate_name(&name)?;
            let id = provider.unwrap_or_else(|| name.clone());
            ensure!(!id.trim().is_empty(), "provider ID 不能为空");
            let mut doc = DocumentMut::new();
            doc["model_provider"] = value(&id);
            let mut auth = None;
            let credential;
            if config::builtin(&id) {
                ensure!(
                    base_url.is_none() && env_key.is_none() && !bearer_token_stdin,
                    "内置 provider 不接受自定义地址或 token；请使用自定义 provider ID"
                );
                credential = if let Some(file) = auth_file {
                    ensure!(id == "openai", "该内置 provider 的原生认证暂不支持");
                    auth = Some(std::fs::read(file).context("无法读取认证文件")?);
                    Credential::NativeFile
                } else if id == "openai" {
                    Credential::External {
                        store: "file".into(),
                    }
                } else {
                    Credential::Provider
                };
            } else {
                let mut def = Table::new();
                def["name"] = value(&name);
                def["base_url"] = value(base_url.context("自定义 provider 需要 --base-url")?);
                def["wire_api"] = value("responses");
                if let Some(key) = env_key {
                    def["env_key"] = value(key);
                }
                if bearer_token_stdin {
                    let mut token = String::new();
                    std::io::stdin()
                        .take(1024 * 1024 + 1)
                        .read_to_string(&mut token)?;
                    ensure!(
                        token.len() <= 1024 * 1024,
                        "输入 token 超过大小限制，未保存截断内容"
                    );
                    let token = token.trim_end_matches(['\r', '\n']);
                    ensure!(!token.trim().is_empty(), "输入 token 不能为空");
                    def["experimental_bearer_token"] = value(token);
                }
                credential = if let Some(file) = auth_file {
                    auth = Some(std::fs::read(file).context("无法读取认证文件")?);
                    def["requires_openai_auth"] = value(true);
                    Credential::NativeFile
                } else {
                    Credential::Provider
                };
                let mut providers = Table::new();
                providers[&id] = Item::Table(def);
                doc["model_providers"] = Item::Table(providers);
            }
            let preset = Preset {
                format_version: 1,
                name: name.clone(),
                provider: id,
                config: doc.to_string(),
                credential,
                source: "add".into(),
            };
            switcher::add(&paths, preset, auth)?;
            output(
                cli.json,
                serde_json::json!({"added":name}),
                format!("已保存预设 {name}。"),
            )
        }
        Commands::List => {
            let presets = switcher::list(&paths)?;
            let summaries = presets
                .iter()
                .map(config::summary)
                .collect::<Result<Vec<_>>>()?;
            let text = if presets.is_empty() {
                "尚无预设；使用 import 或 add 添加。".to_owned()
            } else {
                // Preset names are ASCII; the Chinese heading occupies eight columns.
                let width = presets.iter().map(|p| p.name.len()).max().unwrap().max(8);
                let mut lines = vec![
                    format!("预设名称{}  Provider", " ".repeat(width - 8)),
                    format!("{}  ────────", "─".repeat(width)),
                ];
                lines.extend(
                    presets
                        .iter()
                        .map(|p| format!("{:<width$}  {}", p.name, p.provider)),
                );
                lines.join("\n")
            };
            output(cli.json, serde_json::to_value(summaries)?, text)
        }
        Commands::Use {
            name,
            auth_mode,
            dry_run,
        } => {
            let result = switcher::switch(&paths, &name, auth_mode, dry_run)?;
            for warning in &result.warnings {
                eprintln!("警告：{warning}");
            }
            if !dry_run {
                eprintln!("{RESTART_NOTICE}");
            }
            let text = if dry_run {
                format!(
                    "预览切换到 {name}：\n配置字段：{}\n更换 auth.json：{}\n未写入文件。",
                    result.changed_keys.join(", "),
                    result.auth_changed
                )
            } else {
                format!("已切换到 {name}。")
            };
            output(cli.json, serde_json::to_value(result)?, text)
        }
        Commands::Current => {
            let data = switcher::current(&paths)?;
            let text = format!(
                "预设：{}\n默认 provider：{}\n默认模型：{}\n配置：{}\n{}",
                data["name"].as_str().unwrap_or("未匹配已保存预设"),
                data["provider"].as_str().unwrap_or_default(),
                data["model"].as_str().unwrap_or("Codex 默认"),
                paths.config().display(),
                data["scope"].as_str().unwrap_or_default()
            );
            output(cli.json, data, text)
        }
        Commands::Doctor => {
            let data = switcher::doctor(&paths)?;
            let text = serde_json::to_string_pretty(&data)?;
            let ok = data["ok"].as_bool() == Some(true);
            output(cli.json, data, text)?;
            ensure!(ok, "本地诊断发现问题");
            Ok(())
        }
        Commands::Undo => {
            switcher::undo(&paths)?;
            eprintln!("{RESTART_NOTICE}");
            output(
                cli.json,
                serde_json::json!({"undone":true}),
                "已撤销最近一次切换。",
            )
        }
        Commands::Recover => {
            let recovered = switcher::recover(&paths)?;
            if recovered {
                eprintln!("{RESTART_NOTICE}");
            }
            output(
                cli.json,
                serde_json::json!({"recovered":recovered}),
                if recovered {
                    "已恢复未完成事务。"
                } else {
                    "没有未完成事务。"
                },
            )
        }
        Commands::Remove { name } => {
            switcher::remove(&paths, &name)?;
            output(
                cli.json,
                serde_json::json!({"removed":name, "credentials_retained":true}),
                "已移除预设登记；凭据文件及事务历史保留，可用于恢复。",
            )
        }
    }
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("错误：{error:#}");
        std::process::exit(1);
    }
}
