# 参与贡献

欢迎通过 Issue 报告问题，或提交范围清楚的 Pull Request。开始较大的功能前，先说明使用场景和预期结果，方便确认方向。

## 本地开发

源码使用 `rust-toolchain.toml` 指定的 Rust 工具链。安装和使用发行包无需 Rust；修改源码时需要 Rust，打包测试还需要 Python 3.11+ 和 OpenSSL 3。

从 `main` 创建自己的工作分支，修改后运行：

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
python3 -m unittest discover -s packaging -p 'test_*.py'
```

这些检查覆盖代码格式、锁定依赖下的编译，以及使用合成文件的打包与安装器边界。修改运行行为时，补充对应的合成或本机 loopback 测试。macOS 客户端接入、系统凭据和真实图片调用分别验收；使用独立测试目录和测试凭据。

## 提交 Pull Request

- 说明解决的问题、实际变化和运行过的检查。
- 保留现有客户端配置、图片和已保存设置；更新受影响的教程或示例。
- 不提交 API Key、私钥、系统凭据、个人图片或含真实业务数据的日志。发布包与签名材料放在源码仓库之外。

安全问题按 [SECURITY.md](SECURITY.md) 中的私密报告流程处理。
