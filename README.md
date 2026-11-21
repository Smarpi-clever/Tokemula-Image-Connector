# Tokemula Image Connector

让 Codex、WorkBuddy 通过标准 stdio MCP 调用 Tokemula 生成和编辑图片。Rust 原生程序，配套 Skill；Key 和默认设置在本机保存，多客户端共用。

## 快速开始

当前下载版 [**0.1.0-wb1.5**](https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/tag/v0.1.0-wb1.5) 支持 **macOS Apple Silicon（M 系列）**。先安装 Codex App / CLI 或 WorkBuddy AI 5.11.4，再打开终端运行对应命令。

### 1. 安装并接入客户端

**Codex App / CLI**

```sh
curl -fsSL https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/download/v0.1.0-wb1.5/install.sh | sh -s -- --harness codex
```

**WorkBuddy**

```sh
curl -fsSL https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/download/v0.1.0-wb1.5/install.sh | sh -s -- --harness workbuddy
```

命令会下载原生程序、自动校验、安装 MCP 与 Skill，并保留其他 MCP、编程 Provider 和已有设置。无需 Node、Rust 或 Python。安装脚本源码见 [`packaging/install.sh`](packaging/install.sh)。

### 2. 连接自己的 Key

重新打开客户端。WorkBuddy 首次接入时，先到 **专家·技能·连接器 → 连接器 → 自定义连接器**，对 `tokemula-image` 点击 **信任**。系统授权在 macOS 原生窗口完成。

然后新建任务，发送：

> 配置 Tokemula 图片服务。

AI 调用 `configure_tokemula` 后，打开返回的本机页面，填写自己的图片 Key，选择默认模型、尺寸和画质，再保存。Key 只填在本机页面，已有设置继续沿用。

### 3. 开始使用

> 用 Tokemula 生成一张简洁插画：白色桌面上一盆仙人掌，旁边一个浅蓝色杯子，米白背景，无文字。完成后展示并提供可保存的原图。

原图会自动保存在桌面的 **Tokemula-Images** 文件夹。WorkBuddy 在聊天正文直接显示本次原图，同时自动打开原生预览，并保留文件入口；Codex 沿用其图片展示方式并保留原图路径。

继续编辑时直接发送：

> 用 Tokemula 编辑刚才那张图片：保留主体和构图，把背景改成浅桃色，在桌面左前方添加一片小绿叶，只输出一张。

日常省略模型、尺寸和画质即可沿用默认设置。编辑结果另存新文件，不覆盖原图。图片长期保留，重启、升级和卸载都不会删除；回看旧聊天时，保持原文件的位置和名称。编辑其他已有图片时，可上传原图或提供完整路径。

请求结果未知时，保留原引用，不重复提交。文件被移动或删除时报告路径失效，不拿旧图替代。

## 客户端与配置

- [Codex App / CLI 教程](https://docs.tokemula.com/image-connector/codex)
- [WorkBuddy 教程](https://docs.tokemula.com/image-connector/workbuddy)
- [其他 Harness 适配说明](docs/HARNESS-ADAPTERS.md)

<details>
<summary>标准 MCP 配置参考</summary>

上面的安装命令已完成注册。核对配置时，将示例中的用户目录换成实际路径，保留其他配置条目。

Codex 的 `config.toml`：

```toml
[mcp_servers.tokemula-image]
command = "/Users/你的用户名/.local/share/tokemula-image-connector/bin/tokemula-image-mcp"
```

WorkBuddy 的 `mcp.json`：

```json
{
  "mcpServers": {
    "tokemula-image": {
      "command": "/Users/你的用户名/.local/share/tokemula-image-connector/bin/tokemula-image-mcp"
    }
  }
}
```

</details>

## 能力与文档

- 生成、原图编辑、多参考图及模型声明的高级参数。
- 模型目录、默认/备用模型、按操作保存的尺寸与画质偏好。
- 同步生成与编辑、目录开放的异步/批量能力及原任务引用恢复；可用模式以当前 Key 的模型目录为准。
- 八个工具的参数由 MCP 工具发现提供，费用和额度在 [Tokemula 网站](https://tokemula.com/)查看。

[设置与日常管理](docs/MANAGEMENT.md) · [安装、更新、卸载与恢复](docs/DISTRIBUTION.md) · [HTTP 与图片交付](docs/HTTP-DEVELOPMENT-CONTRACT.md) · [批量任务](docs/IMAGE-BATCH-CONTRACT.md)

## 常见问题

- **App 放在其他位置？** 安装命令支持 `--client` 和 `--workbuddy-app`，用法见[安装说明](docs/DISTRIBUTION.md#快速安装mac-m-系列)。
- **安装完成，客户端还没连接？** 重启客户端并新建任务，检查该 MCP 已启用；WorkBuddy 先完成“信任”。
- **macOS 提示无法验证开发者？** 在系统设置的“隐私与安全性”中正常确认对应程序，见[Mac 教程](https://docs.tokemula.com/image-connector/)。当前包使用产品测试签名，Apple Developer ID 与公证尚待完成。
- **已有安装？** 同版会复用；旧版的保设置升级步骤见[版本管理](docs/DISTRIBUTION.md#升级并保留设置)。

## 开发

使用仓库锁定的 Rust 工具链和 `Cargo.lock`，见[开发构建与验证](docs/DISTRIBUTION.md#开发构建与验证)。安装器边界测试可运行 `python3 -m unittest discover -s packaging -p test_install.py`。

原有离线库接口及兼容数据含义保留，见[离线库说明](docs/HTTP-DEVELOPMENT-CONTRACT.md#b1-离线兼容数据结构)。发行维护入口见[打包说明](packaging/README.md)。

## 许可证

[Apache-2.0](LICENSE)。发布包包含第三方许可说明。
