# 服务端兼容参考：图片费用回执

Connector已取消费用查询工具、自动查费与金额展示；用户在 [Tokemula 网站](https://tokemula.com)查看费用。本文保留既有服务端兼容协议和库级类型说明，不作为客户端工具调用指引。

契约版本 `schema_version="1"`。`GET /v1/images/requests/{request_ref}/billing` 使用图片请求的同一 API Key。它只读取已有记录，不生图、不扣费、不重新提交图片。

`request_ref` 是服务端 `X-Client-Request-ID` 返回的公开引用。当前公开生成值为 UUID；此兼容接口接受 1–128 位 ASCII 字母、数字、`_`、`-`，调用方按原值查询并核对响应中的同名字段。内部 `client:` 账务前缀由服务端关联，不作为参数。Connector仅保留引用用于任务恢复或站内记录核对，不请求费用接口；该引用不是重发图片 POST 的幂等键。

## 成功结构

HTTP 200，`Cache-Control: private, no-store`：

```json
{
  "schema_version": "1",
  "request_ref": "synthetic-image-request",
  "status": "settled",
  "visibility": "visible",
  "settlement_source": "balance_debit",
  "amount": {"ledger_amount": "0.00007813", "ledger_unit": "CNY"}
}
```

| 字段 | 规则 |
|---|---|
| `status` | `pending`、`settled` 或 `unavailable` |
| `visibility` | `visible` 或 `hidden`；实际消费金额沿现有用户消费可见性，S1 报价 hidden 不改变此规则 |
| `settlement_source` | settled 必填；`balance_debit` 为余额扣款、`subscription_usage` 为订阅额度消耗、`usage_only` 为仅计量且客户金额为零。其他状态省略 |
| `amount` | 仅 settled + visible 必填；其余组合省略。`ledger_amount` 为非负精确十进制字符串（最多 8 位小数），`ledger_unit` 固定 `CNY`。usage_only 金额只能为零 |
| `reason_code` | 可选安全码：`settlement_evidence_missing`、`settlement_pending`、`amount_hidden`。仅描述对应的 unavailable、pending、hidden 状态，不携带内部错误或对象 |

当前服务端按消费可见性返回 visible；pending/hidden 由契约保留，只有有真实对应证据或策略时才返回。没有现成受理证据时使用 404，不新增请求跟踪。时间字段本版本省略。

金额来自同一数据库事务提交的客户效果快照，使用原有 8 位账务量化值。余额扣除 1 表示人民币 1 元，不换汇；订阅消耗和 Key 窗口计量分别表达，不与余额扣款相加。usage 统计原值不代替实际入账金额。已知免费请求可返回字符串 `"0"`；SimpleMode 没有账务证据及失败诊断中的零值使用 unavailable。

接口读取明确 `sync` 或 `async` scope 的单次图片记录。单异步提交在执行前将公开引用持久保存到任务；202 响应与后续任务 GET 返回同一 `request_ref`，恢复方法见[任务与批量恢复契约](IMAGE-TASKS-CONTRACT.md)。任务完成、处理失败或结果转存失败时，费用均取真实结算快照；Redis 任务到期后仍可按已保存引用读取账务证据。

旧 Images 记录的同步/异步路径已被同样规范化，缺 scope 时保持缺证未知；其他明确 scope 返回 404。

历史可归属图片记录缺少充分结算快照时返回 unavailable / settlement_evidence_missing，省略金额和来源。重复请求及日志恢复不覆盖已提交快照；归档保留相同金额、来源和归属。

## 读取与错误

- 同时限制当前用户、当前 Key 和图片范围。未知引用、尚未出现的记录、其他用户、同用户另一个 Key、非图片对象统一 404。
- 有效身份在余额、Key 额度或订阅额度耗尽后仍可读自己的回执；Key/用户禁用、Key 过期及 IP 限制保持。
- 404 表示此次未取得回执；内部读取错误返回 503。错误体只含安全 code/message。
- 该后端兼容接口保留原只读语义；Connector的生成、编辑、任务查询不再调用它。图片和原请求引用继续交付，结果未知时不重新提交。

B1 离线 `FeeReceipt` 保持原结构及使用条件。服务端兼容协议规定正式版本、请求引用、CNY、结算来源及状态组合；使用该接口的兼容调用方按此契约校验。响应只保留客户金额，不包含倍率、成本、账号/渠道 ID 或上游别名。Connector不调用或向Agent输出该费用响应。
