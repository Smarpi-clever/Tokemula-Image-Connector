# 首次连接与日常管理

先从[分发包完成 setup](DISTRIBUTION.md)。默认安装不修改 PATH，后文的 `tokemula-image` 均表示安装结果给出的绝对管理路径；默认 Unix 路径为 `$HOME/.local/share/tokemula-image-connector/bin/tokemula-image`。自定义安装根会由已安装程序识别，不必每次重复根目录。

在自己的交互终端运行：

```sh
tokemula-image configure --terminal
```

首次连接时输入 Key，程序只读验证图片目录，Key 不回显。已配置后可使用已保存的 Key 修改设置。随后选择默认模型、备用顺序和自动降级，再按操作与执行方式设置默认尺寸、画质，最后确认保存。所有 Harness 共用这些设置。

当前目录接口尚待生产发布时会显示“目录接口尚未可用”，原配置保留。程序不以试生图验证 Key。身份有效但余额或额度不足时，仍可选择模型并保存，当前提交限制保持。

## 使用本机连接页

```sh
tokemula-image configure --browser
```

管理程序创建限时的 loopback 页面并打开默认浏览器。已有 Key 时可直接选择模型和出图偏好，换 Key 时再填写并验证。管理命令在本次页面结束后退出，不安装后台服务。需要手动打开链接时使用 `--browser --no-open`。

在 App 的 Harness 中调用不接收秘密的 `configure_tokemula`，可取得本机链接或终端指引。未知界面同时提供两种入口，不自行弹出浏览器。不要把 Key 粘贴进聊天、MCP 参数或命令参数。

连接页关闭、取消或过期时不覆盖旧配置；成功保存后新操作使用新配置，在途图片请求保持其原凭据，不重发。页面不将 Key 写入 URL、回显、localStorage 或第三方资源。

AI 的非交互命令窗口没有可输入终端时，`configure --terminal` 只返回本机命令。请由用户在自己的终端运行，不改用管道或聊天收集 Key。

## 选择默认与备用模型

模型名称与实际 ID 来自当前 Key 的正式目录。终端使用编号选择，备用编号以逗号分隔并保留顺序；空备用列表表示没有备用。未要求的模型和参数不由 AI 猜测。

- 明确传 `model` 时精确使用该模型，不自动替换。
- 普通请求省略 `model`，先尝试默认模型。
- 只有有效目录表明首选不存在、暂不可用或不支持整次请求时，且已开启自动降级，才按用户备用顺序选择。
- 关闭降级或没有合适备用时返回原因和可选项，不从列表外另选。
- 不减少图片数量、丢参考图、去掉 mask 或修改参数来套入某个模型。
- 已发送收费请求后不再换模型提交；超时或结果未知按原引用查询。

本次降级不改默认模型，下一次仍从首选开始。批量幂等重放沿首次保存模型，后改偏好不改变同一请求。历史任务和图片查询不依赖默认模型是否可用。

## 先设置尺寸和画质，以后直接描述图片

在终端或连接页中，选择默认模型、操作和执行方式，再选择尺寸、画质并保存。生成、编辑、单任务异步和批量分别使用对应设置。未设置时显示“自动／模型默认”。

更换默认模型后，程序会列出不兼容的旧偏好。可明确确认清除这些偏好，再保存新设置；选择取消则保持原配置。请求中的自动降级继续保留全部明确要求。

- 常用像素尺寸带长宽比，例如 `1024×1024 · 1:1`、`1536×1024 · 3:2`、`1024×1536 · 2:3`。
- 支持自定义尺寸的具体路径会提供宽高输入，并显示步长、比例、边长和总像素限制。
- 批量的 `1K/2K/4K` 是分辨率等级，保留该模式的选项。画质选项来自当前能力，显示中文及实际参数值。
- “自动／模型默认”沿当前模型处理；仅在路径支持时发送 `auto`，其余保持省略。

之后在 Harness 说“生成一张咖啡店海报”即可使用已存设置。本次说“改为 1536×1024、高画质”会覆盖默认，备用模型也须满足本次尺寸、画质及全部输入要求。

图片思考复杂度使用模型自身的 `complexity`。支持该参数时，常规请求明确发送 `medium`；用户单次可选择 `low/medium/high`。页面展示当前路径是否适用，普通配置无需再选一项。画质和 Harness 语言模型的推理强度各自独立。

出图偏好保存在独立、非敏感、版本化的 `image-preferences.json`，不含 Key。路径由受管 DataPaths 的 tasks 父目录派生，默认 Unix 位置为 `$HOME/.local/state/tokemula-image-connector/image-preferences.json`；已安装程序使用安装记录中的实际数据路径。旧凭据 envelope 和模型设置保持原格式，旧版仍可读取。文件缺失使用默认；损坏或读取失败时报告错误并保留原件。保存失败或取消保留原设置，成功后只影响新请求；旧任务恢复和批量同键重放沿原输入。

## 安装、检查与移除接入

将 `tokemula-image` 与独立 `tokemula-image-mcp` 放在同一目录。安装会自动解析当前用户配置根、项目和客户端位置，注册独立程序，不注册 `cargo run`。

```sh
tokemula-image install --harness codex --scope user
tokemula-image install --harness claude --scope project --project /absolute/project
tokemula-image install --harness deepseek --profile web
tokemula-image status
tokemula-image status --harness codex --scope project --project /absolute/project
tokemula-image remove --harness codex --scope user
```

支持 `codex`、`claude`、`opencode`、`deepseek`、`workbuddy`。没有指定 Harness 的交互安装/移除会询问目标。DeepSeek 的 profile 必须已经初始化；Desktop 仍留待独立环境验收。DeepSeek 和 WorkBuddy 没有项目 MCP，指定该作用域会明确拒绝。

WorkBuddy 标准入口为 `install --harness workbuddy --scope user` 和 `status --harness workbuddy --scope user`。管理器自动发现 `WorkBuddy AI.app`（5.11.4，默认 `~/.workbuddy-ai`），并保留 `WorkBuddy.app`（5.11.3，默认 `~/.workbuddy`）；原生配置根覆盖优先，非默认应用路径可用 `--workbuddy-app` 指定。WorkBuddy 返回 `awaiting_confirmation` 时，在客户端完成原生确认后再查 `status`。安装不代替项目/客户端审批。各目标的版本、作用域、Skill共享和状态含义见 [Harness 适配组件](HARNESS-ADAPTERS.md)。

只有非默认安装或隔离验证需要显式路径：`--home` 指定 Harness 文件系统用户根（系统凭据仍由实际登录用户的原生服务管理）、`--client` 指定 Harness CLI、`--mcp` 指定独立 MCP 程序、`--project` 指定项目、`--state-dir` 指定本产品私有归属目录、`--workbuddy-app` 指定应用包。HOME、CODEX_HOME、CLAUDE_CONFIG_DIR、XDG、DSH_HOME 和 WorkBuddy 原生覆盖继续按其有效值解析；没有任意 API 网关或凭据条目选项。

移除只处理指定接入。其他 Harness、共享 Key、模型偏好和用户后来添加的配置保持。用户修改过受管项时返回冲突；不把旧快照整份覆盖回来。

## 查看来源或退出绑定

```sh
tokemula-image status
tokemula-image logout
```

`status` 显示当前实际来源、凭据可用状态、模型与出图偏好、安装状态和待确认事项，不显示 Key。显式 `TOKEMULA_IMAGE_KEY`（包括空值）优先于系统存储；该开发路径不读取系统 Key，也不应用已存偏好。它不会自动导入系统库。

`logout` 只删除本产品保存的 Key，保留非秘密偏好和所有接入。若当前终端仍设置环境覆盖，状态会继续显示环境来源；清除该环境变量后，后续操作不再使用旧 Key 缓存。

系统凭据库锁定时，解锁后重试。首次 store 初始化失败且被当前进程缓存时，恢复系统服务后重启当前配置进程；程序会区分这两种状态，不退回明文文件。保存失败保留上一份可用配置；显示部分保存状态时按提示重新配置，不重复提交图片。

本批真实检查限于本机隔离目录、专用合成系统凭据条目和 loopback 服务。其他系统、自然语言自主触发、真实 Key/收费、App UI及系统安装包另行验收。

## 产品程序与接入是两层管理

`install/remove` 继续只处理指定 Harness；`setup/uninstall` 处理本产品成套二进制，不能混用。

首次从已解压包运行 `./tokemula-image setup --source "$PWD" --trust-key /absolute/trusted-public-key.txt`。使用包发布方提供、已确认可信的公钥；本轮内部测试公钥不是正式产品信任根。可用 `--install-root` 指定用户有权限的持久目录，`--adapter-state` 仅用于非默认归属根。程序输出稳定管理/MCP路径，不写Shell或PATH；可移走原解压目录。

`health`只读检查实际内嵌资源消费者；`--build-info`分别显示两程序实际版本/目标。页面、Skill和插件资源已内嵌，不依赖开发checkout或语言工具链。

## 检查、升级和恢复

```sh
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" update check
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" update apply --confirm
```

自定义根用setup输出的绝对路径替换。未配置可信渠道时返回not_configured，不显示已是最新；没有正式更新源时不会自动启用公开网络源。本批只用显式配置的loopback测试源与测试公钥。自动检查借现有进程最多每24小时一次，失败不阻塞配置或图片请求，MCPstdout仍仅协议；安装升级需用户明确发起并确认。

程序先验证签名、版本/目标、包校验和与安全归档；复制到私有stage后再完整核对签名、全部文件hash及同一清单，才执行候选启动/八工具协议检查，然后通过单个current指针切换管理与MCP成套版本。旧MCP可完成在途调用，不被强杀或重发；重连后使用新版。文件占用或操作未完成会保留旧版/恢复材料并给出状态。

上一验证程序保留为独立 `recover` 入口，即使新版管理程序不能启动也可运行：

```sh
"$HOME/.local/share/tokemula-image-connector/recover" recover
```

也可显式 `--install-root`，从保留的旧程序或原分发包执行恢复。事务journal覆盖实际指针切换，恢复重复操作不依赖损坏新程序。源码、凭据/偏好和任务格式不在升级时迁移。

稳定MCP路径减少外部配置变化；受管Skill由目标版本自身更新，rollback使用旧版本自身资源。旧归属缺管理上下文时，按原安装位置运行 `update integration --harness ... --scope ...` 并补原必要路径/profile，合法匹配才更新并补记录；原install不会因此扩大权限。用户修改过的内容仍返回冲突，不卸载全部Harness重装，也不覆盖整份旧用户配置。

## 产品卸载与重装

```sh
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" uninstall
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" uninstall --purge --confirm
```

默认移除本产品程序、仍归本产品的接入和可重建缓存，保留系统Key、默认/备用及出图偏好、任务恢复记录；重新setup和接入后继续使用。明确purge才再删除本产品凭据、设置和可确认归属的恢复记录，回到未配置状态。新增偏好文件也只按准确路径和完整格式确认归属；任务记录清理要求完整原格式及准确的规范文件名，同名文本、带未知用户扩展或非规范JSON保留。

先清外部接入，再清归属资料和程序。外部客户端缺失或用户修改受管项时，保留原管理/恢复程序；处理具体冲突后重试原uninstall。

进入程序文件清理后，独立管理副本位于安装根`.uninstall-recovery/tokemula-image`，不属于被删除的版本集合。某项unlink失败会返回非零退出、uninstall_incomplete和绝对resume_command；已删文件不会让后续校验卡死，剩余文件仍根据独立保留的签名清单验证。排除具体文件故障后复制运行返回命令，例如：

```sh
"$HOME/.local/share/tokemula-image-connector/.uninstall-recovery/tokemula-image" recover --install-root "$HOME/.local/share/tokemula-image-connector"
```

此时recover继续先前已授权的程序清理，不再读取Key、重复外部配置或再次做凭据清理；它不是对已删除接入的整份配置回写。该恢复副本最后才清理。用户主动另存图片、其他客户端资料、安装后新增文件保持，所属空目录以外不递归删除；凭据操作只针对本产品条目。

macOS arm64 的实际包/生命周期证据由私有D2-D3验收记录汇总。Linux可共用Unix布局但未实机验证；Windows成套替换/占用与自删除仍需原生实现验收，当前不把Unix软链接方案套为已支持，也不缩减最终三平台目标。
