# 安全报告

## 报告漏洞

请使用 GitHub 的[私密漏洞报告](https://github.com/Smarpi-clever/Tokemula-Image-Connector/security/advisories/new)，提交复现步骤、受影响版本和预期行为。报告通过私密方式发送给维护者。

请使用合成图片、测试数据和脱敏后的错误信息说明复现步骤。真实 API Key、私钥、系统凭据、个人图片和原始业务日志保留在自己的设备上，报告只附定位问题所需的脱敏片段。

## 当前发行范围

当前公开测试包为 `0.1.0-wb1.3`，支持 macOS Apple Silicon（M 系列）。漏洞报告请注明程序版本、系统版本、客户端版本和复现条件。

程序使用系统凭据库存储 Key；本机连接页和管理命令不要求把 Key 发到聊天或写进 MCP 配置。发行包包含签名清单，安装入口会验证下载内容；安装与系统确认说明见[安装教程](https://docs.tokemula.com/image-connector/)。
