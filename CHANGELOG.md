# 更新记录

## [0.1.0-wb1.4] - 2026-11-22

- 增加标准 MCP Apps 图片视图，WorkBuddy 可直接展示本次生成和编辑原图，用户点击后再保存。
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

[0.1.0-wb1.4]: https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/tag/v0.1.0-wb1.4
[0.1.0-wb1.3]: https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/tag/v0.1.0-wb1.3
