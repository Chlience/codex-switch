# codex switch

跨平台 Codex provider 切换器，命令行程序名为 `codex-sw`。支持已有配置导入、命名凭据、默认 provider 切换、活跃实例提示、撤销和中断恢复。

发现 Codex 仍在运行时，会显示 PID 并提醒重启，切换照常执行。工具不会结束进程、等待退出或要求额外的强制参数。既有会话不会因此自动切换 provider。

从源码安装需要 Rust 1.89 或更新版本：

```text
cargo install --path . --locked
codex-sw --help
```

用户运行已编译的二进制时不需要 Rust、Node.js 或 Python。GitHub Actions 工作流会在 Linux、macOS、Windows 上测试并生成对应二进制构建产物；仓库本身不自动发布或推送。

最常见的用法是先保存当前配置，再添加或导入其他配置：

```text
codex-sw import original
codex-sw import work --from /path/to/config.toml
codex-sw list
codex-sw use work --dry-run
codex-sw use work
codex-sw current
codex-sw doctor
codex-sw undo
```

默认目录是 `CODEX_HOME`，未设置时为用户主目录下的 `.codex`。所有命令支持 `--codex-home <directory>` 和 `--json`；JSON 写入 stdout，警告写入 stderr。

| 命令 | 行为 |
|---|---|
| `import <name>` | 导入当前用户默认配置，保留认证方式；不激活 |
| `import <name> --from <file>` | 从指定 config.toml 导入 |
| `import <name> --profile <name>` | 导入独立 profile，兼容读取旧 `[profiles.*]` |
| `import <name> --provider <id>` | 从配置中选择某个 provider |
| `import --all` | 导入所有 provider；仅当前 provider 自动关联原生认证文件 |
| `import <name> --auth-file <file>` | 显式关联原生认证文件 |
| `add <name> ...` | 添加新的预设，不激活 |
| `list` / `current` | 查看预设和实际用户默认配置，不显示密钥 |
| `use <name>` | 修改默认 provider，提示活跃实例重启 |
| `use <name> --auth-mode auto\|symlink\|copy` | 选择原生认证文件激活方式 |
| `doctor` | 离线检查配置和凭据依赖，显示活跃实例 |
| `undo` | 撤销最近一次切换；遇到外部修改或凭据刷新会拒绝覆盖 |
| `recover` | 恢复中断事务；保留已被外部修改的文件 |
| `remove <name>` | 删除未使用的预设登记；凭据与历史备份保留 |

`import` 和 `use` 支持 `--dry-run`。同名相同内容的导入会跳过；不同内容必须使用新名称。预设名称允许 ASCII 字母、数字、横线及下划线，避开 Windows 保留名称和大小写冲突。

新增一个通过环境变量认证的 provider：

```text
codex-sw add work --base-url https://provider.example/v1 --model your-model --env-key WORK_API_KEY
codex-sw use work
```

请在将要运行 Codex 的环境中设置 `WORK_API_KEY`。工具只保存变量名称，不复制当前环境中的密钥，也不能修改调用它的父 shell 环境。

已有直接 token 可以通过 `import` 保留 `experimental_bearer_token`。新增直接 token 时使用 `add ... --bearer-token-stdin`，从标准输入传入；该选项与 `--env-key`、`--auth-file` 互斥。需要原生认证时使用 `add ... --auth-file <file>`。使用现有官方文件登录可添加 `add official --provider openai`；工具不会为该预设导出系统凭据库。

认证按原方式保存：

- 原生 `auth.json` 按完整字节存为 `codex-sw/credentials/auth.json.<name>`，保留 `auth_mode`、tokens 和未知字段。
- `experimental_bearer_token`、请求头和认证命令保存在私有预设中的配置片段内；激活时恢复到 Codex 配置。整个方案是受文件权限保护的明文存储，不提供加密。
- `env_key` 和 `env_http_headers` 保存环境变量引用。切换时检查所需变量；环境请求头缺失会提示。
- `keyring`、`auto`、`ephemeral` 保留为外部认证方式，工具不导出或验证其中的凭据。
- 认证命令只导入配置；导入和诊断不会执行它。

Linux/macOS 的 `auto` 使用相对软链，Windows 的 `auto` 使用文件复制。Windows 手动选择 `symlink` 时可能需要开发者模式或相应权限。复制模式会在后续切换前回存相同身份的最新凭据；检测到身份已变化时，要求重新导入，避免把新的登录覆盖到旧记录。

```text
CODEX_HOME/
├── config.toml
├── auth.json -> codex-sw/credentials/auth.json.work  （软链模式）
└── codex-sw/
    ├── providers/<name>.json
    ├── credentials/auth.json.<name>
    ├── state.json
    ├── lock
    ├── pending.json   （仅在事务进行中或中断时存在）
    ├── undo.json
    └── history/<transaction>.json
```

导入只提取 provider 和相关模型设置。切换保留 MCP、沙盒、项目授权及其他无关配置，使用 TOML 语法树编辑以保留无关注释；所选 provider 的定义作为整体恢复。配置中未保存的相关模型字段在切换时清除，避免上一个预设的参数残留。已有 config.toml 软链会保留，并编辑其实际目标。

导入旧 profile 不会改写源文件。若正在使用的 config.toml 仍有旧顶层 `profile` 选择器，需先按 Codex 官方指引迁移；工具不会静默删除旧 profile 配置。CLI 参数、独立 profile、项目和管理配置仍可能覆盖用户默认值，`current` 不表示运行中会话的完整配置。

文件写入使用同目录临时文件及替换，工具实例之间使用文件锁。跨文件修改先保存事务日志，异常后通过 `recover` 恢复。每次完成的事务保存在私有 history 中，不自动清理。Unix 管理目录权限为 0700，管理文件为 0600；Windows 管理目录与凭据文件使用当前用户 SID 的受保护 DACL。备份同样包含敏感内容。

活跃实例检测基于进程名称、可执行文件和官方 Node 启动入口；能读取进程环境时排除其他 CODEX_HOME，无法确定目录的实例也会提示。检测属于尽力检查，无法保证识别所有包装器、远程进程或受权限限制的进程。进程检测失败时仍提示后正常切换。

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

测试全部使用临时目录和虚构凭据；活跃进程测试只创建、结束测试自身的子进程。Windows/macOS 的运行时验证由对应平台 CI 执行，本机交叉编译检查不能替代它。
