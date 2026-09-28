# 验证依据

2026-09-28 在 WSL2/Linux x86_64 上，对 Codex CLI 0.156.1 做了 43 个隔离案例、142 项结果检查。实际客户端连接本地模拟 Responses、OAuth 刷新和撤销端点，使用虚构凭据。该验证不证明真实服务认证有效，也不覆盖原生 Windows/macOS 或系统凭据库。

| 情况 | 本机实际行为 |
|---|---|
| auth.json 软链下登录、正常刷新 | 保留链接，更新目标 |
| 软链下登出 | 删除链接，目标保留；OAuth 调用撤销接口 |
| 刷新未完成时切换软链到 B | B 记录混入 A 的新 token |
| 刷新未完成时替换普通 auth.json | 活动文件混入 A 的新 token |
| 目标已有 0644 权限，随后登录 | 权限仍是 0644，因此工具自行保护文件 |
| env_key 与直接 token 共存 | 使用环境变量 |
| env_key 与 requires_openai_auth=true 共存 | 本机模型请求使用环境变量，与当前认证文档的描述不同 |
| env_key 缺失或为空，另有其他凭据 | 报错，不回退 |
| 直接 token 与原生文件认证共存 | 使用直接 token |
| 直接 token 为空 | 发送空 Bearer 头，不回退 |
| 认证命令与直接 token 共存 | 配置加载失败 |
| 原生文件同时有 key 与 tokens | 显式 auth_mode 影响选择，需完整保留 |
| 模型请求使用直接 token，但目录有过期 OAuth | 仍观察到该 OAuth 被刷新；具体启动或辅助功能原因未追踪 |
| 新版独立 profile | 能覆盖 provider 配置 |
| 旧版顶层 profile 选择器 | 当前客户端拒绝启动 |

运行中切换的提醒行为由用户明确要求：显示活跃实例并提醒重启，正常完成切换，不阻断、不要求 --force、不结束进程。发现已经发生的文件并发修改或恢复冲突仍会报告错误，避免覆盖用户修改。

产品测试在 `tests/workflows.rs`，本机 `cargo test --locked` 的 25 项集成测试通过，覆盖真实 CLI 进程提示及继续切换、导入保留、敏感信息隐藏、原生认证文件、复制模式刷新回存、无关配置与软链保护、撤销冲突、预提交事务回滚和已提交事务恢复。

实现后又使用本机 Codex 0.156.1 完成四项端到端检查：直接 token、环境变量、原生认证软链和原生认证复制。每项均通过 codex-sw 导入、切至另一预设、再切回，随后启动真实 Codex 连接本地模拟 Responses 服务；四项均完成请求，且请求头使用预期的虚构凭据。该检查不访问真实模型服务。

`cargo fmt --check`、Linux 与 Windows MSVC 目标的 `cargo clippy --locked --all-targets -- -D warnings` 均通过；Windows 检查另外带 `--target x86_64-pc-windows-msvc`。`cargo build --locked --release` 已生成本机 Linux 二进制。CI 配置了 Linux、macOS、Windows 原生测试与构建矩阵。

本机只能执行 Linux 测试和 Windows 目标编译检查；未声称其他平台 CI 已运行。事务恢复测试通过构造中断状态验证恢复行为，没有模拟断电、文件系统故障或真实 OAuth 服务。进程检测和文件内容变化检查不能替代会话认证隔离。

## Provider 切换范围回归

后续将切换范围收窄到 provider 地址和认证配置。在 Linux x86_64 上，`cargo test --locked` 的 28 项集成测试通过，覆盖新旧预设保留当前模型参数及未设置状态、模型变化后的预设匹配与重复导入、带表头的列表、长名称对齐，以及拒绝 `add --model`。

子进程测试移至 `tests/cli.rs`，通过 CLI 执行配置写入；持有文件锁的库测试保留在 `tests/workflows.rs`。两个测试程序分别连续运行 20 次，均通过，未再复现原先并行启动子进程时出现的文件锁竞争。

`cargo fmt --check`、Linux 的 `cargo clippy --locked --all-targets -- -D warnings` 和 Windows MSVC 的 `cargo check --locked --all-targets --target x86_64-pc-windows-msvc` 均通过，release 二进制已构建。本轮未执行 Windows/macOS 原生运行时测试，也未重跑上文使用 Codex CLI 0.156.1 的模型请求兼容性案例。

## 交互式添加验证

新增 `codex-sw add` 无参数入口，依次粘贴 provider 配置与 auth.json，使用所选 provider 的 `name` 保存预设。Linux 上的 33 项集成测试通过，覆盖单个 provider 与完整配置、名称重复及大小写冲突、保存时发生的并发重名、无效内容重试、EOF 和输入大小限制，以及原始凭据字节保留。

使用已安装的 release 二进制完成了 Linux 终端交互验证：粘贴多行 TOML 和 JSON，以独立的 `END` 行结束，预设名称自动取自 `name`，凭据文件权限为 0600，当前配置未改变。格式检查、Linux Clippy 和 Windows MSVC 编译检查通过；本轮未运行 Windows/macOS 终端交互或真实模型请求。

## 参考资料

- [Codex 配置参考](https://learn.chatgpt.com/docs/config-file/config-reference)
- [Codex 高级配置](https://learn.chatgpt.com/docs/config-file/config-advanced)
- [Codex 认证](https://learn.chatgpt.com/docs/auth)
- [Windows 软链创建条件](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createsymboliclinkw)
