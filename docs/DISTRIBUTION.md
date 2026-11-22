# 安装与版本管理

## 快速安装（Mac M 系列）

在终端选择一个客户端运行。安装器固定下载 [GitHub Release `v0.1.0-wb1.6`](https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/tag/v0.1.0-wb1.6) 的原生程序，先校验归档，再调用现有 `setup` 验签安装，最后注册 MCP 与 Skill。

Codex App / CLI：

```sh
curl -fsSL https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/download/v0.1.0-wb1.6/install.sh | sh -s -- --harness codex
```

WorkBuddy AI 5.11.4：

```sh
curl -fsSL https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/download/v0.1.0-wb1.6/install.sh | sh -s -- --harness workbuddy
```

安装脚本源码见 [`packaging/install.sh`](../packaging/install.sh)。它只使用 macOS 自带工具，不需要 Node、Rust 或 Python，不更改 PATH 或编程 Provider。已有同版安装会复用；其他版本或待恢复安装会给出明确提示，继续使用原版本管理入口。

Codex 自动查找 PATH 中的 CLI，再查标准应用目录内已支持的 App CLI 布局；放在其他位置时加 `--client "/完整路径/codex"`。WorkBuddy 使用原生应用发现；自定义目录加 `--workbuddy-app "/完整路径/WorkBuddy AI.app"`。

命令成功表示程序与接入已注册，WorkBuddy 可能仍为 `awaiting_confirmation`。重新打开客户端，WorkBuddy 首次接入先完成该连接器的原生信任；再新建任务，发送“配置 Tokemula 图片服务”，通过 `configure_tokemula` 返回的本机入口填写 Key 和默认设置。系统及客户端信任由用户在原生窗口确认。安装脚本不读取 Key、不打开配置页面，也不生成或编辑图片。

## 升级并保留设置

<a id="从-wb13-升级"></a>

从先前 Mac 测试版升级时，先用旧版管理器默认卸载，Key、默认模型、尺寸和画质会保留：

```sh
"$HOME/.local/share/tokemula-image-connector/bin/tokemula-image" uninstall
```

然后运行上方对应客户端的新安装命令。两个客户端都使用时，分别运行 Codex 和 WorkBuddy 的安装命令；WorkBuddy 按提示重新确认连接器信任。桌面 Tokemula-Images 中的原图和其他客户端配置保持原位。

## 从压缩包手动安装

解压后在包目录运行：

```sh
./tokemula-image setup --source . --trust-key /absolute/trust-public-key.txt
```

`setup` 部署本产品程序，与 `install --harness ...` 注册接入分开。默认安装根为用户 `.local/share/tokemula-image-connector`；可指定 `--install-root /absolute/owned/root`。程序返回可直接运行的稳定绝对路径，不修改 Shell 或全局 PATH，原解压目录可移走。

```sh
/absolute/installed/root/bin/tokemula-image install --harness codex --scope user
/absolute/installed/root/bin/tokemula-image configure --terminal
```

App 配置可使用 `configure --browser` 或 Harness 的 `configure_tokemula` 链接。Key 不放在参数、聊天或 MCP 参数中。完整管理命令见[管理指南](MANAGEMENT.md)。

## 检查、确认升级与独立恢复

```sh
/absolute/installed/root/bin/tokemula-image update check
/absolute/installed/root/bin/tokemula-image update apply --confirm
```

检查不会安装；没有更新渠道或信任根时准确显示尚未配置。内测只使用 loopback 更新源和隔离测试信任，正式签名身份/公钥/渠道另行准备。两个程序、Skill 和配套资源作为同一发行版切换，旧 MCP 完成已有请求后重连使用新版，不重发任务。

若新版管理程序无法启动，独立恢复入口不经过当前版：

```sh
/absolute/installed/root/recover recover --install-root /absolute/installed/root
```

它使用保留的已验证程序恢复上一一致版本。受管配置仍须匹配原归属；用户改过的内容返回冲突，不能用旧整份快照覆盖用户后加配置。

## 产品卸载与重装

```sh
/absolute/installed/root/bin/tokemula-image uninstall
/absolute/installed/root/bin/tokemula-image uninstall --purge --confirm
```

默认保留系统 Key、默认/备用模型及任务恢复引用；彻底清理是明确选择。先移除仍归本产品的外部接入，再删自身程序与所需归属资料。客户端缺失、用户改动冲突或移除失败时保留管理/恢复入口；桌面 Tokemula-Images 中的原图、用户另存图片、其他客户端和用户新增文件均保留，使用 purge 也不删除图片。


程序文件清理受阻时，结果为 `uninstall_incomplete` 并给出 `resume_command`；版本目录外的独立控制程序会保留。排除报告的具体文件故障后，复制返回的命令继续：

```sh
"/absolute/installed/root/.uninstall-recovery/tokemula-image" recover --install-root "/absolute/installed/root"
```

它继续先前已授权的程序清理，不再读取 Key 或重复外部接入操作，控制程序自身最后移除。

## 构建与发行边界

维护者入口为[packaging/README.md](../packaging/README.md)，包含真实版本构建、第三方许可、包清单和 Ed25519 签名。Windows/Linux 可在对应原生环境使用同一构建入口和相应 Rust target；当前完整实机生命周期验收目标为 macOS arm64，不能把交叉构建或静态设计写成其他系统通过。

当前内部测试信任与正式 Developer ID、公证、Windows 代码签名和长期更新密钥分别验收；不要求关闭系统防护。生产接口、真实 Key 与图片调用按各自环境授权执行；DeepSeek Desktop、其他系统与正式发布分别验收。

## 开发构建与验证

发布包由 [`packaging/package.py`](../packaging/package.py) 在隔离副本中构建，并为副本 Skill 注入 `metadata.internal_build`；仓库 Skill 保留业务原文，具体流程见[打包说明](../packaging/README.md)。

固定工具链为 **Rust 1.100.0**，项目 `rust-toolchain.toml` 同时声明 `rustfmt` 和 `clippy`，无需改变全局默认工具链。HTTP/MCP使用 `rmcp 3.11.0`、Tokio、reqwest、schemars、base64和image；必要图片缓存使用tempfile，适配器使用toml_edit/JSONC CST保留格式、serde_yaml_ng读取原生配置、sha2核对WorkBuddy原生审批，测试使用loopback Axum；实际版本保存在根 `Cargo.lock`。

使用 `--no-modify-path` 安装 rustup 后，先在当前终端设置 PATH；这不会修改 Shell 配置或全局默认工具链：

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

首次准备工具链及依赖（需要网络）：

```sh
rustup toolchain install 1.100.0 --profile minimal --component rustfmt --component clippy
cargo fetch --locked
```

之后在仓库根目录运行离线检查：

```sh
cargo build --workspace --locked --offline
cargo test --workspace --locked --offline
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
```

工程不依赖主站 checkout、数据库或真实凭据。复制本仓库必要文件到临时目录后即可用相同工具链与已准备的 Cargo 缓存独立检查。B1测试只处理合成数据；B2测试仅连接loopback HTTP fixture，API与图片下载client均禁用代理。实际binary测试清空继承环境，不调用真实服务。

跨项目联测使用真实服务端 handler 与鉴权产生的合成响应，由以下显式测试通过 loopback HTTP 消费。正常构建和测试不需要该文件或主站源码：

```sh
TOKEMULA_IMAGE_CATALOG_FIXTURE=/absolute/path/synthetic-catalog.json \
  cargo test --lib --locked --offline \
  http::tests::server_generated_catalog_crosses_http_boundary -- --ignored --exact
```

单异步跨项目检查消费 S2b 的历史 Go 任务产物，经 loopback HTTP → `get_image_task` → MCP 验证三种终态和原引用。测试只把合成图片 URL 改绑本地下载服务；任务 GET 响应头故意使用不同引用：

```sh
TOKEMULA_IMAGE_TASK_FIXTURE=/absolute/path/S2b-go-async-billing.json \
  cargo test --lib --locked --offline \
  http::tests::tasks::server_generated_tasks_cross_http_and_mcp_with_original_references -- --ignored --exact
```

批量跨项目检查消费真实 Go handler/service/download 产物，验证任务状态、展开ID、数字分页和指定图片：

```sh
TOKEMULA_B3B_GO_ARTIFACT=/absolute/path/B3b-go-batch.json \
  cargo test --lib --locked --offline \
  http::tests::batches::server_generated_batch_dtos_cross_http_mcp_with_original_states_pages_and_content -- --ignored --exact
```

高级编辑联测消费真实 Go 目录/解析/转发产物，并可导出实际 Rust multipart 回送 Go 检查顺序、mask 和高级字段：

```sh
TOKEMULA_B3C_GO_ARTIFACT=/absolute/path/B3c-C1-go-advanced.json \
  cargo test --lib --locked --offline \
  http::tests::advanced::server_generated_advanced_catalog_and_parser_inputs_cross_http_mcp_and_export_wire -- --ignored --exact
```

正常构建不需要 Go、服务端目录或外部 fixture。分发用户不需要 Rust、Node 或 Python；Harness 自身运行时沿其原契约。正式生产端点、真实 Key/收费、其他系统原生生命周期和正式签名发布分别验收。
