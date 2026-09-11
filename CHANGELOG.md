# Changelog

记录本仓库的**破坏性变更与行为变更**。格式参考
[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循
[语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

> ⚠ **尚未发布。** `Cargo.toml` 的版本号仍停在 `0.2.21`，但下列变更含多处破坏性改动，
> 按语义化版本应发布为 **`0.3.0`**。发版时请先 bump `version`，再把本节移到新的版本号下。

### 破坏性变更

- **`refunds()` 返回类型变更**：`Result<WeChatResponse<RefundsResponse>, PayError>` →
  `Result<RefundsResponse, PayError>`。失败不再以「带内错误」形式返回。
- **删除 `WeChatResponse<T>`**：在新契约下它的 `Err` 变体永不可达，留着只会让人以为仍需手工
  检查 `is_success()`；`is_success()` / `ok()` / `err()` 一并移除。
- **`WechatPay` 字段私有化**：原先全部是 `pub`（`private_key` / `v3_key` / `base_url` 都不该被
  外部随手改写）。要覆盖网关地址请用 `with_base_url`。
- **`PayError` 新增变体**：`ApiError`、`UnknownPlatformSerial`、`StaleNotify`。
  对 `PayError` 做穷尽 `match` 的代码需要补分支（用 `kind()` 可做粗分类）。
- **`ErrorResponse` 新增 `detail` 字段**：以结构体字面量构造它的代码需要补该字段。
- **`cargo test --doc` 不再是受支持的命令**：README 作为 crate 文档，但其中示例需要真实密钥与
  公网，已设 `[lib] doctest = false`。

### 行为变更

- **非 2xx 一律返回 `Err`**。此前错误体会被解析进字段全为 `Option` 的成功类型，
  下单失败会伪装成 `Ok(JsapiResponse { code: Some("PARAM_ERROR"), prepay_id: None })`。
- **2xx + 错误信封也返回 `Err`**。微信偶尔以 HTTP 200 返回 `{"code","message","detail"}`，
  现在会被识别（判据：顶层出现非空字符串 `code`；`code: null` / `""` 不算）。
- **HTTP 客户端改为复用**：连接池跨请求共享；此前每个请求都 `Client::new()`。
- **新增超时**：connect 5s / request 10s / pool idle 90s，可用 `with_timeouts` 覆盖。
  此前没有超时，服务端不响应会一直挂住。
- **回调验签新增防重放**：`PlatformKeys::verify_notify` 校验 ±300s 时间戳窗口。
  此前 `verify_signature` 只验签名，抓到一次合法回调即可无限重放。
- **平台证书按 `Wechatpay-Serial` 选键**：轮换期新旧两张并存，不再只认单张
  （此前轮换期会导致回调验签全部失败 → 订单不发货）。
- 格式错误的 2xx 响应体现在归类为 `JsonError`（此前是 `RequestError`）。

### 新增

- 订单查询 `query_order`、关单 `close_order`、退款查询 `query_refund`。
- 平台证书管理：`cert::PlatformKeys` + `WechatPay::fetch_platform_keys`（含刷新窗口）。
- 回调防护：`notify::NotifyHeaders`（含 `from_pairs`）+ `PlatformKeys::verify_notify`。
- 错误三层归类：`PayError::kind() -> ErrorKind`（Network / Api / Local）。
- `HttpTimeouts` 与 `WechatPay::with_timeouts` / `timeouts()`。
- 离线测试网 `tests/offline.rs`（25 个带断言的用例，不需要凭证）+ GitHub Actions CI。
- 仓库指南 `AGENTS.md`、归属声明 `NOTICE`、本 `CHANGELOG.md`。

### 修复

- **错误吞噬**：下单失败曾被当作成功返回。
- **非微信形状的错误体被静默丢弃**：现在保留原文（超长截断、空体有显式标记）。
- **许可证元数据不一致**：`Cargo.toml` 声明 MIT 而 `LICENSE` 是 Apache-2.0 ——
  已统一为 Apache-2.0（代码实际收到的许可），并补齐版权署名。
- **`cargo check --features async --all-targets` 编译失败**：7 个测试函数缺少 cfg 门。
- **16 个冒烟测试在干净检出上必然失败**：已标记 `#[ignore]`，`cargo test` 转绿。

### 其他

- 声明 MSRV **1.89**。`rust-version` 同时让 cargo 优先挑选兼容该版本的依赖 ——
  本仓库不提交 `Cargo.lock`，不声明的话实际 MSRV 会随解析漂移（实测 1.85 / 1.88 均失败）。
- **删除 22 处冗余 `unsafe impl Send/Sync`**，并加 `#![forbid(unsafe_code)]` 与编译期
  `Send + Sync` 断言。
- 公开 API 全部补齐文档注释（原缺 179 处），并以 `missing_docs` + CI `-D warnings` 作硬门槛。
- 全仓 rustfmt 化，CI 增加 `cargo fmt --all -- --check`。
