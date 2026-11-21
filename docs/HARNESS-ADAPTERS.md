# Harness 适配组件

`tokemula_image_connector::adapters` 提供安装器可直接复用的 Rust API。它注册已有独立 MCP 可执行文件，并从 `skills/tokemula-image/SKILL.md` 安装同一份 Skill；注册名和 Skill 名统一为 `tokemula-image`。本组件不分发程序、不保存 Key，也不修改语言模型 Provider 或审批。

## 调用入口

```rust
use std::{ffi::OsString, path::PathBuf};
use tokemula_image_connector::adapters::{Adapter, AdapterPaths, Harness, Scope};

let adapter = Adapter::new(Harness::Codex, Scope::User, AdapterPaths {
    home: PathBuf::from("/absolute/user-home"),
    codex_home: PathBuf::from("/absolute/effective-codex-home"),
    claude_config_dir: Some(PathBuf::from("/absolute/effective-claude-config")),
    project_dir: PathBuf::from("/absolute/project"),
    state_dir: PathBuf::from("/absolute/private-tokemula-state"),
    cli: PathBuf::from("/absolute/codex"),
    executable: PathBuf::from("/absolute/图片工具/tokemula-image-mcp"),
    process_path: OsString::from("/absolute/node/bin:/usr/bin:/bin"),
})?;

let plan = adapter.plan()?;
let installed = adapter.install()?;
let owned = adapter.check()?;
// A changed program or embedded Skill uses the explicit managed-update path.
let updated = adapter.update()?;
let removed = adapter.remove()?;
```

调用方将当前有效的 HOME、CODEX_HOME 及目标项目解析为绝对路径，明确传入。CLAUDE_CONFIG_DIR 未设置时传 None；显式设置时传 Some(绝对路径)，两者保留原生配置位置差异。CLI 与 MCP 程序也使用绝对路径，不注册 `cargo run` 或开发脚本。`process_path` 供 CLI 的 Node 等启动器使用，不写入注册配置。

- `detect()`：执行当前 CLI 的版本命令，返回版本；未安装、不可执行或无法识别时返回具体错误码。
- `plan()`：检测版本、程序和当前配置归属，返回实际配置/Skill 路径及 Ready/Installed，不修改注册。
- `install()`：实际写入注册和 Skill；重复安装返回 AlreadyInstalled。
- `check()`：检查配置与 Skill 是否仍匹配本产品归属记录。客户端是否连接、是否加载 Skill 由原生客户端诊断另验。
- `update()`：当前内容仍匹配原归属时，更新请求的程序与本编译内嵌Skill，并同步共享引用；用户修改过的内容返回冲突。
- `remove()`：读取当前配置，只移除仍归本产品的注册与 Skill；没有安装返回 NotInstalled。

这些方法为同步本地操作，管理程序复用同一套入口。原 `Harness::{Codex,ClaudeCode}`、`Adapter::new` 和 `AdapterPaths` 保持原构造方式；新增客户端使用下文各自明确的参数类型，旧调用者无需填写新字段。

## 成套升级与旧记录继续

产品二进制升级优先使用稳定MCP路径，减少Harness配置改动。新版本的管理程序调用 `update()` 或 `update_owned(state_dir, stable_mcp)`，以新编译内嵌的Skill/插件资源更新受管内容；回退时由保留的旧管理程序调用相同入口，恢复旧版本正文。不能在旧A进程中假称已应用B的内嵌资源。

`install()`仍是接入安装，程序/Skill不同时继续返回`installation_update_required`；已有`remove()`仍只移指定接入。需要更新现有单个目标时，使用管理程序的明确命令：

```sh
/absolute/installed/bin/tokemula-image update integration --harness codex --scope user
```

项目、非默认配置根或DeepSeek profile沿原`--project/--home/--profile/--client/--state-dir`参数解析，不手填内部AdapterPaths。旧归属记录没有新context时，显式目标`update`先核对现有原归属并补齐必要上下文；不会要求手改ownership.json或卸载全部Harness。用户修改过entry/Skill仍保留冲突。

生命周期API还提供`owned_inventory(state_dir)`、`update_owned(state_dir, executable)`、`remove_owned(state_dir)`、`recover_owned_updates(state_dir)`和`cleanup_empty_owned_state(state_dir)`。批量结果含`completed`及逐项状态/错误：更新先全部预检，失败撤销本次已更新目标；移除成功者退出归属，失败者保留。调用方必须在completed=true后才删除产品程序。客户端缺失、旧context不足或用户修改时保留管理/恢复入口，不把部分清理报成功。

外部接入全部成功移除、且没有待恢复操作后，cleanup_empty_owned_state只删除已识别且installations为空的归属文件，并rmdir所属空目录。偏好、任务引用、未知字段、用户文件与非空目录保留；尚有接入或恢复journal时拒绝清理，调用方继续保留管理程序。

新安装在同一ownership.json记录可选、非秘密的实际客户端路径/作用域上下文。旧记录缺context仍可按原显式Adapter构造检查、更新和移除；自动批量流程不猜上下文。OpenCode内联config内容可能含其他工具凭据，因而不持久化；依赖这种覆盖的目标由原显式配置环境处理。

受管更新的私有`managed-update.private.json`只记录本次旧/新受管内容与快照引用。进程在配置、共享Skill或归属之间中断时，recover只接受仍匹配旧/新版本的内容，局部恢复本产品entry及引用，不覆盖用户后来新增字段。恢复失败保留journal和管理工具供继续，不使用安装前整份旧用户配置回填。外部Harness全体卸载/重装不是更新实现。

## Codex 与 Claude Code 的原生入口

| 目标 | MCP 配置 | Skill |
|---|---|---|
| Codex 用户 | 有效 CODEX_HOME/config.toml，官方 mcp add/remove 后保格式合并 | HOME/.agents/skills/tokemula-image/SKILL.md |
| Codex 项目 | 项目/.codex/config.toml，以 toml_edit 最小修改并保留注释 | 项目/.agents/skills/tokemula-image/SKILL.md |
| Claude 用户 | 默认 HOME/.claude.json；有 override 时 CLAUDE_CONFIG_DIR/.claude.json，官方 --scope user | 默认 HOME/.claude/skills/tokemula-image/SKILL.md；有 override 时使用该根/skills |
| Claude 项目 | 项目/.mcp.json，官方 mcp add/remove --scope project | 项目/.claude/skills/tokemula-image/SKILL.md |

Claude 的 local 是项目私有作用域，本产品不将它作为全局安装。普通 settings.json 不用于写入 MCP 状态。Codex 当前 `mcp add` 没有项目 scope 参数，项目配置由组件直接编辑。CLI/App 共读同一配置时使用同一适配目标，不另建 App 注册。

当前 Codex 用户级官方命令会重排其他 MCP 内容并丢失部分尾注释。组件在官方操作成功后，基于本次读取的原 TOML 文档只应用本产品的增删，保留无关字段和注释；项目级复用同一编辑逻辑。

项目 MCP 的信任与审批保留客户端原行为。安装成功后，未信任项目可能尚未连接；本组件不替用户批准项目。

## 归属与失败恢复

私有 state_dir 内保存 `ownership.json` 和安装前配置快照。Unix 目录 0700、文件 0600；快照可能包含其他工具的配置或凭据，不进入日志、源码或支持报告。错误只输出固定说明，不回显 CLI 载荷或配置内容。

没有归属记录的同名 MCP 或 Skill 视为冲突，即使名称和路径看起来相同，也不接管。用户修改过本产品注册或 Skill 时，check/install/remove 返回 ownership_conflict，保留现状。请求安装的程序路径或内嵌 Skill 版本与已受管安装不同时，plan/install 返回 installation_update_required；check/remove 仍按原归属记录操作。卸载使用当前文件移除本产品项，保留后来新增的 Provider、MCP、字段和其他 Skill；不将安装前整份快照覆盖到卸载结果。

安装或移除步骤失败时恢复本次操作开始前的配置与 Skill；自动恢复失败时返回 rollback_required 并保留私有快照。正常卸载后删除对应快照和归属项，仅删除空的本产品 Skill 目录；用户添加的其他文件保留。

CLI 子进程清空继承环境，只注入明确的 HOME、配置根、XDG 路径、临时目录和启动 PATH。Claude 使用 `--bare`，关闭 Keychain/OAuth 自动读取；管理命令不启动模型会话。注册本身只包含可执行程序路径和原生非秘密字段。

## OpenCode V2：用户与项目作用域

`adapters::opencode::{OpenCodeAdapter, OpenCodePaths, OpenCodeOverrides}` 提供同名的 `detect/plan/install/check/update/remove`，创建方式为 `OpenCodeAdapter::new(scope, paths, overrides)`。当前契约固定为官方 V2 2.3.2；V1 只返回适用版本提示，不自动升级或迁移。

`OpenCodePaths` 只包含 HOME、XDG_CONFIG_HOME、项目、私有 state、CLI、MCP executable 和启动 PATH。`OpenCodeOverrides` 显式携带当前有效的配置目录、配置文件、配置内容及原生发现开关；默认不更改客户端规则。

注册使用 `mcp.servers.tokemula-image={type:"local",command:[绝对路径]}`。组件按真实加载顺序选择当前有效文件，保留 JSONC 注释、未知字段和其他配置。官方 `mcp add` 的文件选择与实际生效目标一致时使用它；选址不一致或移除时用成熟 CST 仅合并本产品条目，不新建更高优先级文件遮蔽原设置。只读覆盖或管理策略控制同名项时明确返回状态。

Skill 采用原生的路径 ID 和加载优先级；不以 frontmatter 名称猜 ID。默认新文件位于用户有效 OpenCode 根或项目 `.opencode/skills`。已发现同源、已归属的有效 Skill 时复用该文件，具体共享条件见下一节。完整路径与行为说明见 [OpenCode 适配](OPENCODE-ADAPTER.md)。

## DeepSeek：profile 内的薄插件

`adapters::deepseek::{DeepSeekAdapter, DeepSeekPaths, DeepSeekRuntime}` 通过 `DeepSeekAdapter::new(Scope::User, paths)` 管理明确 profile。`DeepSeekPaths` 显式包含 DSH_HOME、profile 名称及 `CliWeb/DesktopBundled` 运行时来源；不会给旧 AdapterPaths 添加参数。

当前使用官方 `@deepseek-ai/dsh` 0.2.2-rc.5 契约。MCP 是 profile 全局注册；请求 `Scope::Project` 返回 unsupported_scope，绝不暗中转全局。原生项目 Skill 目录 `.dsh/skills` 与 MCP profile 作用域分开；本适配器将共用 Skill 安装到 DSH_HOME/skills。完整接口与当前前置见 [DeepSeek 适配](DEEPSEEK-ADAPTER.md)。

唯一薄插件源为 [integrations/deepseek/index.js](../integrations/deepseek/index.js)。适配器生成匹配版本的 manifest 与 Cordis patch，再调用官方 plugin add/remove，插件只以生命周期注册现有 binary；不复制图片、凭据或计费逻辑，不改写整个 servers map。计划输出包含 profile、包位置、Skill 位置及 bundle_enabled，禁用与注册归属分别表达。原生插件禁用撤销 MCP 服务；静态 Skill 由适配器管理，remove 才按引用清理，不把文件仍存在误说成插件仍启用。

原生移除写入后失败时，组件通过版本匹配的官方包管理 API 仅恢复本产品依赖和 bundle 位置，保留 disabled 与其他插件。若恢复再次失败，返回 `rollback_required` 并保留私有记录；修复运行时或权限后调用 `DeepSeekAdapter::recover()`，不手工重写 profile。恢复 helper 源为 `integrations/deepseek/recover.mjs`，使用 CLI/Web 已有 Node，不新增用户运行时。

profile 必须已由对应客户端初始化。本批经用户确认验收 CLI/Web，可操作已初始化的对应 profile；Desktop 留待独立环境验收，当前保持 `desktop_validation_required`，不进行注册。Desktop 恢复条件是安全隔离初始化、完全退出及 bundled CLI 的实际验收；npm CLI 不能管理 desktop profile，CLI/Web 证据不代替 Desktop。有效 native dump 为 YAML，仅读取数据检查 manager 与同名冲突，不执行 tagged 内容。用户无需为本产品另装 Node，调用者提供已有 Harness 的运行时。

## WorkBuddy：注册与原生确认分开

`adapters::workbuddy::{WorkBuddyAdapter, WorkBuddyPaths, WorkBuddyStatus}` 通过 `WorkBuddyAdapter::new(Scope::User, paths)` 管理 macOS WorkBuddy 5.11.3（原身份）及 WorkBuddy AI 5.11.4（新身份）。专属路径包括 HOME、有效 WORKBUDDY_CONFIG_DIR、私有 state、应用包和独立 MCP 程序。完整示例见 [WorkBuddy 适配](WORKBUDDY-ADAPTER.md)。

按真实官方管理入口确认的结构，仅合并配置根 `mcp.json` 中的本产品项，并安装配置根 `skills/tokemula-image/SKILL.md`。没有等价项目 MCP 目录；项目请求返回 unsupported_scope，用户仍按 WorkBuddy 原生全局管理、任务启用方式使用。

`install/check` 在原生审批未完成时返回 **AwaitingConfirmation**；匹配当前配置的原生审批完成后才返回 Installed。该状态表示注册和批准已成立，实时连接由客户端诊断检查。产品不写审批、不改变登录或信任规则；SHA-256 只用于核对官方审批的配置完整性指纹。

`export_package(destination)` 生成官方 MCP + Skill Connector 包：元数据与图标来自 [plugins/workbuddy](../plugins/workbuddy)，mcp.json 写入实际程序绝对路径，Skill 正文直接来自同一源。导出包不是自动注册，不能把导出成功当作 Installed。

## 跨客户端共享 Skill

原有单目标 state_dir 用法继续可用。要让 OpenCode 复用 Codex/Claude 已安装的同源 Skill，调用者须将同一私有 state_dir 传给相关适配目标。记录按 Harness、作用域和真实配置路径区分；只有已记录归属、当前字节一致且原生优先级确实生效的副本才可复用。

不同 state_dir 或未知同名 Skill 返回 ownership_conflict，不通过相同正文猜测接管，也不为通过安装而复制高优先级副本。共用引用存在时卸载一者保留文件并转移最后清理责任；最后一个受管引用移除后才删除原受管 Skill。原客户端构造和旧归属记录不新增前置条件。

所有目标继续使用同一 ownership.json、私有快照及原子文件操作。旧记录没有 skill_owned 字段时仍按原自有 Skill 解释；新借用记录明确记为非自有，不改变旧字段含义。

## 接入验证

已有 C1 本机目标版本为 Codex CLI 0.170.0、Claude Code 2.1.329；C2+C3 使用 OpenCode V2 2.3.2、DeepSeek 0.2.2-rc.5、WorkBuddy 5.11.3 的定向版本契约。隔离验收分开记录：配置保护、客户端实际解析/连接、工具发现、Skill 元数据/发现，以及模型自主调用。真实客户端检查通过官方 app-server、mcp get/list 和可用 Skill 校验入口完成，不发送模型 turn。

C1 macOS 证据由资料目录的 B3c-C1-VALIDATION 汇总；新增目标的实际完成层级、原生诊断与剩余条件由 C2-C3-VALIDATION 汇总。DeepSeek Desktop 是用户已同意的独立环境后续项，当前批次按 CLI/Web 范围验收。Windows/Linux 的真实客户端接入、App UI、自然语言自主触发、真实收费与安装器发行布局分别在后续验收；本组件的绝对路径参数由后续安装器复用。
