# 图片能力目录与服务端兼容字段

契约版本：`schema_version="1"`。`GET /v1/images/models` 使用图片 API Key 的 Bearer 鉴权，返回当前 Key 的配置与候选能力快照；实际生成仍执行原有鉴权、调度和计费。下文JSON和价格字段是服务端wire兼容参考；Connector的模型列表只输出能力与提交限制，不输出价格或金额。费用在 [Tokemula 网站](https://tokemula.com)查看。

## 请求与响应

无查询参数、无请求体。成功 HTTP 200，`Cache-Control: private, no-store`：

```json
{
  "schema_version": "1",
  "observed_at": "2026-11-19T00:00:00Z",
  "can_submit": true,
  "models": [{
    "id": "synthetic-image",
    "display_name": "synthetic-image",
    "availability": "configured",
    "capabilities": [{
      "operation": "generation", "mode": "sync",
      "parameters": {"max_images": 10, "sizes": ["1024x1024"], "qualities": ["auto"]},
      "input_limits": {"max_request_bytes": 268435456, "max_images": 0, "max_image_bytes": 0, "mime_types": []},
      "result_formats": ["b64_json"], "cancel_supported": false
    }],
    "pricing": {
      "status": "available",
      "rates": [{
        "operation": "generation", "mode": "sync", "unit": "image",
        "quantity_basis": "output_image_count",
        "conditions": {"size_tier": "1K"},
        "unit_price": {"ledger_amount": "0.25", "ledger_unit": "CNY"}
      }]
    }
  }]
}
```

示例模型、价格与时间均为合成数据。`reason_code` 只在受限时出现，例如 `image_access_disabled`、`insufficient_balance`、`quota_exhausted`、`subscription_unavailable`、`subscription_limit_exceeded` 或 `no_compatible_model`。余额/额度不足仍返回自身可见目录及 `can_submit=false`；没有图片权限返回空 `models`，不暴露其他分组。

身份无效、Key 停用/过期、用户或 IP 限制沿原鉴权返回 401/403/429。目录依赖读取失败返回 503 和安全错误，不回退通用模型列表。只有本 GET 具有报价读取准入，原 POST 的收费检查不变。

## 模型与能力

- `id` 是客户请求使用的精确公开 ID；`display_name` 是其可读名称。
- 可选 `family` 与 `preference_rank` 只有可信配置支持时才出现；未标记 Image 2.6 时，客户端不根据名字猜系列。rank 数值越小越优先，同级保留目录顺序。
- `availability` 为 `configured` 或 `temporarily_unavailable`。这不是实际提交成功证明。
- `capabilities` 每项明确一对 `operation=generation|edit` 与 `mode=sync|single_async|batch`；不能将不同项任意交叉组合。组合缺失即未声明支持。
- `parameters.max_images` 是该操作每次/每项允许的输出数量上限，最小为 1。`sizes`、`qualities` 为可显式指定的值；空列表表示不接受显式指定，省略请求参数使用服务端默认。不是“无限制”。
- `input_limits.max_request_bytes` 是每项能力必选的正整数，限制编码后的完整 HTTP 请求体字节（JSON 或 multipart，包含 multipart boundary/part headers，不含 HTTP headers）。取 gateway 限额和有效 server 全局限额的较小值；server 未设正限额时沿原规则回退 gateway。默认均为 256 MiB。generation 没有图片输入仍有此请求体限制；客户端发送前检查实际编码体，不能只累加原始文件大小。
- `input_limits.max_images/max_image_bytes/mime_types` 给出输入图片上限和格式；generation 无图输入时为 0/0/[]。可选 `max_prompt_bytes`、`max_batch_items`、`max_batch_output_images`、`max_batch_input_images`、`max_batch_input_bytes` 为批量模式的有效配置限制；后两项按每项 `output_count` 展开后累计：`sum(reference_image_count × output_count)` 与 `sum(inline_reference_bytes × output_count)`，不是去重后的文件个数/字节。每张、每项、整个任务及编码后请求体限制必须同时满足。
- `result_formats` 使用 `b64_json`、`url` 或 `authenticated_download`；批量下载需要该 Key。`cancel_supported` 仅批量业务取消为 true，停止 MCP 等待不是业务取消。
- 尺寸/质量等不能确证的能力不放入允许列表；不能确证整个实际转发目标时，不声明该组合。对同一公开 ID 可达的多条路径取可保证范围的交集。

### 可选尺寸规则与图片思考复杂度

`parameters` 可增加 `size_rules` 和 `complexities`，仍使用契约版本 1。旧目录省略这两个字段时，新客户端继续按原有 `sizes/qualities` 工作；旧客户端可忽略新增字段。

`size_rules` 描述该操作、模式与实际模型路径支持的自定义像素尺寸：`multiple_of` 为宽高步长，`min_edge/max_edge` 为每条边的闭区间，`min_pixels/max_pixels` 为总像素闭区间，`max_aspect_ratio` 为对称比例上限（长边÷短边）。自定义 `WIDTHxHEIGHT` 必须同时满足这些条件。未声明规则时仅接受 `sizes` 列出的显式值。固定像素显示为“1024×1024 · 1:1”；`auto` 显示“自动”；批量 `1K/2K/4K` 保持分辨率等级。

`complexities` 列出可明确发送的图片模型思考复杂度，例如 `["low","medium","high"]`。缺失或空列表时不隐式发送复杂度，显式请求在收费提交前拒绝；支持 `medium` 时，未指定的普通生成、编辑和 `single_async` 明确发送 `medium`。该参数独立于画质和语言模型的推理强度。

管理设置以操作和执行模式保存尺寸、画质。未设置表示模型默认；仅在该能力明确接受时发送字面 `auto`。单次参数覆盖已存偏好，解析后的明确要求参与整次请求的候选筛选，备用模型保持相同尺寸、画质和全部输入。旧任务查询与批量同键重放沿原记录，不重新应用当前偏好。

当前已核对的 Image 2.6 精确型号为 `gpt-image-2.6-sunburst` 与 `gpt-image-2.6-flare`。原生 Images 路径的自定义尺寸：宽高均为 16 的倍数，每边不超过 3840，总像素 655360–8294400，长短边比例不超过 3。该系列画质为 `auto/low/medium/high/xhigh/max`，复杂度为 `low/medium/high`。具体配置与可见型号仍取当前 Key 的目录。[官方图片指南](https://developers.openai.com/api/docs/guides/image-generation#gpt-image-26)

OAuth 的 Responses 回退同样转发图片工具自身的 `complexity`，保持外层语言模型 reasoning 设置。该型 OAuth 路径当前目录公布三种标准像素尺寸及 `auto`；纯 APIKey 原生路径另公布自定义规则。同一公开 ID 同时可达两种路径时取交集。GPT Image 2/2.5、1.5/mini 沿各自已确认的尺寸与画质，不从 2.6 的参数推导它们的能力。[Responses 图片工具参数](https://developers.openai.com/api/reference/resources/responses/methods/create)

## 可选高级编辑能力

`edit + sync/single_async` 可增加 `edit_options`。缺少该字段只表示未声明 mask 与新高级选项；原单图入口及既有 `input_limits` 继续有效，多参考图数量仍使用 `input_limits.max_images`。新增字段不用于 batch，也不改变 generation 的输入契约。

```json
{
  "edit_options": {
    "backgrounds": ["auto", "opaque", "transparent"],
    "output_formats": ["png", "jpeg", "webp"],
    "input_fidelities": ["low", "high"],
    "transparent_output_formats": ["png", "webp"],
    "output_compression": {"min": 0, "max": 100, "formats": ["jpeg", "webp"]},
    "mask": {
      "max_images": 1,
      "max_image_bytes": 4194303,
      "mime_types": ["image/png"],
      "requires_alpha": true,
      "same_dimensions_as_first_image": true,
      "applies_to_image": 0
    }
  }
}
```

以上为纯 APIKey 路径、上游 `gpt-image-1.5` 的示例。数组为可显式选择的值；空数组表示该选项未声明支持。`output_compression` 和 `mask` 缺失表示不支持对应新用法。压缩值为包含端点的整数区间，且输出格式必须属于其 `formats`；透明背景须匹配 `transparent_output_formats`。省略 output_format 时按服务端 PNG 默认理解，但请求不补写新参数。所有新参数省略时保持原请求。

mask 独立于参考图计数，只允许一张 PNG，含 alpha 通道，与第一张参考图宽高相同，并作用于其零号位置。本产品的精确 mask 上限为 `4194303` 字节。参考图仍按其原 `max_images/max_image_bytes/mime_types` 校验；完整请求体限额包含全部参考图、mask 和 multipart 开销，不压缩或重绘输入来通过限制。[OpenAI multipart 编辑参考](https://developers.openai.com/api/reference/python/resources/images/methods/edit)使用“小于 4 MB”的表述，并规定 PNG、第一图及尺寸关系；[图片指南](https://developers.openai.com/api/docs/guides/image-generation)规定 alpha 通道。指南另有 50 MB 通用图片说明；本产品的精确字节限制由上述 `max_image_bytes` 字段声明。

实际路径投影：

- 当前 OpenAI APIKey、OAuth、SetupToken 对 `images-basic/images-native` 均可承接；mask 和高级选项触发 native，显式模型/尺寸也触发，单纯增加 multipart 图片数不触发。客户端不根据旧账号标记虚构 basic-only 限制。
- APIKey 原 multipart 透传。`gpt-image-1.5` 可声明 fidelity `low/high`，`gpt-image-1-mini` 仅 `low`；`gpt-image-2` 及本目录 2.5 型号不声明该选项。OAuth 可从 direct 回退 Responses，后者未传 input_fidelity，因此 OAuth 可达时此项交集为空；其他已列选项和 mask 在两条路径均保留。
- 兼容 Gemini 请求按现有 `images-apikey` 条件排除不能接收该请求的 OAuth；未有正式 profile 的型号继续不擅自声明能力。
- Grok multipart 在当前转发中丢弃四项高级参数；官方编辑规范未给出 mask 语义，因此本目录只保留已实现的有序多图能力，不声明 edit_options。其余参数仍遵循原 Grok 能力。
- 单异步复用同步解析和执行路径，采用相同高级限制；原 Composite 不新增 single_async。

以上仅投影现有路由，没有改变账号调度、鉴权或 POST 行为。各条件以同一公开模型实际可接收请求的路径为范围，保留跨路径交集。

## 用户默认模型与有序备用

管理配置保存实际 `default_model`、有序 `fallback_models` 和 `auto_fallback`，由当前Key目录确认；名称用于显示，不从“2.6”等文字猜测ID。此规则属于新的系统设置管理路径，B1纯离线选择及原显式注入/环境开发入口保持原条件。

本次明确指定model时沿用精确选择，不自动替换。未指定且已有偏好时，先验证默认模型能否满足整次请求；默认缺失、暂不可用或完整参数/所有输入/实际编码体不兼容时，只有auto_fallback开启才按备用顺序寻找适用项。没有适用项返回原因与完整请求可用的替代ID，不挑偏好链外模型，也不减少n、丢参考图、去mask或修改参数。关闭降级时只尝试默认模型。

生成、编辑、single_async和batch各按自身正式能力检查，同一偏好排序复用。Key无效、全局准入拒绝、余额不足或目录失败不会触发逐模型网络尝试。所有选择发生在一次收费POST前；POST后超时、断线或结果未知不再换模型提交。批量同键明确重放优先使用首次记录的模型和稳定参数，后来更新默认模型不改变同一请求。

降级只影响当前任务，不改保存的默认或顺序。结果的model_selection包含当次selected_model、used_fallback及安全原因；下一次仍先检查默认模型。服务器提供的effective_model单列保留。查询既有任务和结果不应用该选择规则。

## 服务端保留的 CNY 价格字段

`pricing.status` 为 `available`、`hidden` 或 `unknown`。`hidden/unknown` 没有金额或 rates，可带安全 `reason_code`；不会用零值代替。hidden 仅控制本报价，不改变既有实际消费金额的可见性。

`available` 的 `rates` 是有依据的条件单价，不是预计总费用，也不是最终扣款。每条 rate：

| 字段 | 含义 |
|---|---|
| `operation/mode` | 适用的能力组合 |
| `unit` | `image`、`per_request` 或 `token`，保持真实计费模式 |
| `quantity_basis` | image/per_request 为 `output_image_count`：现有图片按次链路也按输出张数计次；token 为 `usage_tokens` |
| `conditions` | 可选 `size_tier`（1K/2K/4K）、`context_min_tokens`、`context_max_tokens`；未列出的条件不改变该单价。context 区间为 `[min,max)`，max 缺失表示无上界 |
| `unit_price` | image/per_request 的单位客户金额；token 不使用此字段 |
| `components` | token 单价列表，每项为 `kind` 与 `unit_price`；kind 为 `input`、`image_input`、`output`、`image_output`、`cache_write`、`cache_read`、`image_cache_read` |

金额统一为 `{"ledger_amount":"精确十进制字符串","ledger_unit":"CNY"}`，不含 display、汇率、内部倍率、上游成本或账号/渠道 ID。账户扣减数值 1 表示人民币 1 元；没有 USD/CNY 的 1:1 换算。客户端不计算账务或将单位价重命名为预计总价。

size_tier 对应结算中的图片尺寸档：最长边≤1024为1K、≤2048为2K，其余为4K；明确的1K/2K/4K标签按其档位，未确定尺寸默认2K，结算仍优先采用实际响应尺寸。Token价保留现有上下文条件；没有最终usage时预计总额保持unknown。报价按observed_at的有效时点取得；后续提交时仍按原计费时点和实际用量结算。

多个可达账号/映射不能给出一致报价时返回unknown，不任选最低价。存在本接口无法完整表达的计费条件时返回unknown并给出reason_code，而不是伪造平价。已知零单价可为available并用字符串"0"表达。

## Connector 边界与后续接口

正式wire沿用models/capabilities/pricing；B2的 `b2-dev-1` 不保留长期兼容层。B1 `data/amount` 仍是内部离线结构：HTTP层消费正式目录，模型选择使用不改变权限的内部价格投影；MCP输出剔除pricing和金额，保留能力元数据。非image计价不是无效目录；原离线调用者无需提供schema_version、网络或认证。

S1服务端目录、候选能力及其兼容价格字段保持；服务端费用接口另见[兼容费用契约](IMAGE-BILLING-CONTRACT.md)。Connector不读取该费用接口；生成/编辑wire、原图与预览、任务/请求引用、一次POST及本地文件期限沿既有实现保留。

单异步任务引用、任务期限和批量结果恢复见[任务与批量恢复契约](IMAGE-TASKS-CONTRACT.md)。
