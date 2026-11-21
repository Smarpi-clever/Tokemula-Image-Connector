# WorkBuddy 适配：注册后保留原生确认

`adapters::workbuddy::WorkBuddyAdapter` 支持以下已核实的 macOS 应用身份与版本：

| 应用 | Bundle ID | 版本 | 默认配置根 |
|---|---|---|---|
| WorkBuddy.app | `com.tencent.workbuddy.mac` | 5.11.3 | `~/.workbuddy` |
| WorkBuddy AI.app | `com.workbuddy.workbuddy-ai` | 5.11.4 | `~/.workbuddy-ai` |

适配器核对应用身份与版本后管理本产品注册和 Skill。新版本的默认根依据发行物 `product.json` 以及桌面启动时注入的配置根；显式 `WORKBUDDY_CONFIG_DIR` 优先，其次为 `CODEBUDDY_CONFIG_DIR`。直接使用 Rust 适配器时继续传入有效 `config_dir`。

## 安装与检查

普通用户通过已安装的管理程序接入：

```sh
tokemula-image install --harness workbuddy --scope user
tokemula-image status --harness workbuddy --scope user
```

管理器先查找用户和系统 Applications 中的 `WorkBuddy AI.app`，再查找同位置的 `WorkBuddy.app`；同名时用户目录优先。非默认路径和隔离验证使用 `--workbuddy-app /absolute/WorkBuddy\ AI.app`。发现与离线管理不要求登录或联网。

App 已打开时，若列表没有显示新注册的连接器，进入“自定义连接器 → 添加 MCP”，保留现有 JSON 设置，仅在文件末尾添加一个换行以启用“保存”。保存后返回列表，再由用户点击 `tokemula-image` 的“信任”。

Rust 调用方式保持不变：

```rust
use std::path::PathBuf;
use tokemula_image_connector::adapters::{
    Scope,
    workbuddy::{WorkBuddyAdapter, WorkBuddyPaths, WorkBuddyStatus},
};

let adapter = WorkBuddyAdapter::new(Scope::User, WorkBuddyPaths {
    home: PathBuf::from("/absolute/home"),
    config_dir: PathBuf::from("/absolute/effective-workbuddy-config"),
    state_dir: PathBuf::from("/absolute/private-tokemula-state/workbuddy"),
    app_bundle: PathBuf::from("/absolute/WorkBuddy.app"),
    executable: PathBuf::from("/absolute/图片工具/tokemula-image-mcp"),
})?;

let plan = adapter.plan()?;
let status = adapter.install()?;
if status == WorkBuddyStatus::AwaitingConfirmation {
    // 在 WorkBuddy 的原生连接器管理中确认 tokemula-image，再调用 check。
}
```

- `detect()` 只读应用 `Info.plist`，核对产品标识和版本，不启动 GUI。
- `plan()` 返回配置/Skill 位置与当前状态；目标程序必须是可执行的独立文件。
- `install()` 最小合并 `mcp.json` 的 `mcpServers.tokemula-image`，安装共用 Skill。缺少当前配置的原生审批时返回 `AwaitingConfirmation`；重复调用保持同一状态与文件。
- `check()` 校验实际配置、Skill 和私有归属记录，再只读核对当前配置对应的原生审批。满足时返回 `Installed`；实时连接与工具发现由 WorkBuddy 原生诊断检查。
- 确认后的重复 `install()` 返回 `AlreadyInstalled`。真实用户修改过产品项时返回冲突。

本入口管理全局注册，用户可在任务中按需启用；`Scope::Project` 返回 `unsupported_scope`。有效配置根由调用方解析；适配器不把 Electron userData 目录或另一产品的路径作为配置根。

## 原生确认与选择性移除

上述版本将用户 MCP 保存在 `<config_dir>/mcp.json`，用户 Skill 位于 `<config_dir>/skills/tokemula-image/SKILL.md`。本产品只保存独立 executable 的绝对路径，不写认证、Provider 或工具权限。

原生确认保存在同根 `mcp-approvals.json`。适配器只读该文件，按此版本原生算法核对当前命令的审批指纹；此 SHA-256 仅用于审批绑定，不是额外的文件校验。审批由 WorkBuddy 自身完成。

用户要求卸载时调用：

```rust
let status = adapter.remove()?;
```

移除仍归本产品的 MCP 项和 Skill 文件，保留其他 MCP、未知字段、用户后加内容、Skill 目录额外文件与原生审批记录。产品项已经移除时，`check/remove` 返回 `NotInstalled`。新安装遇到外来同名目录、注册或 Skill 时返回冲突。

私有快照、归属、原子文件写入和失败恢复复用现有适配组件。`check/remove` 以原归属内容为准，不要求调用方继续提供旧发行路径；不同路径的新安装请求返回 `installation_update_required`。

## 导出官方 Connector 包

```rust
let package = adapter.export_package(&PathBuf::from("/absolute/new-package-dir"))?;
```

目标须为新的绝对目录。包包含：

- `connector-meta.json`：单一产品标识、版本、中英文描述和示例。
- `mcp.json`：一个 stdio server，指向所给独立 executable，无 Key 或 env 注入。
- `icon.svg`。
- `skills/tokemula-image/SKILL.md`：从权威 `skills/tokemula-image/SKILL.md` 原字节生成。

元数据和图标的维护源在 `plugins/workbuddy/`；Skill 业务正文只有一份。导出操作生成包装产物；注册仍通过上述安装与原生确认流程完成。Marketplace 审核发布由后续发行工作处理。

## 原有原生验收与新版兼容检查

C2+C3 使用官方 5.11.3 ARM64 DMG、有效开发者签名和公证结果。隔离诊断运行发行物自身 `daemon-app-server-entry.js --stdio`，使用桌面原生的 Electron Node 模式及明确配置/数据根。该原生 app-server 完成 MCP 注册、确认、连接、八工具发现、Skill 解析/发现和卸载后复查；没有模型 turn 或图片工具调用。

该次隔离验收未启动 GUI；GUI 启动路径会注册系统 URL 协议并处理 LaunchServices 关联。独立 app-server 对未提供的桌面能力收到明确错误并按自身逻辑继续启动，未伪造账户、登录、审批或成功结果。5.11.4 的身份、应用发现、默认配置根、覆盖及隔离管理回归由 WB1 验证；真实 App 生成、编辑、展示和保存另按 App 验收证据记录。
