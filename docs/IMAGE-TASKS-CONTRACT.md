# 图片任务与批量结果恢复

使用提交时的同一 API Key。单异步按已有 `task_id` 恢复请求引用，批量按已有 batch ID 恢复状态和输出。单异步生成、编辑和 `get_image_task(task_id)` 保留原任务恢复能力；批量提交、分页、按项交付与业务取消见[批量客户端契约](IMAGE-BATCH-CONTRACT.md)。客户端不查询或输出费用，费用在 [Tokemula 网站](https://tokemula.com)查看。

## 单异步任务：保存引用，再执行

既有 `POST /v1/images/generations/async`、`POST /v1/images/edits/async` 提交成功返回 202；`Location` 与 `poll_url` 指向 `GET /v1/images/tasks/{task_id}`。无 `/v1` 的既有别名保持。

新增字段：

| 字段 | 含义 |
|---|---|
| `request_ref` | 取提交请求的服务端 `ClientRequestID`，与该次响应头 `X-Client-Request-ID` 一致；在执行前保存到 Redis，202 和后续任务 GET 返回同一个值 |
| `result_expires_at` | 图片结果的可靠到期时间，Unix 秒或 null。当前存储接口只提供 URL，没有期限来源，返回 null |

既有 `id`、`task_id`、状态、结果、错误、创建/完成时间及查询条件保持。每次 GET 的响应头属于本次查询请求；恢复原生图请求时，使用任务 JSON 中的 `request_ref`。完成、处理失败、结果转存失败和服务对象重建均保留原引用。旧任务没有引用时省略该字段，原结果仍可读取。

`expires_at` 始终表示 Redis **任务记录**到期：沿现有默认 24 小时 TTL，创建时设置，完成或失败时重新计时。它与图片 URL 期限分别表达。任务到期后按原规则返回 404；客户端继续保留已取得的请求引用。

任务状态描述图片执行与结果保存情况；失败或取消不推导退款。

## Connector B3a：一次提交、按需查询

`generate_image` 与 `edit_image` 增加可选 `mode`：省略或 `sync` 保持原同步行为；`single_async` 分别使用 `/v1/images/generations/async` 与 `/v1/images/edits/async`。共用目录选择、参数/输入校验、编码后的请求体限制及一次 POST；`mode` 留在 Connector，不写入 Images 业务字段。B3c 保留 `image_path`，增加有序 `image_paths`、第一图 mask 和目录声明的高级参数；同步/单异步共用输入规则，见[HTTP 编辑契约](HTTP-DEVELOPMENT-CONTRACT.md)。

有效 202 必须含一致的任务标识、原请求引用和 `processing` 状态，原引用与提交响应头核对。客户端立即返回 `submission=accepted`、任务 ID、引用、状态及期限，不附加任务 GET。无法确认受理时返回 `unknown`，保留已取得的安全引用；不会重发 POST。

`get_image_task` 只接受 `task_id`，验证安全 ID 后构造固定 GET 路径；不使用 `poll_url`、目录、`can_submit` 或本机登记作为前提。使用当前配置的 Key，由服务器判断归属。一次调用只读取一次任务：

- `processing`：返回状态、原引用、期限，以及可用的 `retry_after_seconds`。
- `completed`：从 `result` 复用原图片解析、独立无认证下载和 MCP 大图/多图交付，保留原请求引用。任务响应读取预算按当前服务端最多 10 张输出、每张 20 MiB 原图的 base64 上界与 JSON 余量计算；单图和 MCP 消息限制保持原规则。
- `failed`：执行超时返回 `image_task_timeout`，已确认的结果转存失败返回 `image_result_storage_failed`，其他错误返回 `image_task_failed`。原因仅由白名单类型/代码及固定公开消息映射为静态说明，原始错误消息不回显。
- 查询失败：返回 `query_state=not_obtained`、任务 ID 和本地已知引用，提交状态为 `unknown`；下一次查询仍是 GET。

旧任务缺少原引用时继续交付原结果。本地已有引用单列在 `recovery.request_ref`，来源为 `local_record`；顶层 `request_ref` 保持 null。GET 响应头始终属于该次查询，不能替换任务 JSON 中的引用。停止等待只停止本次 MCP 操作；已保存的任务可继续按 ID 查询。已取得任务状态后若在结果读取或交付期间出错或取消，保留该 `status` 和 `query_state=retrieved`。错误中的已知引用只补缺失值，`request_ref_source` 区分 `task_json`、`submission_header` 和 `local_record`；任务身份匹配时，即使状态字段无效，也保留已取得的安全 JSON 引用。202 头与 JSON 引用不一致仍返回 unknown。

### 私有恢复记录

Unix：`$HOME/.local/state/tokemula-image-connector/tasks/<task_id>.json`；Windows：`%LOCALAPPDATA%/tokemula-image-connector/tasks/<task_id>.json`。有效 202 解析后立即通过临时文件原子保存；Unix 目录 0700、文件 0600。同步与纯离线入口不依赖这个目录可写。

记录仅含 `task_id/request_ref/selected_model/status/created_at/completed_at/expires_at/result_expires_at/observed_at`，不含 Key、Key 散列、认证、提示词、图片或完整载荷。写入失败时仍返回 accepted 与全部引用，`recovery.status=failed` 及安全存储错误提示调用方另存。

重建客户端后，调用方可从记录取出 ID 再查询；启动不会读取全部记录或自动访问网络。状态和观察时间是本地恢复快照，服务器任务 GET 提供当前状态。任务引用不随图片缓存清理，当前保留至用户主动清理；另存仍需要的任务和请求引用后再删除文件。任务 404/过期后，错误保留已知引用，不重新提交。

## 批量任务：服务端 DTO 与客户端输出

服务端 `BatchImagePublicBatch` 保留既有字段与数值，以下为兼容接口参考。Connector对外投影仅保留任务/条目/状态/期限及图片，剔除价格和金额字段；不修改服务端结算。

服务端 DTO 字段包括：

- `ledger_unit`：固定 `"CNY"`，说明客户金额的账户单位。
- `output_expires_at`：直接投影任务的 `OutputExpiresAt` 为 Unix 秒；缺值返回 null。它表示输出清理期限；实际可取得性继续由任务状态、输出删除状态和原下载流程决定。

原字段的 JSON 类型和数值保持：

| 字段 | 类型与来源 |
|---|---|
| `estimated_cost` | number，任务的预计费用 |
| `hold_amount` | number，任务预占金额；历史缺 `HoldAmount` 时沿原规则回退到 `EstimatedCost` |
| `actual_cost` | number 或 null，任务保存的结算结果；缺值保留 null |
| `settled_at` | Unix 秒或 null，任务结算时间 |
| `output_deleted_at` | 原可选 Unix 秒字段，输出删除时间 |

批量金额列为 `DECIMAL(20,10)`，公开 DTO 沿用 Go float64/JSON number；余额及冻结余额列为 8 位小数。原批量 hold/capture 命令与任务结算字段各自保留原值，既有数据库落账精度差异随原链路呈现。本接口不把预估、预占或历史 fallback 当作最终扣款，也不把单任务费用回执的 8 位字符串规则套到批量 number 字段。

真实隔离账务反例：预占 `0.0001562500`、任务结算 `0.0000781250` 时，余额从 `100` 经预占变为 `99.99984375`，capture 后为 `99.99992188`；净扣 `0.00007812 CNY`、冻结余额归零。任务结算值与余额差额相差 `0.000000005 CNY`，源于既有列精度；保持原金额字段与扣款行为，统一批量账务精度另批处理。

输出到期或删除后，费用元数据仍属于任务记录；图片清理和账务保留分别处理。

## 耗尽余额或额度后恢复结果

以下五个已注册 GET 路由允许有效身份在余额、Key 额度或适用订阅额度耗尽后读取已有结果：

1. `/v1/images/batches`：列表按当前用户及 Key 过滤。
2. `/v1/images/batches/{id}`：状态与输出元数据。
3. `/v1/images/batches/{id}/items`：条目列表。
4. `/v1/images/batches/{id}/items/{custom_id}/content`：指定图片内容，沿原 `image_index` 参数。
5. `/v1/images/batches/{id}/download`：ZIP 下载，沿原过滤与下载限制。

所有单项读取保持 UserID + APIKeyID 双重归属校验；用户/Key 禁用、Key 过期、IP、分组身份限制继续生效。单异步 GET 沿原查询准入，不增加新提交条件。

这些读取不创建任务、不生图、不扣费、不预占、不重新投队。内容和 ZIP 下载成功后的原下载记录更新保留。已清理输出继续返回原不可用状态，不自动重新生成。

批量提交、POST cancel、DELETE 及 `/v1/images/batches/models` 保持原准入；豁免按具体 GET 路由匹配。图片能力目录仍按[目录契约](IMAGE-CATALOG-CONTRACT.md)独立处理。
