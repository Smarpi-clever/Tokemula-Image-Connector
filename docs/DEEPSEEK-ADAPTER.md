# DeepSeek Harness：profile 插件适配

`adapters::deepseek::DeepSeekAdapter` 使用 DeepSeek Harness **0.2.2-rc.5** 的官方插件管理入口。它将现有 MCP 可执行程序注册为 `tokemula-image`，安装共用 Skill，不复制图片、模型选择、凭据或计费逻辑。

## CLI/Web 使用

先用对应 Harness 正常初始化所选 profile。官方 Web profile 带有 `mcp-manager`；普通 base、SDK、headless profile 没有该服务。`plan()` 对未初始化 profile 返回 `profile_initialization_required`，对未声明可用管理服务的 profile 返回 `mcp_manager_required`。

```rust
use std::{ffi::OsString, path::PathBuf};
use tokemula_image_connector::adapters::{Scope, deepseek::{
    DeepSeekAdapter, DeepSeekPaths, DeepSeekRuntime,
}};

let adapter = DeepSeekAdapter::new(Scope::User, DeepSeekPaths {
    home: PathBuf::from("/absolute/home"),
    dsh_home: PathBuf::from("/absolute/effective-dsh-home"),
    profile: "web".into(),
    runtime: DeepSeekRuntime::CliWeb,
    project_dir: PathBuf::from("/absolute/project"),
    state_dir: PathBuf::from("/absolute/private-product-state"),
    cli: PathBuf::from("/absolute/installed-dsh"),
    executable: PathBuf::from("/absolute/图片工具/tokemula-image-mcp"),
    process_path: OsString::from("/installed/harness/runtime/bin:/usr/bin:/bin"),
})?;
let plan = adapter.plan()?;
let installed = adapter.install()?;
```

调用方解析有效根与已安装运行时；组件不下载或安装 Node。旧 `Adapter`、`AdapterPaths` 的 Codex/Claude 构造条件不变。

- `detect()`：核对所选 CLI 的实际版本。
- `plan()`：检查 profile、MCP 命名空间、程序、包与 Skill 归属，返回路径和原生 `bundle_enabled`。
- `install()`：写出本产品薄插件，通过官方管理命令安装，再保存共用归属记录；重复安装为 AlreadyInstalled。
- `check()`：核对原生依赖与私有包、Skill 的归属。插件已安装但原生禁用时仍属于已安装状态，启用状态由 plan 单列。
- `remove()`：调用原生包移除，只撤回本产品；重复移除为 NotInstalled。
- `recover()`：自动恢复再次失败时，修复原生运行时或文件权限后显式重试私有记录中的恢复；不会重置整个 profile。`recovery_path()` 提供私有诊断位置。

MCP 是 **profile 全局**注册。`Scope::Project` 返回 `unsupported_scope`，不偷偷写成全局安装。DeepSeek 原生项目 Skill 位于项目 `.dsh/skills`，它与 profile MCP 是不同作用域；组件默认管理 `$DSH_HOME/skills/tokemula-image/SKILL.md`。真实客户端优先使用项目 `.dsh`、项目 `.agents`，再到用户级来源。卸载本产品全局 Skill 保留已有项目 Skill。

## 插件与配置保护

私有包位于 `state_dir/deepseek-packages/<profile>`，名称 `tokemula-image-connector`。生成的 package.json 声明 ESM、`engines.dsh="=0.2.2-rc.5"` 和 `dsh.bundle.patch`；JSON 形式的 cordis.patch.yml 将绝对程序路径交给包入口。公开入口源为 `integrations/deepseek/index.js`，只调用原生 `ctx.mcpManager.register`。该注册自动绑定插件生命周期。

安装使用 `dsh plugin --profile <name> add <absolute-package> --workspace-root --offline --ignore-scripts`；移除使用 `... remove tokemula-image-connector --workspace-root`。两个命令的选项不同，沿本版本实际 pnpm 行为处理。组件不重写 servers map，保留其他插件、profile 字段及用户补丁。安装包没有额外依赖或构建脚本。

原生 `pluginManager/setBundleEnabled` 或 Harness 插件界面可禁用/启用本产品：禁用会撤销 MCP 和工具，启用会重新注册。组件不把 `pnpm disable` 当作有效命令。

归属、快照、原子私有写入及 Skill 引用使用现有共用 State/Ownership。两个 profile 使用同一产品 state 时共享一个用户 Skill；卸载其中一个保留另一个的 Skill。相同名称但无归属记录，或用户改过受管包/Skill时返回冲突。profile/home 补丁对本产品有效命令的覆盖也由原生 dump 检出，check/remove 对照归属记录中的原命令，不使用新安装请求的路径冒充原归属。卸载不恢复整个旧 profile 快照。

安装失败用官方 remove 撤回本产品原生依赖，再恢复本产品本地文件。移除前保存本次私有快照及本产品原依赖和 bundle 位置。原生命令可能先写入再失败；检测到变化时，使用该安装版本的官方包管理 API 恢复本产品，不用旧快照覆盖整个 profile。

恢复 helper 为公开源 `integrations/deepseek/recover.mjs`，编入 Rust 组件，使用 CLI/Web 已有的 Node 和该 CLI 安装内的官方模块；不额外安装 Node。它在原生 profile 写锁内调用 `runProfilePnpm`，设置 `activateNewBundles:false`，随后用官方 `saveManifest` 在当前内容上恢复本产品的原依赖与 bundle 位置。原 disabled 状态、其他插件及失败期间新增的用户字段保持。重装使用原私有包，而非新安装请求的程序路径。

自动恢复成功后保留原操作失败结果，原安装仍能 check/remove。只有恢复再次失败才返回 rollback_required，保留归属、私有包、此次配置快照和 `state_dir/deepseek-recovery/<profile>/remove.json`。修复运行时、原生包管理器或存储权限后调用 `recover()`；成功后可继续 check/remove，恢复记录随之清理。缺少版本匹配的官方恢复 API 同样返回该状态，不猜写 profile。

管理命令清空继承认证，明确 HOME、DSH_HOME、DSH_AGENTS_HOME、XDG 与临时根，关闭本版本遥测，保留原生作用域和审批。Harness 会读取工作目录和 DSH_HOME 中的 `.env`；隔离验收只使用本批空项目和临时根。

## Desktop 当前前置

`DesktopBundled` 用于识别 Desktop 自带入口，不表示本批已支持 Desktop 安装。desktop profile 不能使用 npm CLI；官方要求先启动 Desktop 初始化，然后完全退出，再使用安装内 `resources/runtime/cli/bin/dsh` 管插件。官方同时拒绝 desktop 的 boot/config-dump，不能照搬 CLI/Web 命名空间检查。

用户已选择将 Desktop 留待独立环境验收，本批完成范围为 CLI/Web。本批没有启动 Desktop。当前固定版本启动会使用 Electron userData、平台日志并注册系统 `dsh` 协议关联；尚未取得覆盖这些副作用的官方完整隔离入口。Desktop `plan/install` 在前置检查后返回 `desktop_validation_required`，不注册。

恢复需要：独立 OS 用户/虚拟机或官方完整隔离入口、真实 Desktop 正常初始化并退出、该安装 bundled CLI，以及可验证的原生命名空间冲突检查。CLI/Web 的结果分别保留，不计作 Desktop 通过。

## 已取得的接入证据

实际 Rust API 安装后，官方 Web 管理服务读取插件；MCP watch 返回 connected 与8工具，probe返回准确八工具名且关闭确认成功；空会话的 cold-readable skills/list发现共用 Skill。原生禁用撤销、启用重连、项目 Skill 优先级与卸载后保留均单独核对。测试没有模型 turn、真实 Key或收费图片调用。

图片进入模型仍取决于该模型支持 image 输入且 attachment store 已启用。适配器不更换语言模型 Provider 来满足该条件。自然语言触发、真实图片和 Desktop UI 在后续授权中验收。
