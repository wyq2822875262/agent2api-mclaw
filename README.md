# MClaw 移动云盘 · Agent2API 插件

提供模型目录、流式与非流式对话，以及移动云盘 `aitokens` 权益查询。

在「账号 → 添加账号 → MClaw 移动云盘」选择「手机号 + Token」或「Basic / Cookie 导入」，填写自己的移动云盘凭证。云盘登录过期后需要更新凭证。插件能重新获取模型 API 密钥，云盘登录 Token 仍由移动云盘签发。

v3.5.1 的宿主不为第三方 WASM 插件开放自定义登录页，而且内置二维码交互仅对指定提供商开放。原侧栏扫码入口由凭证导入表单接替。来源及协议参考见 [PROVENANCE.json](PROVENANCE.json)。

## 安装与升级

在 Agent2API v3.5.1 的「插件管理」中导入 `agent2api-mclaw-0.1.3-wasm32-unknown-unknown.zip`，再到「模型管理」刷新目录。安装包可跨系统使用。WASM 包沿用原包 ID、提供商 ID 和账号凭证结构。升级时直接导入新版 ZIP。

从 [GitHub Releases](https://github.com/wyq2822875262/agent2api-mclaw/releases/latest) 下载安装包和 SHA-256 校验文件。每次推送都会触发 GitHub Actions 构建和测试。推送与插件版本匹配的 `v` 标签时，Actions 会把同一次构建生成的文件发布到对应 Release。 [构建记录](https://github.com/wyq2822875262/agent2api-mclaw/actions/workflows/build.yml)

## 构建与验证

需要 Rust 1.93.1、Python 3.11+ 和 Node.js 20+。

```sh
rustup target add wasm32-unknown-unknown --toolchain 1.93.1
cargo +1.93.1 test --locked --manifest-path wasm/Cargo.toml
cargo +1.93.1 build --locked --manifest-path wasm/Cargo.toml --release --target wasm32-unknown-unknown
node scripts/verify-wasm.mjs
python3 scripts/package.py
```

离线测试执行编译后的 WASM 模块，通过模拟宿主接口和上游响应验证关键调用。`pack.json` 定义入口和提供商，`wasm.json` 声明宿主能力与上游域名。旧版原生实现位于 `crates/`，供协议参考。

许可为 AGPL-3.0。SDK 来源见 [UPSTREAM.md](UPSTREAM.md)，许可见 [LICENSE](LICENSE)。
