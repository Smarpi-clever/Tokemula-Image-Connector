# HTTP 传输与同步交付契约

目录采用 [正式图片能力契约](IMAGE-CATALOG-CONTRACT.md)，版本 `schema_version="1"`。本文件说明同步生成/编辑、图片交付和任务引用恢复。费用在 [Tokemula 网站](https://tokemula.com)查看，Connector不查询或展示费用。

## 三层边界

B1 的 `Catalog`、`SelectionRequest`、`FeeReceipt` 继续供离线调用者直接使用，无网络、认证或正式 schema 前置。HTTP 层消费正式目录，用不影响权限的内部 unknown 价格投影复用 B1 选择，对外只输出能力与提交限制。`b2-dev-1` 目录不再接受；原 B1 fixture 保留独立语义。

服务基地址固定为 `https://api.tokemula.com/v1`。测试仅在库内部构造时注入 loopback 地址，MCP 工具参数没有地址或认证字段。原 `ImageClient::new(Some/None)` 保持显式注入条件，不读取设置。独立 MCP 的管理路径在每次操作开始时读取一份系统凭据与模型偏好快照，同次目录、提交及任务读取共用该身份。显式 `TOKEMULA_IMAGE_KEY` 环境覆盖（包括空值）优先，沿原开发入口执行，不读取或导入系统凭据，也不应用保存的偏好。无凭据仍可发现工具，不读取 `.env`、其他模型 Key 或主站配置。

模型列表及生成、编辑、任务结果不输出服务端价格、预估费用或实际金额。服务端账务和兼容费用接口保持原契约，客户端不调用该费用接口。

## 同步生成和编辑

- `POST /images/generations`：Bearer 认证和 JSON；字段为 `model`、`prompt`、`n`，可选 `size`、`quality`、`complexity`。编辑的 multipart 使用相同参数名，`single_async` 复用同步解析及上游转发。
- `POST /images/edits`：Bearer 认证和 multipart；旧 `image_path` 单图使用 `image` 文件部分，多图 `image_paths` 按输入顺序发送重复 `image[]`。两种路径输入二选一，只读取明确指定的绝对路径。每张参考图最多 20 MiB、64,000,000 像素，并遵守所选能力的数量、字节和实际 PNG/JPEG/WebP MIME 限制；原图字节保持。
- 每个 POST 前重新读取当前目录。基础能力复用 B1 选择，编辑再按整次请求的所有图片、mask、参数和完整请求体选择；显式模型保持，失败时提供适用替代项。实际 JSON 字节或 reqwest 生成的完整 multipart Content-Length 必须符合目录的 `max_request_bytes`，包含全部文件和字段/边界开销；等于上限可提交，超出时零 POST。禁止自动重试和 POST 重定向；连接与整体请求均有限时。

### 多参考图、mask 与高级参数

尺寸和画质先按“单次参数 → 对应操作/模式的已存偏好 → 模型默认”解析，再对每个候选验证完整请求。`complexity` 接受 `low/medium/high`；目录声明支持时默认明确发送 `medium`，未声明时省略隐式默认并在提交前拒绝显式值。自定义尺寸只在目录含 `size_rules` 时按步长、比例、像素和边长校验。同步、单异步、JSON 和 multipart 使用同一套判断；批量沿自身等级尺寸能力，不新增未经声明的画质或复杂度字段。

`edit_image` 在 sync 和 single_async 中接受同一编辑参数。多参考图沿 `input_limits.max_images`，新增可选 `edit_options` 声明 mask 和高级参数；没有该字段的旧目录继续支持原基础编辑。

- `mask_path` 上传为 `mask`，按目录作用于第一张参考图。当前规则为 PNG、具有 alpha 通道、尺寸与第一图一致，大小小于 4 MiB（最多 4,194,303 字节）；mask 不占参考图数量，但独立受其字节上限和整个请求体限制。客户端不裁剪、重绘或改变原图。
- `background/output_format/output_compression/input_fidelity` 全部默认不发送，仅接受目录对应允许值。透明背景只配合声明的 PNG/WebP 格式；省略 output_format 时按服务端 PNG 默认检查组合，wire 不补该字段。压缩参数须显式选择允许压缩的 JPEG/WebP，范围沿目录的 0–100。
- 当前 OpenAI 路径按实际账号与回退路径投影：OAuth 可能回退到省略 input_fidelity 的 Responses，因此相应模型不声明该参数。Grok 保留多图编辑，未声明 mask 或这四项高级参数。详细路线与规范来源见[目录契约](IMAGE-CATALOG-CONTRACT.md)。

成功图片响应的HTTP读取预算按本次已通过能力校验的n计算：每张20MiB原图的base64上界（4×ceil(bytes/3)），另加每张16KiB记录开销和整体1MiB JSON元信息余量。目录和错误体继续各自限制为64MiB；不会把图片数量变化放宽到这些非图片响应。单图解码后仍校验20MiB与像素上限。

成功响应沿现有 Images 格式：`data` 数组，每项是 `b64_json` 或 `url`（二选一），可带 `expires_at`（Unix 秒，未给则远程期限未知）。HTTP 头 `X-Client-Request-ID` 是关联引用。可选顶层 `model` 表示服务端提供的客户可见实际模型，不自行用选择值冒充该信息。

返回 MCP image 与结构化元信息（selected_model、effective_model/source、状态与request_ref），图片格式由实际内容验证。URL 用独立、无 Authorization 的 HTTP client 下载，下载重定向同样不携带 API 凭据；URL 不写入工具诊断。小图保持原始inline。按SDK官方响应类型序列化后，含request ID、所有图片及元信息的消息预算为8MiB（低于默认16MiB传输上限）；超出时按最大图片逐个改为256像素内的PNG预览，完整原图另存至当前用户私有缓存，直到整条响应符合预算。文本和 `local_files[].inline_representation="preview"` 明确区分预览/原图；每条local_files含image_index、绝对path、原图mime_type及expires_at。原图保留24小时，启动或下次工具操作清理本产品命名的过期普通文件；不删输入、无关文件或链接目标，不运行后台清理。Unix目录0700、文件0600；Windows使用当前用户LocalAppData缓存的继承访问控制。输入20MiB和像素支持范围保持不变。远程到期时间单列，未知为null；本地缓存期限不推定远程URL期限。

### MCP Apps 内嵌图片视图

客户端协商 `capabilities.extensions["io.modelcontextprotocol/ui"].mimeTypes` 包含 `text/html;profile=mcp-app` 时，生成、编辑、单异步查询和批量查询四个工具通过 `_meta.ui.resourceUri` 关联固定资源 `ui://tokemula/image-viewer`。它是无外部依赖的静态 HTML，图片直接来自本次 `ui/notifications/tool-result`，不经语言模型转抄、不先写图片文件。八个工具名、参数、默认选择、上游调用和错误/恢复引用保持。

符合 8 MiB 消息预算的结果继续返回原始 `ImageContent`。超预算时，协议 image 块明确标为预览，本次原图只暂存进程内存；结果 `_meta["tokemula/inlineImages"]` 按 `image_index` 给出独有资源 URI、原始 MIME、字节数及分块数。视图通过同一 Server 的标准 `resources/read` 顺序读取每块最多 3 MiB 的原始字节，再在浏览器内存显示完整图片；每条资源响应也低于 8 MiB。此路径不调用图片文件缓存。内存资源从结果准备起保留 10 分钟，进程退出即结束；过期只报告状态，不重发请求。新结果、失败、无图或取消时清除当前视图，迟到的旧资源响应不会替换本次结果。

“保存图片”只在用户点击时，以原始 MIME 与原始字节调用标准 `ui/download-file` 的内联 `EmbeddedResource`，交给宿主选择保存位置。宿主未声明相应资源读取或下载能力时，视图显示缺少的入口。未协商 MCP Apps 的客户端沿用上段的原始内联图/大图预览与 24 小时文件缓存合同。

## 错误及提交状态

静态错误分类区分 credential_unavailable、invalid_parameters、model/permission 拒绝、authentication_failed、permission_denied、insufficient_balance、quota_exhausted、rate_limited、upstream_failed、timeout、transport_failed、invalid_response。HTTP payload 只读取已知错误 code，不回显消息、原始载荷、URL、请求头或认证。工具参数由入口一次转换，绕开SDK默认参数错误中的原值回显；Schema仍由同一参数类型生成。

提交状态为 `not_submitted`、`rejected`、`accepted` 或 `unknown`：有效异步 202 为 accepted；目录/输入失败与连接建立失败可确认未提交；服务器明确4xx拒绝为rejected；请求进入提交后断线、timeout、取消、重定向或异常响应则保守报告unknown。取得合法 `X-Client-Request-ID` 后，成功结果、图片解析/下载/交付失败及适用的取消诊断均保留 `request_ref`；此前的失败不构造引用。MCP取消只停止当前等待，不表示业务取消、免费或退款。

## 后续接口与发布

S1 的服务端目录结构见[能力目录契约](IMAGE-CATALOG-CONTRACT.md)。单异步持久关联和批量只读恢复见[任务接口契约](IMAGE-TASKS-CONTRACT.md)；B3a 已实现单异步客户端，B3b批量执行及恢复见[批量契约](IMAGE-BATCH-CONTRACT.md)。生产发布、真实 Key 和收费调用另批授权；D1 的系统凭据与本机配置验证使用专用合成条目。

## D1 管理配置与操作快照

MCP共八个工具：七个图片业务工具和一个配置入口。`configure_tokemula` 只接收可选 `interface=browser|terminal`，不接收 Key、地址或任意配置字段。terminal 返回独立管理程序的安全本机命令；browser 返回一次性限时 loopback 链接。未知界面返回页面与终端两条入口，不根据stdio是否TTY猜App，也不主动打开浏览器。

客户端声明URL elicitation时，2025协议使用官方SDK的URL请求；2026-07-28使用InputRequired/MRTR承载同一URL请求，续轮只接受当前会话ID。客户端未声明URL能力时返回可点击链接。API Key始终只在本机页面或用户自己的无回显终端输入，不进入MCP form或工具参数。启动、协议探测和工具枚举不创建页面；用户明确发起后，同一未完成会话复用。

系统Key更新只影响后续新操作；在途HTTP身份与既有请求引用保持，不因设置更新重新提交。历史任务/批量查询不检查默认模型是否适用。logout后新的操作重新读取存储，不沿用旧客户端缓存Key。设置读取错误保留可用的安全任务、批量与请求引用。

新管理路径的模型偏好只参与收费POST前的选择，具体规则见[目录与用户偏好](IMAGE-CATALOG-CONTRACT.md)。返回的 `model_selection` 说明默认模型、当次选择、是否降级及原因；`effective_model` 仍只取服务器响应，不以本地选择冒充实际模型。

## B1 离线兼容数据结构

原 B1 library 仍可单独用于离线目录校验和模型选择。下列价格/费用类型仅为库级兼容，MCP不输出这些字段：

```rust
use tokemula_image_connector::{Catalog, ExecutionMode, Operation, SelectionRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let json = std::fs::read_to_string("connector/tests/fixtures/catalog.json")?;
    let catalog = Catalog::from_json(&json)?;
    let request = SelectionRequest::new(Operation::Generation, ExecutionMode::Sync);
    let selected = catalog.select(&request)?;
    assert_eq!(selected.id, "sample-26");
    Ok(())
}
```

示例从仓库根目录读取合成 fixture；在自己的调用方中可直接传入目录字符串。指定模型时设置 `request.model_id = Some("sample-fast")`；不适用时返回稳定错误分类、简明原因和可用替代 ID，保留用户选择而不自动换模型。


`connector/tests/fixtures/catalog.json` 是独立编写的合成样例，**不是线上 API 响应**。该离线输入保持原有字段和调用条件。正式 HTTP 目录使用 `models/pricing.rates`；HTTP 层消费正式目录并使用 `data` 与内部 unknown 价格投影执行 B1 模型选择；MCP只输出模型能力与准入，不输出服务端计价或该内部投影。模型 ID 均为测试用名称。

| 对象 | B1 内部表达 |
|---|---|
| 目录 | `can_submit`、可选 `reason_code`、`data` 模型列表 |
| 模型 | `id`、`display_name`、可选 `family/preference_rank`、`availability`、操作/模式 `capabilities`、`pricing` |
| 提交拒绝原因 | `image_access_disabled`、`insufficient_balance`、`quota_exhausted`；其他代码使用通用拒绝说明，不回显原值 |
| 价格 | `available` 带 `amount`，或 `hidden` / `unknown`；后两种不带金额 |
| 费用回执 | `pending/settled/unavailable` 和 `visible/hidden`；仅 `settled + visible` 必须带金额，其余组合不带金额 |
| 金额 | `ledger_amount` 是非负十进制字符串，`ledger_unit` 是账户记账单位；可选 `display.amount/currency` 单独表示服务端展示数据 |

金额字符串精确保留数值，不做浮点计算、汇率换算或货币符号推断。`"0"` 是已知零金额；无金额、隐藏、待结算分别由状态表达。测试单位为 `TEST_CREDIT`，展示单位示例为 `TEST_DISPLAY`。

解析失败仅返回 `invalid_catalog` 或 `invalid_fee_receipt`，不保留底层解析器的原始错误或载荷。选择错误包括 `submission_not_allowed`、`model_not_found`、`model_unavailable`、`unsupported_operation`、`unsupported_mode`、`unsupported_parameter`、`no_compatible_model`。成功返回模型只是离线选择结果，实际执行结果由后续调用链记录。
