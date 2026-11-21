# Tokemula Image Connector · Mac 测试版

适用于 macOS Apple Silicon（M 系列）。包内是两个原生程序及内嵌的配置页面、Skill 和插件；使用本产品不需要 Rust、Node 或 Python。

## 已有安装

先用旧版管理器默认卸载，保留 Key 和默认设置，再按下方步骤安装本包：

```sh
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" uninstall
```

默认卸载保留 Key、默认模型、尺寸、画质和任务引用。安装本包后，为原来使用的客户端重新注册；WorkBuddy 按提示确认信任。不要添加 `--purge`。

## 安装程序并接入客户端

从[本版 Release](https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/tag/v0.1.0-wb1.5)下载配套的 `trust-public-key.txt`。解压程序包，在终端进入解压目录，再运行：

```sh
./tokemula-image --build-info
./tokemula-image setup --source . --trust-key "/完整路径/trust-public-key.txt"
```

`setup` 会验证包的 Ed25519 签名并返回稳定程序路径。这个 Mac 测试版尚未进行 Apple Developer ID 签名或公证；首次运行时按 macOS 提示正常确认单个程序，不关闭全局系统保护。

根据使用的客户端选择命令；两个客户端都用时分别运行。

**Codex：** 先按[CLI 定位步骤](https://docs.tokemula.com/image-connector/codex#connect)找到 App 内置或已安装的 `codex` 程序，将第一行双引号中的示例换成实际完整路径：

```sh
CODEX_CLIENT="/在这里粘贴codex程序的完整路径"
"$CODEX_CLIENT" --version
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" install --harness codex --scope user --client "$CODEX_CLIENT"
```

**WorkBuddy：**

```sh
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" install --harness workbuddy --scope user
```

`setup` 安装程序，`install/remove` 管理指定客户端接入。安装不修改编程 Provider、Shell 启动文件或 PATH；原解压目录可以移走。安装完成后正常重载客户端，WorkBuddy 按提示信任 `tokemula-image`。成功注册与已连接分别以管理器状态及客户端连接状态确认。

## 首次配置与使用

首次使用时运行以下命令，在本机页面填入自己的 Key，并选择默认/备用模型、尺寸和画质：

```sh
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" configure --browser
```

终端用户也可用 `configure --terminal` 隐藏输入 Key；客户端中的 `configure_tokemula` 会提供同一本机入口。Key 只在这些入口输入，不放入聊天、命令参数或 MCP 配置。已有 Key 和默认设置的用户直接沿用。

之后在客户端直接描述图片，例如：“用 Tokemula 生成一盆白色桌面上的仙人掌，旁边一个浅蓝色杯子，米白背景，无文字，只要一张。”日常省略模型、尺寸、画质和复杂度；明确指定的出图参数优先于默认。

生成和编辑的完整原图默认保存在当前用户桌面的 `Tokemula-Images` 文件夹，格式、尺寸和原始字节保持。文件不会自动过期，重启、升级及卸载（包括 `purge`）都保留这些图片；编辑会另存新图，不覆盖原图。

WorkBuddy 收到本次原图后，配套 Skill 调用原生展示入口，并把同一张原图内嵌到聊天正文，保留可回看的原文件入口。Codex 使用原有图片展示方式。继续编辑刚才的图时直接描述修改；其他已有图片可拖入或提供绝对路径。

旧聊天引用原文件位置。移动、改名或删除文件后，对应入口会失效；保留原位置即可继续回看。路径失效、展示失败或提交结果不明时报告当前状态并保留请求引用，不重新生成，也不从内部会话数据找图。

首次使用如 macOS 请求桌面访问，在系统窗口确认对应程序。目录不可写时先处理该目录的访问权限，再提交图片请求。

## 状态、偏好与断开

```sh
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" status
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" configure --browser
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" remove --harness workbuddy --scope user
```

`remove` 只断开指定客户端，保留共享 Key 和其他接入。完整步骤见[管理指南](https://github.com/Smarpi-clever/Tokemula-Image-Connector/blob/main/docs/MANAGEMENT.md)。

## 更新、恢复和卸载

```sh
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" update check
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" update apply --confirm
```

检查不安装；当前没有自动更新渠道时显示 `not_configured`。同一受信渠道的更新会成套切换程序和资源，旧 MCP 完成已有请求后重连使用新版。需要恢复时使用管理器返回的独立恢复命令；出现冲突按具体状态处理，保留现有管理和恢复入口。

```sh
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" uninstall
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" uninstall --purge --confirm
```

默认卸载保留 Key、偏好和任务引用；明确使用 `--purge --confirm` 才清理这些设置。桌面 `Tokemula-Images` 中的原图、用户另存图片和其他客户端数据保留。卸载返回 `uninstall_incomplete` 时，使用其返回的 `resume_command` 继续已授权操作。

## 完整性与支持范围

`release.json` 列出普通负载文件的大小及 SHA-256，`release.sig` 是其 Ed25519 签名；包外 `.sha256` 用于下载完整性校验。`LICENSE`、`THIRD-PARTY/NOTICE.md` 和第三方许可随包保留。

本版仅发布已验收的 macOS arm64。其他系统、DeepSeek Desktop、Apple 签名和公证分别安排。费用请到 [Tokemula 网站](https://tokemula.com)查看。
