# MClaw 移动云盘 · Agent2API 插件

Agent2API 的独立 MClaw 移动云盘插件。提供扫码登录或手动导入、模型目录、流式/非流式对话和移动云盘权益查询；上游不支持的签到和凭证续期不对网关宣告。

### 登录和使用

在 Agent2API 侧栏打开「MClaw 登录」，用自己的移动云盘账号扫码，或按表单手动导入手机号和登录 Token。返回账号页查询余额，在模型管理拉取模型。账号由网关持有；插件的公开视图不包含 Token。插件不会导入旧版独立网关的配置、账号或历史数据。

来源及参考协议文件见 [PROVENANCE.json](PROVENANCE.json)。扫码功能参考同作者的 `139cloudDiskAutoSave`；随包分发 QR 渲染器的依赖许可。

## 下载与安装

- **按版本下载**：[Releases](https://github.com/wyq2822875262/agent2api-mclaw/releases)。
- **按提交下载**：[Actions](https://github.com/wyq2822875262/agent2api-mclaw/actions/workflows/build.yml) → 选择成功的运行 → Artifacts。下载后解开 Actions 的外层压缩包，再把里面的 `agent2api-…-<平台>.zip` 导入 [Agent2API](https://github.com/aimod-cc/Agent2API) 的插件管理；旁边的 `.sha256` 可用于校验。
- 包按**网关所在机器**选择：Windows x64、macOS Apple Silicon（arm64）/ Intel（x64）、Linux x64 / ARM64（GNU/glibc，CI 使用 Ubuntu 22.04）。这些包未作 Agent2API 插件签名。

## 从源码构建

推荐使用与 CI 相同的 Rust 1.93.1 和 Python 3.12。在目标平台执行，`TARGET` 替换为 `pack.json` 中的目标三元组：

```sh
TARGET=x86_64-unknown-linux-gnu
cargo test --locked -p agent2api-mclaw --target "$TARGET"
cargo build --locked -p agent2api-mclaw --target "$TARGET"
python3 scripts/test.py --target "$TARGET"
cargo build --locked --release -p agent2api-mclaw --target "$TARGET"
python3 scripts/package.py --target "$TARGET"
```

打包输出为 `dist/` 下的 ZIP 和 SHA-256 文件。测试只使用本地模拟服务，不需要真实账号；通过模拟测试不代表已与真实服务联调。MClaw 的额外扫码协议集成测试在 macOS/Linux 上运行，需要 `openssl` 命令；Windows 运行 Rust 单元测试和基础 RPC 集成测试。

## 维护

普通 push 自动检查并为五个平台生成构建产物；也可在 Actions 手动运行。需要发布稳定下载时，同步更新 `crates/mclaw/Cargo.toml`、`Cargo.lock` 和 `pack.json` 后推送 `v<版本号>` 标签。标签对应的五个平台都构建成功之后，流程才会发布 GitHub Release 并附上 ZIP 和校验文件。三个插件独立构建、独立发布。

许可：AGPL-3.0（见 [LICENSE](LICENSE)）。锁定的 SDK / Protocol 来源见 [UPSTREAM.md](UPSTREAM.md)。
