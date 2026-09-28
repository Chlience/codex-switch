# codex switch

跨平台 Codex provider 切换器，命令行程序名为 `codex-sw`。只切换 provider 及其地址与认证配置，保留当前模型、推理强度和其他运行参数。支持已有配置导入、Provider 凭据、默认 provider 切换、重启提示、撤销和中断恢复。

切换成功后，工具固定提示“已有 Codex 实例需要重启后使用新配置”。撤销成功或实际恢复事务后也会提示；预览、诊断和失败操作不提示重启。工具不扫描 Codex 进程，也不输出 PID；既有会话不会因此自动切换 provider。

从源码安装需要 Rust 1.89 或更新版本：

```text
cargo install --path . --locked
codex-sw --help
```

用户运行已编译的二进制时不需要 Rust、Node.js 或 Python。GitHub Actions 工作流会在 Linux、macOS、Windows 上测试并生成对应二进制构建产物；仓库本身不自动发布或推送。

最常见的用法是先保存当前 provider 和认证配置，再添加或导入其他 provider：

```text
codex-sw import
codex-sw import --from /path/to/config.toml --provider company
codex-sw list
codex-sw use company --dry-run
codex-sw use company
codex-sw current
codex-sw doctor
codex-sw undo
```

默认目录是 `CODEX_HOME`，未设置时为用户主目录下的 `.codex`。所有命令支持 `--codex-home <directory>` 和 `--json`；JSON 写入 stdout，警告写入 stderr。

| 命令 | 行为 |
|---|---|
| `import` | 导入当前默认 provider 及认证配置；不激活 |
| `import --from <file>` | 从指定 config.toml 导入 |
| `import --profile <profile>` | 提取独立 profile 的 provider 配置，兼容读取旧 `[profiles.*]` |
| `import --provider <id>` | 从配置中选择某个 provider |
| `import --all` | 导入所有 provider；仅当前 provider 自动关联原生认证文件 |
| `import --auth-file <file>` | 显式关联原生认证文件 |
| `add` | 依次粘贴 provider 配置和 auth.json，按 Provider ID 登记 |
| `add <provider> ...` | 添加新的 Provider，不激活 |
| `list` | 显示带表头的 Provider 和端点两列表格 |
| `current` | 查看实际用户默认 provider 和模型，不显示密钥 |
| `use <provider>` | 修改默认 provider，提示已有实例重启 |
| `use <provider> --auth-mode auto\|symlink\|copy` | 选择原生认证文件激活方式 |
| `doctor` | 离线检查配置和凭据依赖 |
| `undo` | 撤销最近一次切换；遇到外部修改或凭据刷新会拒绝覆盖 |
| `recover` | 恢复中断事务；保留已被外部修改的文件 |
| `remove <provider>` | 删除未使用的 Provider 登记；凭据与历史备份保留 |

`import` 和 `use` 支持 `--dry-run`。所有选择操作使用 Provider ID，即 `[model_providers.<id>]` 中的 `<id>`；内置 Provider 使用其固定 ID，例如 `openai`。ID 区分大小写，支持中文，不能为空或包含控制字符。

每个 Provider ID 只登记一条配置与认证记录。重复导入相同内容会跳过；内容不同则报错并保留原记录。`add` 拒绝已有 ID，交互式添加会在接收 auth.json 前检查重复，保存时在文件锁内再次检查。需要保留多个端点或账号时，在配置中使用不同的 Provider ID。

无需先创建文件时，运行：

```text
codex-sw add
```

按提示粘贴 provider 的 TOML 配置，单独输入一行 `END` 结束。例如：

```toml
[model_providers.company]
name = "Company gateway"
base_url = "https://provider.example/v1"
wire_api = "responses"
```

此配置的 Provider ID 为 `company`，`name` 仅作为 Codex 服务商配置中的显示名称保留。也可以粘贴完整 `config.toml`；含多个 provider 时，需要用顶层 `model_provider` 指定所选 Provider。工具检查 ID 是否已登记，通过后接收认证内容。

随后粘贴完整 auth.json 内容，再单独输入一行 `END`：

```json
{"OPENAI_API_KEY": "your-api-key"}
```

输入通过校验后自动保存 Provider，无需额外确认；使用 `codex-sw use company` 激活。auth.json 的原始字节和未知字段会保留。交互式添加为自定义 provider 设置 `requires_openai_auth = true`；如配置包含 `env_key`、`experimental_bearer_token` 或认证命令，先移除这些认证字段再粘贴。模型和其他运行参数仍不进入保存的 Provider 配置。

两段输入均支持空行和多行，每段最多 1 MiB，结束标记不计入内容。输入结束前必须提交 `END`；读取到 EOF 或中途退出时不保存未完成的 Provider 记录。交互提示写入 stderr，`add --json` 的保存结果写入 stdout。程序的结果和错误消息不包含粘贴的凭据内容。

`list` 显示保存的 Provider ID 和端点，按显示宽度对齐，例如：

```text
Provider  端点
────────  ────
company   https://company.example/v1
proxy     https://proxy.example/v1
```

端点取自该 Provider 的 `base_url`；内置 `openai` 使用保存的 `openai_base_url`。未保存显式地址的内置 Provider 显示“Codex 默认”，自定义 Provider 显示“未配置”。这些是保存的配置，环境变量、启动参数和项目配置仍可能影响运行中的实际地址。URL 中的用户名、密码、查询参数和片段会隐藏；JSON 的 `endpoint` 同样经过处理，未保存地址时为 `null`。

独立预设别名已取消。旧记录仍按其中的 Provider ID 读取，例如旧别名 `original` 指向 `proxy` 时，使用 `codex-sw use proxy`。旧文件名、认证软链和事务索引保持有效；不需要改名或迁移凭据。若同一 Provider ID 对应多条旧记录，选择、导入或删除时会报告冲突，要求先整理重复登记，避免选错凭据。

`list --json` 不再返回 `name`；`use --json` 使用 `provider` 字段。`current --json` 通过 `matches_saved` 表示当前配置和认证是否匹配保存记录。

新增一个通过环境变量认证的 provider：

```text
codex-sw add work --base-url https://provider.example/v1 --env-key WORK_API_KEY
codex-sw use work
```

请在将要运行 Codex 的环境中设置 `WORK_API_KEY`。工具只保存变量名称，不复制当前环境中的密钥，也不能修改调用它的父 shell 环境。

模型由当前 Codex 配置决定，`add` 不接受 `--model` 参数。

已有直接 token 可以通过 `import` 保留 `experimental_bearer_token`。新增直接 token 时使用 `add ... --bearer-token-stdin`，从标准输入传入；该选项与 `--env-key`、`--auth-file` 互斥。需要原生认证时使用 `add ... --auth-file <file>`。使用现有官方文件登录可添加 `add openai`；工具不会为该 Provider 导出系统凭据库。

认证按原方式保存：

- 原生 `auth.json` 按完整字节存为 `codex-sw/credentials/auth.json.<storage-key>`，保留 `auth_mode`、tokens 和未知字段。
- `experimental_bearer_token`、请求头和认证命令保存在私有 Provider 记录中的配置片段内；激活时恢复到 Codex 配置。整个方案是受文件权限保护的明文存储，不提供加密。
- `env_key` 和 `env_http_headers` 保存环境变量引用。切换时检查所需变量；环境请求头缺失会提示。
- `keyring`、`auto`、`ephemeral` 保留为外部认证方式，工具不导出或验证其中的凭据。
- 认证命令只导入配置；导入和诊断不会执行它。

Linux/macOS 的 `auto` 使用相对软链，Windows 的 `auto` 使用文件复制。Windows 手动选择 `symlink` 时可能需要开发者模式或相应权限。复制模式会在后续切换前回存相同身份的最新凭据；检测到身份已变化时，要求重新导入，避免把新的登录覆盖到旧记录。

```text
CODEX_HOME/
├── config.toml
├── auth.json -> codex-sw/credentials/auth.json.work  （软链模式）
└── codex-sw/
    ├── providers/<storage-key>.json
    ├── credentials/auth.json.<storage-key>
    ├── state.json
    ├── lock
    ├── pending.json   （仅在事务进行中或中断时存在）
    ├── undo.json
    └── history/<transaction>.json
```

导入只提取 `model_provider`、`openai_base_url`、所选 `model_providers.<id>` 定义及相关认证方式。切换只更新这些 provider 设置和必要的认证存储配置。所选 provider 的定义作为整体恢复；`openai_base_url` 以保存记录为准，记录未设置时清除。模型、推理强度、上下文窗口、模型目录、`service_tier`、MCP、沙盒、项目授权和其他设置保持当前值，包括未设置状态。工具使用 TOML 语法树编辑以保留无关注释。已有 config.toml 软链会保留，并编辑其实际目标。

执行 `use` 时，工具在写入的 provider 定义前添加管理注释，并在各 provider 配置块前保留一个空行。例如，激活 `company` 后：

```toml
# 手动维护的 provider
[model_providers.personal]
name = "Personal"
base_url = "https://personal.example/v1"

# Managed by codex-sw
[model_providers.company]
name = "Work"
base_url = "https://company.example/v1"
```

注释标明该定义由 `codex-sw` 写入；Provider ID 已显示在配置块标题中。已有用户注释会保留，重复切换不会累积管理注释或空行。工具会将行内和点分写法的 provider 整理为独立配置块，保留配置值。`add` 和 `import` 只保存 Provider 记录，不修改源配置或添加管理标记；内置 provider 没有自定义定义时也不创建配置块。`undo` 会恢复切换前的配置文本。

已有 Provider 记录保持可读取，其中旧版本保存的模型参数会被忽略，不再参与切换、记录匹配或重复导入判断。工具不会自动重写旧记录文件。`list` 的文本和 JSON 输出均不包含模型；`current` 仍显示配置中的实际默认模型。

导入旧 profile 不会改写源文件。若正在使用的 config.toml 仍有旧顶层 `profile` 选择器，需先按 Codex 官方指引迁移；工具不会静默删除旧 profile 配置。CLI 参数、独立 profile、项目和管理配置仍可能覆盖用户默认值，`current` 不表示运行中会话的完整配置。

文件写入使用同目录临时文件及替换，工具实例之间使用文件锁。跨文件修改先保存事务日志，异常后通过 `recover` 恢复。每次完成的事务保存在私有 history 中，不自动清理。Unix 管理目录权限为 0700，管理文件为 0600；Windows 管理目录与凭据文件使用当前用户 SID 的受保护 DACL。备份同样包含敏感内容。

`use` 和 `doctor` 的 JSON 输出不再包含 `active_instances`；工具不判断是否有运行中的 Codex 实例。重启提示写入 stderr，JSON 结果仍写入 stdout。

已在 Codex 0.156.1 上复现：旧实例进行 OAuth 刷新时切换共享 auth.json，旧 token 可能写入新记录。软链与复制模式都会受到影响。按产品约定，工具仅提醒重启，继续切换，不自动等待或阻止。文件锁和事务日志不保证运行中会话之间的认证隔离。不同账号并行会话、独立 CODEX_HOME 启动器和后台代理不在首版范围内。

登出可能删除 auth.json 链接，OAuth 登出还可能撤销远端凭据；保留的命名文件不能据此认定仍然有效。`doctor` 只检查本地结构，不验证真实服务的 token 有效性。空 token 或缺失的必要环境变量会阻止激活，导入可以保留有问题的配置并报告待修正项。

本项目采用的认证兼容事实及本机验证范围见 [验证记录](docs/validation.md)。不同 Codex 版本的优先级可能变化，因此保留原始认证组合，不将各种凭据强制转换成同一种形式。

开发验证：

```text
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo check --locked --all-targets --target x86_64-pc-windows-msvc
cargo build --locked --release
```

测试全部使用临时目录和虚构凭据，覆盖新旧记录切换时保留模型参数、参数未设置状态、记录匹配、重复导入及 CLI 输出；CLI 测试覆盖固定重启提示，以及预览、诊断和失败操作不提示重启。Windows/macOS 的运行时验证由对应平台 CI 执行，本机交叉编译检查不能替代它。
