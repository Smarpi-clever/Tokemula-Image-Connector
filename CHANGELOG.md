# 更新记录

## [0.1.0-wb1.6] - 2026-11-22

- 同步生成和编辑新增显式 `result_mode=files`，持久原图照常保存，仅返回文件路径与元数据；省略参数仍沿用原图块交付。
- 兼容同时提供 URL 与 base64 的图片结果：非空 base64 优先校验，空值才取 URL；损坏内容如实报错、不重投。
- WorkBuddy 普通图片愿望默认走 Tokemula，仅用户明确排除或指定其他图片服务时例外；不可用或失败不自动换服务。
- WorkBuddy 默认直接在聊天正文显示本次完整原图，保留同一文件入口；用户要求时再打开原生预览。
- 增加非敏感 stderr 分段计时，保留八工具、默认参数、异步/批量、旧 inline、编辑另存及桌面原图长期保留合同。

## [0.1.0-wb1.5] - 2026-11-22

- Mac 默认把完整原图保存到桌面 Tokemula-Images，编辑另存、不覆盖旧文件，多图按返回顺序交付。
- WorkBuddy 通过配套 Skill 在聊天正文内嵌本次原图，并自动打开原生预览、保留文件入口；正常重启后可以回看同一文件。
- 桌面图片无自动过期，缓存清理、升级与卸载（含 purge）均保留；移动或删除后明确报告路径失效。
- 保留 Codex 原有展示、八工具、Key 和默认图片设置；结果未知或展示失败时不重新生成。

## [0.1.0-wb1.4] - 2026-11-22

- 增加标准 MCP Apps 图片视图；后续真实 WorkBuddy 聊天未能自动显示，wb1.5 改用持久原图与原生展示。
- 大图通过内存资源分块显示完整原图，保持原始字节、格式和尺寸；未协商 MCP Apps 的客户端沿用原交付方式。
- 修正失败或无图时的视图清空与高度回缩，保留八工具、默认设置和原请求恢复方式。
- 更新 Mac 测试包验证公钥；旧版升级保留 Key 和图片偏好，按安装指南重新接入客户端。

## [0.1.0-wb1.3] - 2026-11-21

首个公开 Mac 测试版，支持 macOS Apple Silicon（M 系列）。

- 提供独立原生 stdio MCP 程序及管理命令，接入 Codex 和 WorkBuddy AI 5.11.4。
- 安装入口自动下载、校验程序包，并注册 MCP 和配套 Skill。
- 在本机连接页配置 Key、默认模型、备用顺序和图片偏好，多客户端共用设置。
- 提供图片生成、原图编辑、模型查询及任务与批量接口。同步生成、编辑、显示和保存已完成真实使用验收。
- 支持保留设置的卸载、指定客户端移除和已安装版本恢复。

[0.1.0-wb1.6]: https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/tag/v0.1.0-wb1.6
[0.1.0-wb1.5]: https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/tag/v0.1.0-wb1.5
[0.1.0-wb1.4]: https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/tag/v0.1.0-wb1.4
[0.1.0-wb1.3]: https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/tag/v0.1.0-wb1.3
