# Changelog

记录本仓库的**破坏性变更与行为变更**。格式参考
[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循
[语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

> 相对已打 tag 的 `v0.3.0`。建议按 0.x 语义升 **`0.4.0`**（新增默认开启的行为）。

### 新增

- **自动重试**（默认开启）：按「这次失败在微信侧到底有没有被处理」分类，只重试
  **确定没送到**（连接被拒 / 连接超时）、**微信明确未受理**（429 / 500 / 502 / 503，
  以及错误码为 `SYSTEM_ERROR` 的 2xx 错误信封）与**已受理但未处理完**（202）的失败。
  措辞取自官方 HTTP 状态码页：429「请求未受理」、502/503「请求无法处理」、
  各接口的 500「请用相同参数重新调用」、202「请使用原参数重复请求一遍」。见 `src/retry.rs`。
- `RetryPolicy`（次数 / 退避 / 抖动）+ `WechatPay::with_retry`。退款走**独立**的分钟级策略
  （`RetryPolicy::for_refund`：最多 2 次尝试、首次退避 60s、**不加抖动**）—— 官方对退款
  重试的要求是「间隔 1 分钟」，且该接口在**失败时**报错限流只有 6QPS，秒级退避打过去基本
  是白打；这一档关掉抖动是因为抖动**向下**取随机，等于把「至少间隔 1 分钟」变成
  「可能立刻重发」。用 `with_refund_retry` 单独覆盖或关闭。
- `PayError::may_have_taken_effect()`：区分「结果未知 / 可能已生效」（读写超时、响应体没读
  完、202、响应体解析失败）与「确定没生效」，供调用方决定是否该去查单，而不是换个单号重开。

### 行为变更

- **新增的重试只覆盖三类有官方依据的失败**：确定没送到、微信明确未受理、202。
  **写接口超时（结果未知）不重试** —— 官方口径是先用查单接口确认状态；只读接口
  （查单 / 退款查询 / 平台证书）超时照常重试，因为重放纯读没有副作用。
- **429 / 500 / 502 / 503 现在会自动重试**，这会改变既有错误路径的可观察结果：
  多次尝试后返回的是**最后一次**尝试的错误体（原先只发一次、返回第一次的响应体）。
  既有测试中依赖「单个响应体」的用例已显式关掉重试以隔离关注点。
- **关单不再把「HTTP 200 + 错误信封」当成成功**。走 `request_no_content` 的路径原先根本
  不看响应体，微信以 200 返回 `{"code":"RULE_LIMIT",...}` 时会静默返回 `Ok(())` ——
  尽管 README 一直承诺「包括 HTTP 200 但 body 是错误信封，都返回 Err」。现在错误信封在
  传输层就被归一成 `Err`，与非 2xx 走同一条判定。
- **202 的去向变了**：以前被当成 2xx 成功放行，再拿空 body 去解析，报出一个与真实原因
  无关的 JSON 错误；现在按官方要求重发，重试用尽后降级成带状态码 202 的 `ApiError`。
  注意 202 意味着**请求已被接收、可能随后生效**，所以 `may_have_taken_effect()` 对它返回
  `true` —— 拿到它应当去查单，而不是换个单号重开。
- 每次重试都**重新签名**（新 nonce + 新 timestamp）：重试可能跨过 5 分钟的签名有效窗口，
  复用旧签名会直接 401。
- `async` feature 新增对 `tokio`（仅 `time`）的依赖，用于退避的异步 sleep。

## [0.3.0] - 2026-09-11

> **不发布到 crates.io。** 本仓库是 fork —— `wechat-pay-rust-sdk` 这个名字在 crates.io 属于上游，
> `Cargo.toml` 已设 `publish = false`，引入方式见 README「引入依赖」（git / path）。
> 版本号仍按语义化版本管理：相对上游 `0.2.21` 含破坏性改动，故取 `0.3.0`。

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
- 离线测试网 `tests/offline.rs`（30 个带断言的用例，不需要凭证）+ GitHub Actions CI。
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
