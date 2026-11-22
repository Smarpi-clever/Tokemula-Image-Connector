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

成功响应沿现有 Images 格式：`data` 数组，每项可提供 `b64_json`、`url` 或二者，可带 `expires_at`（Unix 秒，未给则远程期限未知）。非空 `b64_json` 优先解码并完成图片验证；缺失、空串或全空白时才使用有效 URL。损坏的非空 b64 或解码后的非图片如实失败，不转用 URL、不重投。该共享解析同时用于同步生成/编辑及单异步完成取图。HTTP 头 `X-Client-Request-ID` 是关联引用。可选顶层 `model` 表示服务端提供的客户可见实际模型，不自行用选择值冒充该信息。

返回 MCP image 与结构化元信息（selected_model、effective_model/source、状态与request_ref），图片格式由实际内容验证。URL 用独立、无 Authorization 的 HTTP client 下载，下载重定向同样不携带 API 凭据；URL 不写入工具诊断。远端到期时间单列，未知为 null，不从它推导本地文件期限。

### macOS 产品默认交付：桌面持久原图

独立 MCP 的 `for_current_user()` 产品入口把每张已取得的完整原图保存到当前用户 `Desktop/Tokemula-Images`，解析桌面真实父路径以跟随用户符号链接/iCloud 桌面；不修改桌面或已有目录权限。生成、编辑及批量提交在读取操作设置/发出收费 POST 前创建一次写入探针，无法写入时返回 `image_storage_unavailable`、`not_submitted`，不改目录或重投。保存后才返回路径；写入结果失败返回 `image_storage_failed`、`accepted` 并保留原请求/任务引用及已保存的本次文件路径。

图片字节、格式和尺寸保持原样。文件名使用独立随机名称，编辑和多图不覆盖输入或先前产物。`local_files` 按本次图片顺序返回 `image_index`、绝对 `path`、`mime_type`、`width`、`height`，并使用 `persistent=true`、`retention="user_managed"`、`expires_at=null` 表示无自动过期；不把旧临时 `expires_at` 语义改成虚假远期时间。`content.text` 同时给出每张完整绝对路径，使模型直接消费，`inline_representation` 区分协议图块是 `original`、`preview` 或显式文件交付的 `none`。

消息预算仍为按 SDK 官方类型序列化的 8 MiB（含 request ID、全部图片和元信息）。小图保持原 ImageContent；超预算逐个换为 256 像素内 PNG 预览，完整原图已经保存，仍展示同一次持久路径，不再另写缓存或创建 10 分钟内存资源。此产品路径不声明 MCP Apps 工具 UI。WorkBuddy 配套 Skill 默认直接在正文嵌入本次原图并保留同一文件入口，不先调用 `present_files` 或自动打开侧栏；用户明确要求打开时再用原生展示，`files` 为本次原始路径，`cwd` 为任务目录。Codex沿原生图片交付方式使用原图及其绝对路径。

桌面图是用户产物，重启、升级、缓存清理以及卸载（包括 purge）均不删除。用户移动、改名或删除文件后，旧路径失效，展示/编辑应报告真实路径状态，不重新生成或替换为其他图片。

### 同步调用的显式文件交付

`generate_image` 和 `edit_image` 接受可选 `result_mode=inline|files`，仅控制 MCP 返回方式，不传图片上游、不写用户偏好。省略或 `inline` 保持上述图块与 8 MiB 完整消息策略。

`files` 只适用于同步调用，并要求服务已有可用的持久输出；缺少持久能力、目录不可写或与 `single_async` 组合均在图片 POST 前失败，保持 `not_submitted`。完整原图照常保存，text 和结构化 `local_files` 同时给出真实路径及原元信息，`inline_representation="none"`；不构造 base64 图块，不用先编码再删除的方式减重。保存失败保留真实状态、引用和已保存路径，不换目录、不重投。

原异步、批量工具的输入、状态及分页条件保持。旧无持久输出的构造仍可省略该字段或选择 `inline`。WorkBuddy 配套源码 Skill 仅在新 schema 支持且 macOS 持久输出适用的同步调用中选择 `files`，随后直接用 `local-file://local/` 逐段编码的本次原路径在正文嵌图；默认不以 `present_files` 为前置。普通展示不重读全图，视觉分析按需读取。

### 本地分段计时

同步生成/编辑以同一操作 ID 在 stderr 输出非敏感阶段记录，stdout 保持 MCP 协议。开始时间使用 Unix 毫秒与宿主时间线对齐；`elapsed_ms` 相对操作起点，`delta_ms` 表示相邻标记间隔，嵌套区间不重复加总。

阶段覆盖入口/前置、快照派发与 blocking/锁/存储读取、目录 GET 的头/body/校验、候选构造计数、图片 POST 的头/body/JSON、逐图来源/下载/解码/校验，以及原图保存、图块编码、完整消息计长和 render 返回。日志只含静态阶段、状态/错误码、时间、数量及字节；不记录 Key、prompt、URL、路径、请求头、图片/base64 或完整错误。成功、失败和取消记录最后阶段与真实提交状态。render 后的 stdio/宿主消费仍在外部未拆分区间，历史 914 秒不补猜分段值。

### 原有构造与非 macOS 兼容交付

`ImageMcp::new(client)`、`with_settings`、显式目录/离线构造保持原条件，不自动增加桌面写入前置。非 macOS 产品也保留原交付方式：小图原始内联；大图按同一 8 MiB 预算变成 PNG 预览，并把完整图存入当前用户私有 24 小时缓存。`local_files` 的绝对路径、原 MIME、临时 `expires_at` 和 `inline_representation="preview"` 保持原义。缓存启动或下次工具操作仅清理本产品过期普通文件，不删输入、用户桌面产物或链接目标；Unix 缓存目录 0700、文件 0600。输入 20 MiB 与像素限制保持。

未启用桌面交付的旧构造，仍可按客户端 `extensions["io.modelcontextprotocol/ui"].mimeTypes` 协商使用既有 `ui://tokemula/image-viewer`：小图原始 ImageContent，大图原字节在当前进程内保留 10 分钟并经 `resources/read` 分块读取，用户点击后调用 `ui/download-file`。该兼容能力不作为 macOS/WorkBuddy 默认交付前置。

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
