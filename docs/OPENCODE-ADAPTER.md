# OpenCode V2 适配

`adapters::opencode::OpenCodeAdapter` 管理 OpenCode 2.3.2 的用户或项目 MCP 注册与共用 Skill。它沿用私有归属、快照、原子写入及恢复逻辑，不修改原 Codex/Claude 构造参数。V1 和其他未验版本返回 `unsupported_version`，保留客户端及配置。

## 使用接口

```rust
use tokemula_image_connector::adapters::{Scope, opencode::{OpenCodeAdapter, OpenCodePaths, OpenCodeOverrides}};

let adapter = OpenCodeAdapter::new(Scope::User, OpenCodePaths {
    home: "/absolute/home".into(),
    xdg_config_home: "/absolute/effective-xdg-config".into(),
    project_dir: "/absolute/project".into(),
    state_dir: "/absolute/private-tokemula-state".into(),
    cli: "/absolute/opencode".into(),
    executable: "/absolute/图片工具/tokemula-image-mcp".into(),
    process_path: "/usr/bin:/bin".into(),
}, OpenCodeOverrides::default())?;
let plan = adapter.plan()?;
let installed = adapter.install()?;
```

`detect/plan/install/check/remove` 的状态和错误沿共用适配器。`check` 检查当前配置和归属；原生客户端连接与 Skill 发现由客户端诊断确认。

调用方提供实际有效 HOME、XDG_CONFIG_HOME、程序和项目路径。`OpenCodeOverrides` 显式承接 `OPENCODE_CONFIG_DIR`、`OPENCODE_CONFIG`、`OPENCODE_CONFIG_CONTENT`，以及当前项目配置/Claude Skill 发现禁用状态；默认不改变发现规则。已有覆盖包含同名服务器而不可写时返回具体错误，不建立另一个文件遮蔽它。

## 生效配置与保护

V2 注册为 `mcp.servers.tokemula-image={"type":"local","command":[绝对程序路径]}`，不写 V1 的 enabled。CLI 使用真正独立 executable，不保存 Key，不注册 cargo run。

加载顺序由低到高：有效 global 根的 JSON→JSONC、显式配置文件、远祖先到当前目录的直接 JSON→JSONC、同顺序 .opencode/JSON→JSONC、内联配置、系统管理配置。普通祖先遍历至文件系统根，不按 .git 截断。

适配器选择该作用域真正提供条目的源；没有条目时选择已有最高优先级可写源，最后才在该作用域默认位置新建。不会为已有 JSONC 另建 JSON 或高优先级文件。系统管理条目及不可写内联覆盖返回明确状态，保留原策略。

官方 mcp add 在其选址与实际目标相同时使用。它按 JSON 优先选择文件，而 loader 的 JSONC 优先；不匹配时直接用成熟 JSONC CST 修改正确文件。增删都保留原文中的无关字段和注释；没有官方 remove 命令时只移除本产品 entry。

新装、重复安装、冲突和卸载使用同一 ownership.json。任何作用域都先核对当前生效的同名服务器与归属记录；外来或用户修改过的来源不能通过新建更高优先级项目文件遮蔽。已经由同一 State 管理的 user/project 注册可并存，各自选择性移除并保留另一作用域及共享 Skill。受管内容被修改时返回冲突。卸载读取当前文件，保留其他 MCP、Provider、模型、未知字段和后来添加的内容；失败恢复本次操作前的文件，不重置其他客户端配置。

## Skill 只使用一个业务源

Skill 正文始终来自 `skills/tokemula-image/SKILL.md`。OpenCode 的有效 ID 来自文件名或目录名，不由 frontmatter name 决定；适配器按原生来源优先级找到 `tokemula-image` 的实际副本。

兼容 .claude/skills 先于 .agents/skills；原生全局与项目 .opencode 的 skill/skills 随后，配置声明的 skills 源最后。同一层按照原生文件扫描顺序，后者覆盖相同 ID。原生安装默认使用有效 global 根或项目 .opencode/skills/tokemula-image/SKILL.md。

如果实际副本已由本产品管理且字节相同，OpenCode 复用并登记引用，不再复制一份。跨 Harness 共用 Skill 时传入同一私有 state_dir；另一客户端卸载时保留仍被引用的文件，最后一个归属负责清理。未知同名文件或无法验证归属时返回冲突，不接管其他客户端的内容。

远程 skills 源可能覆盖本地同名 ID，适配器返回 `remote_skill_source`，要求先取得当前客户端的有效来源证据，不下载或更改远程 Skill、不修改全局发现规则来完成安装。

## 原生隔离检查

本批使用官方 macOS arm64 v2.3.2 executable，临时 HOME 与 XDG 根，关闭模型目录获取和自动更新；未启动模型 turn。系统 managed 根不由 CLI 的 HOME 覆盖，启动前单独确认该测试机不存在生效的管理源。

官方 `debug paths` 确认实际数据位置；持久私有 loopback `serve` 提供 `/api/config`、`/api/mcp`、`/api/skill`。服务初始插件加载是异步过程，刚启动的单次 mcp list 可能暂时为空；诊断等待同一个已启动进程的连接状态后读取，不创建模型会话。

原生 MCP 状态为 connected，原生日志记录发现的工具数；Skill 接口返回实际 ID、文件路径及解析后的正文。该 HTTP 状态接口不提供完整工具 Schema。真实 CLI 结果不外推为 Desktop UI、其他操作系统、自然语言自主触发或收费图片验收。
