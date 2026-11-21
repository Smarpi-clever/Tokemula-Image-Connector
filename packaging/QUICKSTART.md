# Tokemula Image Connector 内测包

此包包含原生管理程序 `tokemula-image`、独立 MCP 程序及已内嵌的页面、Skill 和插件资产。运行本产品不需要 Rust、Node 或 Python；Harness 自身的运行环境沿其原契约。

## 安装程序，再注册 Harness

解压到用户有权限的目录，在该目录运行：

```sh
./tokemula-image --build-info
./tokemula-image setup --source . --trust-key /absolute/path/test-trust-public-key.txt
```

`test-trust-public-key.txt` 是本批内测材料，不是正式发布方身份或产品内置信任根。请与本批分发记录一起取得它；不要用下载包自带的任意公钥替代可信来源。本包未使用正式 Developer ID、公证或长期更新签名材料，正式分发另行安排。不需要关闭系统安全保护。

安装完成后命令会返回实际稳定路径。默认 macOS 用户位置为：

```sh
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" install --harness codex --scope user
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" configure --terminal
```

App 用户可使用 `configure --browser`，或由 Harness 的 `configure_tokemula` 取得本机连接链接。API Key 只在隐藏输入终端或短时本机页面输入，不放在命令参数、聊天或 MCP 参数中。正式图片目录接口尚待生产发布时，配置会准确报告尚未可用，不以试生图验证。

首次连接时选择默认模型、备用顺序，以及对应操作和模式的尺寸、画质。已有 Key 后修改偏好可直接使用已保存的 Key。尺寸显示像素和比例；支持的模型路径提供自定义宽高，批量保留 `1K` 等分辨率等级。未设置时使用自动／模型默认。

之后在 Harness 直接描述图片即可；本次明确的尺寸、画质优先于默认。图片思考复杂度在支持的路径默认 `medium`，单次可指定 `low/medium/high`；该参数与画质、Harness 的语言模型推理强度分别设置。

`setup` 安装本产品程序，`install/remove` 仍只管理指定 Harness 接入。安装不替用户批准 WorkBuddy 或项目信任。显式安装根可使用 `setup --install-root /absolute/owned/root`；本产品不修改 Shell 启动文件或全局 PATH。安装后可以移走原解压目录。

## 检查、确认更新与恢复

```sh
/absolute/installed/root/bin/tokemula-image update check --install-root /absolute/installed/root
/absolute/installed/root/bin/tokemula-image update apply --install-root /absolute/installed/root --confirm
```

没有更新渠道或信任根时显示尚未配置。检查不自动安装，正式更新源尚未启用；本批仅使用 loopback 内测源。两个程序和资产一起切换。已运行旧 MCP 可以完成已有请求；重连后使用新版，不重发收费请求。

保留的上一版本提供独立恢复入口。若新程序无法运行，使用安装结果指出的已验证旧程序绝对路径：

```sh
/absolute/installed/root/recover recover --install-root /absolute/installed/root
```

恢复使用原受管安装资料，不把旧整份 Harness 配置覆盖回去。出现用户修改冲突或客户端不可用时，按具体状态处理，保留管理和恢复程序再继续。

## 两种产品卸载

```sh
/absolute/installed/root/bin/tokemula-image uninstall --install-root /absolute/installed/root
/absolute/installed/root/bin/tokemula-image uninstall --install-root /absolute/installed/root --purge --confirm
```

默认保留系统 Key、模型与出图偏好和任务恢复引用，便于重装。`--purge --confirm` 才删除本产品凭据与设置。先清理仍归本产品的外部接入，再移除程序；部分失败保留恢复入口。用户另存图片、其他客户端数据和用户新增文件保留。


程序文件清理受阻时，结果为 `uninstall_incomplete` 并给出 `resume_command`；版本目录外的独立控制程序会保留。排除报告的具体文件故障后，复制返回的命令继续：

```sh
"/absolute/installed/root/.uninstall-recovery/tokemula-image" recover --install-root "/absolute/installed/root"
```

它继续先前已授权的程序清理，不再读取 Key 或重复外部接入操作，控制程序自身最后移除。

## 包完整性与平台

`release.json` 对本包普通负载文件列出大小与 SHA-256，`release.sig` 是该 JSON 原始字节的 Ed25519 签名。包外 `.sha256` 供传输校验，不能代替发布者身份验证。`LICENSE` 与 `THIRD-PARTY/NOTICE.md` 及其许可文本随包保留。

本批实际目标为 macOS arm64。Windows/Linux 保留后续原生构建与生命周期入口，尚未作为本机实测通过。DeepSeek Desktop、真实收费、正式签名、公证、更新渠道及发布均另行验收。

费用请在 [Tokemula 网站](https://tokemula.com)查看；Connector专注生成、编辑和恢复图片结果。
