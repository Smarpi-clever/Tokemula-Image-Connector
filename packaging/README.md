# 构建内测分发包

此目录是发行维护者的构建入口，不随包要求普通用户安装 Python、Rust 或 OpenSSL。运行时只有两个原生程序；页面、Skill、DeepSeek 薄插件/恢复脚本和 WorkBuddy 元数据已内嵌。

## 在本机生成两个真实版本

准备本机匹配的 Rust 工具链、已锁定 Cargo 依赖及 OpenSSL 3。把测试签名材料放在仓库外的私有目录：

```sh
python3 packaging/package.py test-key \
  --private-key /absolute/private-0700/test-ed25519.pem \
  --public-key /absolute/internal-packages/test-trust-public-key.txt

python3 packaging/package.py build --version 0.1.0-d2a.1 \
  --target aarch64-apple-darwin \
  --output-dir /absolute/internal-packages/candidate-r1 \
  --signing-key /absolute/private-0700/test-ed25519.pem

python3 packaging/package.py build --version 0.1.0-d2b.1 \
  --target aarch64-apple-darwin \
  --output-dir /absolute/internal-packages/candidate-r1 \
  --signing-key /absolute/private-0700/test-ed25519.pem
```

每次复制公开源码到独立临时根，修改该副本的真实 Cargo package version 后 release 编译，实际运行两个程序的 `--build-info` 核对产品、版本、目标和程序身份；只改清单不能通过。副本的唯一 Skill 增加非业务 `metadata.internal_build`，使内测 A/B 的受管 Skill 更新与回退可观察，名称、描述、正文保持不变。原工作树版本和 Skill 不改。

可用 `--target-dir /absolute/owned/build-cache` 在两次构建间复用依赖编译，最后由调用方清理。依赖版本保持原 lock；只有副本本地 package version 可更新。已有同名发行物不会覆盖，修复后换新的 candidate 目录并重新通知验收消费者。

## 产物与验证材料

每个包名为 `tokemula-image-connector-<version>-<system>-<arch>.tar.gz`，随同生成 `.sha256` 和 `.metadata.json`。后者记录两个真实程序的信息、临时源的两项差异、第三方清单数量、包校验值和公共测试信任。私钥不会进入包、源码、正式信任根或日志。

包内 `release.json` 列出全部普通负载文件的 path/size/SHA-256，`release.sig` 对其原始字节签名。使用原生 Ed25519 签名而非仅校验和。无符号链接/硬链接/绝对归档路径；运行端仍独立核对安全解包、可信签名和实际程序版本。

需要构造本地测试更新清单时，先写原始 payload JSON，再运行：

```sh
python3 packaging/package.py sign --input /absolute/update-payload.json \
  --output /absolute/signed-update.json \
  --signing-key /absolute/private-0700/test-ed25519.pem
```

输出 `{payload, signature}`，两字段为原 JSON 字节及 Ed25519 签名的 base64。测试材料仅用于本批 loopback 更新源；正式渠道、公钥分发和长期密钥另行准备。

`third_party.py` 从指定目标的真实 normal/build 依赖图复制发布的许可/NOTICE 与 Rust 标准库说明到 `THIRD-PARTY/`；缺失、未知 SPDX 或越界来源会失败，不以包名列表替代许可文本。

## 其他平台构建入口

`--target` 默认本机 Rust host。Windows/Linux 在相应原生环境安装所需开发工具后可使用相同构建入口和各自 Rust target；Windows 文件名带 `.exe`，Linux 保留所选 GNU/musl target。构建入口会实际运行候选程序，不能在不能执行目标二进制的交叉主机上声称完成分发验证。

本批 macOS arm64 的实际结果由私有 `D2-D3-packaging-RESULT.md` 和统一验收记录给出。Windows/Linux 原生运行、文件占用、系统包及生命周期仍待对应机器验收；此脚本不扩大已验证平台范围。
