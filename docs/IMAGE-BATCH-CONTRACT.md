# 批量提交与结果恢复

批量使用 `submit_image_batch`、`get_image_batch`、`cancel_image_task`。批量使用一个模型，按条目区分 generation/batch 与 edit/batch。同步 `n>1` 和单异步任务仍走原接口。

## 提交一次，保存恢复信息

`submit_image_batch` 接受可选 `model/task_name/size/idempotency_key` 和必需 `items`。每项必需 `custom_id/prompt`，可选 `output_count`（默认1）、`reference_image_paths`（用户提供的本地绝对路径）。

```json
{
  "task_name": "商品图片",
  "items": [
    {"custom_id": "cover", "prompt": "白色背景的产品封面", "output_count": 2},
    {"custom_id": "detail", "prompt": "展示材质细节", "reference_image_paths": ["/absolute/path/reference.png"]}
  ]
}
```

先读取正式目录，选择同时满足全部条目的模型。显式模型不替换；不适用时返回具体原因与适用于整个批次的替代模型。逐项检查提示词 UTF-8 字节、输出数、图片数、实际 PNG/JPEG/WebP 内容及单图字节；整批检查条目数、总输出数、加权参考图总量与输入字节：

- `sum(reference_image_count × output_count)`
- `sum(inline_reference_bytes × output_count)`

每张原图最多20 MiB和6400万像素，并服从目录更小的限制。图片保持原始字节；不丢图、不压缩上传、不减少输出数。

HTTP wire 使用 `POST /v1/images/batches`，成功为 **HTTP 200**，不是单异步的202。请求包含明确的 `model/task_name/image_size/response_mime_type/items`；图片为 `reference_images:[{"mime_type":"image/png","data":"base64"}]`。`size` 写入 `image_size`，省略时使用当前Go契约的`1K`，仍检查目录支持；输出格式固定为当前默认`image/png`。不透传 Provider、远程 file_uri 或高级参数。实际编码后的完整 JSON 字节必须符合每个适用能力的 `max_request_bytes`。

`output_count` 原样发送，由服务器展开条目。客户端不复制 items。服务器列表返回的 `custom_id` 才是后续取图标识；例如当前服务可能把 `cover` 展开为 `cover_01/cover_02`，调用方始终使用实际列表值。

### 稳定幂等与明确重放

每次新提交生成非秘密幂等键，或使用调用方提供的键。键为1–128位可打印ASCII，不含空格。首次POST前确定稳定名称：省略时为`Image batch <idempotency_key>`；显式名称去首尾空白、最多255个UTF-8字节。明确发送名称和固定默认参数，避免服务端按当前时间或配置补值而改变同一请求。

普通管理路径未指定model且没有该键记录时，先按用户默认/备用策略选择满足整个批次的模型；同键恢复仍以首次记录中的模型与稳定参数为准，更新用户偏好不改变原请求。

首次发送前，原子保存最小记录。保存失败即停止，**0 POST**。有效200后更新同一记录，返回`submission=accepted`、真实`batch_id`、幂等键和任务状态；更新失败仍交还受理ID，`recovery.status=failed`提示另存。一次工具调用最多一次提交POST；禁用自动重试与API重定向。

未知结果保留幂等键及已有ID。已知ID时用GET恢复；未知ID时读取一页任务列表，由调用方确认，客户端不按相似名称猜关联。明确重放时，提供同一键和相同条目/文件内容；省略`model/task_name/size`时从该键的记录恢复首次值，不随目录排序变化。显式修改字段仍带原键交服务器判断。409幂等冲突返回`idempotency_conflict`，不更换键绕过。

两次显式重放会有两次HTTP POST；服务端应返回同一批次，业务创建与预占仍各一次。提交准入保持，余额耗尽时GET可恢复，POST重放仍执行原鉴权与额度检查。

### 本地记录格式

B3a 的 `tasks/<task_id>.json` 格式不变。批量记录放在独立子目录：

- Unix：`$HOME/.local/state/tokemula-image-connector/tasks/batches/`
- Windows：`%LOCALAPPDATA%/tokemula-image-connector/tasks/batches/`

文件名为幂等键的URL-safe base64编码加`.json`，不使用Key或Key散列。`record_type="image_batch_v1"`，字段仅为`idempotency_key/task_name/selected_model/image_size/response_mime_type/submission/batch_id/status/observed_at`。不保存API Key、认证、完整提示词、参考图或请求体。Unix目录0700、临时文件0600，通过fsync后原子替换。记录保留至用户主动清理，独立于24小时图片缓存。

发送前记录为`submission=unknown`，覆盖进程在发送附近退出的情况。后续记录仅是本地观察；真实状态由服务器查询。读取到记录后立即保留已知batch_id、名称、模型和幂等键；后续缺少Key、提交准入、参数或文件读取失败时仍交还这些恢复引用。重建服务不自动联网、扫描或轮询。原同步、单异步及已有任务GET不要求批量目录可写。

## 按需读取一页或一张图片

`get_image_batch` 根据参数选择一次有限操作：

| 参数 | 请求 | 输出 |
|---|---|---|
| 无 `batch_id` | 一次批量列表GET | `data/has_more/cursor/limit/next_cursor` |
| 仅 `batch_id`，可带分页 | 一次详情GET + 一次items GET | 任务字段和 `items` 分页对象 |
| `batch_id/custom_id`，可带 `image_index` | 一次详情GET + 一次content GET | 指定图片与已取得任务字段 |

服务器条目ID原样保留，包括`#`、`?`、斜杠和点段。取图时`#`、`?`、斜杠和反斜杠按URL路径段编码，认证目标保持固定API；服务端无法访问的ID沿原HTTP错误返回。`.`、`..`和控制符只拒绝该次取图并保留batch/custom上下文，不影响整页条目交付。

`image_index`默认0。取图不接受`cursor/limit`；没有`batch_id`不能指定`custom_id/image_index`。不带`custom_id`也不能指定`image_index`。

分页默认20，明确发送给服务器。任务列表`limit`为1–100，items为1–500。cursor是非负数字偏移字符串；`next_cursor=offset+本页条目数`，不是最后一项ID。服务端以`len(data)==limit`设置`has_more`；只有下一次显式调用才读取下一页，不将第一页视为全量。

读取已有任务不查询目录、不要求`can_submit`或本地登记，归属由当前Key对应的服务端校验。沿用`queued/running/processing_results/settling/completed/failed/cancelled/output_deleted`；completed仍需查看每项`succeeded/failed/pending`。

### 当前Key认证的图片内容

固定API路径`GET /v1/images/batches/{id}/items/{custom_id}/content?image_index=N`携带当前Key。拒绝重定向，不访问远端提供的下载URL，不把受保护地址交给Harness。实际字节和声明MIME须一致，再复用原MCP图片交付；远端Content-Disposition不决定本地路径。

大图仍保留完整原件、生成明确标识的预览，遵守8 MiB整条MCP消息预算、私有权限和24小时本地期限。只下载指定图片，状态查询不下载。输出过期或删除返回`batch_output_unavailable`，不重新生成；后续内容/缓存失败保留`batch_id/custom_id/image_index`及已取得状态，`query_state=retrieved`。

## 取消是业务请求

`cancel_image_task({"batch_id":"..."})`只发一次`POST /v1/images/batches/{id}/cancel`，不查询目录、不自动轮询或重试。原服务端余额、权限限制保持。

- 返回`cancelled`：`cancel_state=completed`。
- 返回`completed/failed/output_deleted`：`cancel_state=already_finished`，保留该原状态。
- 仍为running等非终态：`cancel_state=requested`，取消尚未完成。
- 服务端明确返回取消失败：`cancel_state=failed`；权限拒绝为`rejected`；断线、超时等结果未明为`unknown`。保留batch_id和安全错误，后续用`get_image_batch`查询。

单异步任务不支持业务取消。MCP停止等待仅停止本次客户端调用；不会自动发送业务取消。任务已完成部分可能已计费，状态不推导退款。

## 任务结果与站内费用

Connector只返回批量任务、条目、状态、输出期限和图片；模型、提交、列表、详情及取消结果均不输出价格、预估费用、预占或实际金额，也不调用单请求费用接口。费用在 [Tokemula 网站](https://tokemula.com)查看。

服务端原DTO与账务保持不变，库级解析可继续保留兼容字段；它们不进入Agent结果。图片输出期限与清理、任务引用和防重复提交规则保持。

## 协议来源与本批范围

协议依据当前主站`BatchImageSubmitRequest`、`validateSubmitRequest`、`BatchImagePublicService`、`BatchImageHandler`与`BatchImageDownloadService`。B3b用Go overlay及合成依赖产生真实handler/service响应，再经Rust loopback HTTP→MCP消费；服务端候选只读。

本批按项交付，不提供ZIP下载/解包、删除、自动全量下载、后台轮询或普通edit多参考图/mask。
