# Repository Guidelines

## Project Overview

`wechat-pay-rust-sdk` (v0.3.0) is a Rust client for the **WeChat Pay v3 HTTP API**. It wraps the
payment endpoints (native / H5 / JSAPI / app / micropay), refunds, platform-certificate download,
and callback (notify) decryption + RSA signature verification behind one `WechatPay` handle.

Single library crate at the repo root, plus one workspace member binary (`example/`, an actix-web
notify server). No CLI, no service, no generated code.

- **MSRV: 1.89**（`rust-version = "1.89"`，edition 2024）。CI 有独立的 MSRV 作业用 1.89.0 跑
  `cargo check --all-targets`。⚠ 声明 `rust-version` 不只是记录下限 —— 它会让 cargo
  **优先挑兼容该版本的依赖**。本仓库按库的惯例不提交 `Cargo.lock`，不声明的话实际 MSRV
  会随解析漂移（实测 1.85 / 1.88 都曾因 `icu_*`、`time`、`aes` 的新版本而编译失败）。No `rust-toolchain.toml`.
- **License: Apache-2.0.** The package previously declared `license = "MIT"` in `Cargo.toml` while
  shipping the Apache-2.0 license text. This fork standardised on Apache-2.0 — the license the code was
  actually received under — and filled in the copyright notice; original author attribution is in
  `NOTICE`. **Do not set the license field back to MIT:** the upstream copyright holders never granted
  MIT, and a downstream party cannot relicense code it does not own.

## Architecture & Data Flow

One implementation serves **both** the blocking and async API. `maybe-async` rewrites the source at
compile time — the `async` feature selects the shape:

```rust
// src/async_impl/pay.rs:28-31
#[cfg(feature = "async")]        use maybe_async::maybe_async as maybe_async_attr;
#[cfg(not(feature = "async"))]   use maybe_async::must_be_sync as maybe_async_attr;

#[maybe_async_attr]
pub async fn native_pay(&self, params: NativeParams) -> Result<NativeResponse, PayError> { ... }
```

`must_be_sync` strips `async`/`.await`; `maybe_async` keeps them. **Never hand-write a sync twin of a
method** — edit the one function in `src/async_impl/pay.rs` and both modes change together.

Request flow (`pay()` is the single generic path):

```
caller ──▶ WechatPay::pay::<P: ParamsTrait, R: ResponseTrait>(method, url, params)
             │  params.to_json()                       # src/model.rs: ParamsTrait
             │  inject appid / mchid / notify_url      # from WechatPay config, NOT from the model
             ▼
        WechatPay::request(method, url, body)          # ⚠ 不注入任何字段
             │  build_header(method, url, body)        # RSA-SHA256 sign → Authorization header
             │  self.client.{get,post,…}(base_url + url).body(body).send()
             ▼
        send_and_check(builder)   # 先取 status + text
             ├── 非 2xx → Err(PayError::ApiError { status, response })
             └── 2xx    → (status, text) 交给 request_json / request_no_content
                            └── request_json 再查一次「2xx + 错误信封」
```

三层请求结构（都在 `src/async_impl/pay.rs`）：

| 方法 | 职责 |
| --- | --- |
| `send_and_check` | 收 builder，检查状态码，返回 `(状态码, 响应体文本)` |
| `request` / `request_json` / `request_no_content` | 签名 + 发送；**不注入字段** |
| `pay` / `get_pay` / `refunds` / `query_order` / `close_order` / `query_refund` | 各端点 |

⚠ **失败有两条伪装路径，两道检查缺一不可。**

1. **非 2xx + 错误体**（多数情况）：若直接 `.json::<R>()`，错误体会被解析进字段全为 `Option`
   的成功类型，下单失败伪装成 `Ok(JsapiResponse { code: Some("PARAM_ERROR"), prepay_id: None })`。
   由 `send_and_check` 的状态码检查挡住。
2. **2xx + 错误信封**（微信偶尔为之）：状态码是 200，但 body 是 `{"code","message","detail"}`。
   成功类型的 `Option` 字段同样照单全收。由 `is_error_envelope` 在 `request_json` 里挡住 ——
   判据是**顶层出现非空字符串 `code`**；本 crate 解析的成功响应都不含顶层 `code`，
   `code: null` / `code: ""` 也不算信封（有专门的反向测试）。

**HTTP 客户端由 `WechatPay` 持有并复用**（连接池跨请求共享），不要退回 `Client::new()` 每请求新建。
超时由 `HttpTimeouts` 配置，默认 connect 5s / request 10s / pool idle 90s，用
`with_timeouts` 覆盖。⚠ **超时不代表操作没发生**：请求可能已被微信受理，支付类接口超时后
必须用 `query_order` 确认状态，不能直接重试下单。

⚠ **`request()` 有意不注入 `appid`/`mchid`/`notify_url`** —— 关单的请求体只需要 `mchid`、
查单的 `mchid` 要放在 query string，无脑注入这三个字段会让这些端点直接失败。注入只发生在
`pay()` 里。`request_no_content` 用于 **204 No Content**（关单），它不解析 body。

⚠ GET 的**查询串必须拼进传给 `request` 的 `url`**：微信签名串第二行是 path + `?` + query。
`query_order` 就是这么拼的，测试会独立重建签名串验签来守住这条。

Post-processing happens in the thin per-endpoint wrappers, e.g. `app_pay` / `jsapi_pay` / `micro_pay`
attach `sign_data` via `WechatPayTrait::mut_sign_data(prefix, prepay_id)` — the prefix differs
(`""` for app, `"prepay_id="` for JSAPI/micropay).

Two exceptions to the generic path: `get_pay()` (no body, GET) and `refunds()`, which hand-rolls its
request instead of calling `pay()` and returns `WeChatResponse<RefundsResponse>`.

Crypto is centralized in `src/pay.rs` traits:

- `WechatPayTrait` — config accessors, `rsa_sign`, `now_timestamp`, `nonce_str`, `mut_sign_data`.
- `PayNotifyTrait: WechatPayTrait` — `verify_signature`, `decrypt_paydata`, `decrypt_bytes`
  (AES-256-GCM, keyed by `v3_key`).
- `impl PayNotifyTrait for WechatPay {}` — both traits are blanket-implemented only for `WechatPay`.

## Key Directories

```
src/pay.rs            WechatPay（含复用的 Client 与 HttpTimeouts）、两个 trait、from_env()、build_header()
src/async_impl/       ⚠ name is misleading — this is the ONLY HTTP layer and serves sync + async
  pay.rs              send_and_check → request* → 各端点（pay / *_pay / refunds / query_order / close_order / query_refund / certificates / get_weixin）
  mod.rs              `pub mod pay;` (not feature-gated, always compiled)
src/model.rs          request params + notify models (Serialize); ParamsTrait
src/response.rs       response types (Deserialize); ResponseTrait, TransactionResponse, ErrorResponse
src/cert.rs           PlatformKeys：serial_no → 公钥 PEM 索引 + fetch_platform_keys（平台证书轮换）
src/notify.rs         NotifyHeaders + verify_notify：时间戳新鲜度 → 按 serial 选键 → 验签（防重放）
src/sign.rs           sha256_sign — RSA PKCS#1 v1.5 + SHA-256, base64 STANDARD
src/util.rs           base64 助手、random_trade_no、verify_rsa_sha256、x509_to_pem、x509_is_valid
src/error.rs          PayError (thiserror); src/request.rs HttpMethod; src/pay_type.rs PayType
src/retry.rs          RetryPolicy / 失败分类 Delivery / 可重试判定 / 退避计算
src/macros.rs         crate-local `debug!` / `error!` (no-ops unless `debug-print`)
example/              actix-web notify server; async usage reference
tests/offline.rs      离线集成测试：本地 mock 微信网关，无需凭证，CI 跑的就是它
.github/workflows/    CI：fmt + 3 种 feature 组合的 clippy -D warnings + 两种模式的 test + MSRV 1.89
NOTICE                Apache-2.0 归属声明（上游贡献者名单）
CHANGELOG.md          破坏性/行为变更记录（0.3.0 起）
```

没有 scripts、没有 `rustfmt.toml`、没有 clippy 配置文件。

## Development Commands

Run everything from the repo root (tests use CWD-relative fixture paths).

```bash
# 完整测试：tests/offline.rs 用本地 mock 服务替代微信网关，不需要凭证或公网
cargo test                    # PASS — lib 10 passed / 14 ignored，offline 40 passed
cargo test --features async   # PASS — 同一套测试在 async 模式下再跑一遍

# 两种 feature 模式都必须能编译（含测试目标）——这是防回归的关键两条
cargo check --all-targets
cargo check --features async --all-targets
cargo check -p example

# 零告警是硬门槛（含 missing_docs）。debug-print 会切换 macros 的 cfg 分支，
# 所以 CI 跑 3 种 feature 组合，本地至少要跑前两条。
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features async -- -D warnings
cargo clippy --all-targets --all-features -- -D warnings

cargo fmt --all -- --check    # 全仓已格式化，CI 有门禁
cargo build --release
cargo run -p example          # actix server on 0.0.0.0:8080

# 需要真实商户凭证的测试默认被 #[ignore]，显式运行：
cargo test --lib -- --ignored
```

**`cargo test` 现在是绿的，可以当作冒烟测试用。** 改动后请同时跑 sync 与 async 两遍。

⚠ 一条**有意不跑**的命令：

- `cargo test --doc` —— README 通过 `#![doc = include_str!("../README.md")]` 充当 crate 文档，
  其示例需要真实商户密钥与公网（还含 actix-web 片段与输出示例）。`[lib] doctest = false` 使普通
  `cargo test` 跳过它们，但**显式**传入 `--doc` 会强制编译 README 代码块并失败（20 个）。
  它不是本仓库受支持的命令；要改 README 示例请自行编译验证。

`cargo check --no-default-features` 会失败，属**既有问题**（`src/error.rs` 无条件引用
`reqwest::Error`，而 reqwest 是 optional 依赖），与本次改动无关。

## Code Conventions & Common Patterns

**Request models** (`src/model.rs`) — `Serialize` only (drop `Deserialize`), flat snake_case that
mirrors WeChat's JSON field names one-to-one. No `rename_all` anywhere; the only two exceptions are
`#[serde(rename = "type")] pub h5_type` (`model.rs:145`) and the `r#type` raw identifier
(`response.rs:250`). Every optional field carries
`skip_serializing_if = "Option::is_none"`. Each params struct implements `ParamsTrait::to_json()`:

```rust
impl ParamsTrait for JsapiParams {
    fn to_json(&self) -> String { serde_json::to_string(self).unwrap() }
}
```

**Injected fields:** do not add `appid` / `mchid` / `notify_url` to a params struct — `pay()`
injects them from `WechatPay` config on every request.

**Response models** (`src/response.rs`) — `Deserialize` only, all fields `pub`, plus a manual marker
impl: `impl ResponseTrait for NativeResponse {}`. Failures surface through
`WeChatResponse<T>` (`#[serde(untagged)]`) with `is_success()` / `ok()` / `err()`; check those instead
of matching the enum directly.

**Naming / ergonomics:** constructors are positional `Type::new(...)`; ergonomic scalars go through
`From` impls so call sites read `1.into()` (`AmountInfo`) and `"open_id".into()` (`PayerInfo`).
Follow this when adding a params type.

**Doc comments:** Chinese, copied from WeChat's field docs, using the `///【字段名】 说明` prefix.
Match this style for new model fields; write code identifiers in English.

**Error handling:** one flat `PayError` enum (`src/error.rs`) with `thiserror`; `#[from]` for
`reqwest::Error` / `serde_json::Error` / `base64::DecodeError`, `(String)` for the rest, and
`ApiError { status, response }` for WeChat business errors. There is no `Result` type alias —
functions are spelled `Result<T, PayError>`. Convert with `?` or `.map_err(...)`; don't `unwrap()` in
library code.

**API-error contract:** 所有 HTTP 路径在**非 2xx** 或 **2xx + 错误信封**时都返回
`Err(PayError::ApiError)`，并保留微信的错误体。按 `response.code` 分支，而不是从空字段去猜：

```rust
match wechat_pay.query_order("ORDER_1") {
    Err(PayError::ApiError { status, response }) => {
        // status: u16（错误信封情况下是 200）；response.code / message / detail 均为 Option
        if response.code.as_deref() == Some("ORDER_NOT_EXIST") {
            // 正常业务结果，不是故障
        }
    }
    Err(other) => { /* 网络 / 序列化 / 本地错误，见 other.kind() */ }
    Ok(order) => { /* … */ }
}
```

`refunds()` 返回 `Result<RefundsResponse, PayError>`（与 `query_refund` 同一个类型）。
原来的 `WeChatResponse<T>` 已删除 —— 它的 `Err` 变体在新契约下永不可达，留着只会让人误以为
还需要手工检查 `is_success()`。非 JSON 响应体（例如网关返回 HTML）会被原样放进
`response.message`（超长则截断），不丢线索。

`src/sign.rs` 与 `from_env()` 仍会 `expect()`/panic —— 属既有债务，不要照抄。

**错误分层：** `PayError::kind()` 返回 `ErrorKind`，把错误归为三层，供调用方决定处置策略：

| 归类 | 含义 | 处置 |
| --- | --- | --- |
| `Network` | 传输 / 连接层失败 | 可考虑重试；⚠ 下单接口不可无脑重试（会重复下单） |
| `Api` | 微信业务拒绝（HTTP 非 2xx） | 不重试，按 `response.code` 分支。`ORDER_NOT_EXIST` 是**正常业务结果**而不是故障 |
| `Local` | 签名、解密、Base64、JSON 解析、验签失败、回调超窗 | 不重试，通常是配置或数据问题，应当告警 |

`kind()` 用**穷尽匹配**，新增 `PayError` 变体时编译器会强制你在这里做出归类决定。

**Logging:** use the crate-local `debug!` / `error!` from `src/macros.rs`, which compile to no-ops
unless the `debug-print` feature is on. Prefer them over unconditional `tracing::debug!`.

**Adding an endpoint:** add one `#[maybe_async_attr] pub async fn` inside `impl WechatPay` in
`src/async_impl/pay.rs`, then pick the right transport helper — **不要一律套用 `pay()`**：

| 端点形态 | 用哪个 | 例 |
| --- | --- | --- |
| 下单类（需要注入 appid/mchid/notify_url） | `self.pay(method, url, params)` | `jsapi_pay` |
| 普通 JSON 请求（GET 或自定义 body） | `self.request_json(method, url, body)` | `query_order` / `certificates` |
| 返回 **204 无 body** | `self.request_no_content(method, url, body)` | `close_order` |

⚠ GET 的查询串必须拼进 `url`（参与签名）。新增 params/response 类型放到 `src/model.rs` /
`src/response.rs`，响应类型需要 `impl ResponseTrait for X {}`。

**`unsafe` 已全部清除。** 原先有 **22 处** `unsafe impl Send/Sync`（覆盖 `WechatPay`、`HttpMethod`、
`Currency`、`AmountInfo`、`PayerInfo`、`GoodsDetail`、`OrderDetail`、`SceneInfo`、`SettleInfo`、
`NativeParams`、`JsapiParams`），全是冗余的 —— 这些类型本来就自动满足 `Send + Sync`，手写的
`unsafe impl` 只会掩盖将来引入非 Send 字段的问题。现已全部删除，换成两道防线：

- `src/lib.rs` 的 `#![forbid(unsafe_code)]`：**编译期强制**，且 `forbid` 不可被内部 `allow` 覆盖
- `tests/offline.rs::public_types_are_send_and_sync`：编译期断言上述类型仍是 `Send + Sync`，
  谁给它们加上 `Rc` / 裸指针，测试会直接编译失败

**不要再为"保证" `Send + Sync` 写 `unsafe impl`。**

**Signing contract** (if you touch headers): `build_header` produces
`WECHATPAY2-SHA256-RSA2048 mchid="…",nonce_str="…",signature="…",timestamp="…",serial_no="…"` over
`"{method}\n{url}\n{timestamp}\n{nonce_str}\n{body}\n"`, with a nonce of the uppercase UUID without
dashes. Endpoint paths passed to `build_header`/`pay` are the `"/v3/..."` suffix only; `base_url` is
prepended (`https://api.mch.weixin.qq.com` by default, set in `WechatPay::new`).

## Important Files

| Path | Why it matters |
| --- | --- |
| `src/async_impl/pay.rs` | The single HTTP implementation — every endpoint lives here, for both sync and async |
| `src/pay.rs` | `WechatPay` definition, `from_env()`, `build_header()`, `WechatPayTrait`, `PayNotifyTrait` |
| `src/lib.rs` | `#![doc = include_str!("../README.md")]` — the README **is** the crate rustdoc（其 ```rust 代码块**不被编译**，见 `[lib] doctest = false`） |
| `src/model.rs` | All request/notify models + `ParamsTrait` |
| `src/response.rs` | All response models + `ResponseTrait` + `WeChatResponse<T>` + `TransactionResponse` |
| `src/cert.rs` | `PlatformKeys`：按 `serial_no` 索引平台公钥 + `fetch_platform_keys` 拉取/解密。**轮换期必须按 serial 选键** |
| `src/notify.rs` | `NotifyHeaders` + `verify_notify`：新鲜度 → 选键 → 验签。模块文档写明幂等必须由业务侧做 |
| `src/error.rs` | `PayError` — the single error type |
| `Cargo.toml` | Feature definitions; `default = ["reqwest/blocking"]`, `async`, `debug-print` |
| `example/src/main.rs` | actix-web notify server; the async API reference. Registers `/pay/notify`, `/pay/notify2` and `/` — `pay_notify3` is defined but never wired into `App`, and it pre-formats the signed message before passing it as `body`, which would double-format (`verify_signature` formats it itself) |
| `tests/offline.rs` | 离线测试网：手写单请求 HTTP mock + `dual_test!` 让同一份测试体在两种 feature 下各跑一遍 |
| `.github/workflows/ci.yml` | CI：两种 feature 模式的 test / check + clippy |
| `README.md` | Chinese usage guide; also the crate docs — its ```rust fences are **not** compiled (see `[lib] doctest = false`) |
| `NOTICE` | Apache-2.0 attribution for upstream contributors |

**测试用 mock：** `WechatPay` 字段全是 `pub`，因此可以把实例指向本地假网关 —— 这正是
`tests/offline.rs` 的做法（`wechat_pay.base_url = "http://127.0.0.1:PORT"`）。
⚠ 同一个特性也是安全脚枪：**生产代码绝不能让用户输入接触到 `base_url`**，否则你签好名的请求
（含 openid、订单信息）会被送到第三方域名。

Dead ends, so you don't chase them: `PayType` (`src/pay_type.rs`) is unused public API, and
`WechatPay::with_base_url` is private + `#[allow(dead_code)]` (the `pub` field is the real lever).
`get_weixin(h5_url, referer)` 对调用方传入的 URL 发 GET 且无白名单 —— 是 SSRF 面；`h5_url` 只能
来自你自己的服务端或微信返回的 `h5_url`，绝不能来自用户输入。

## Runtime/Tooling Preferences

- Plain **Cargo**, edition 2024, no MSRV pin and no toolchain file. `Cargo.lock` is gitignored
  (library convention) — don't commit one, and don't report its absence as a problem.
- Features are the only build knob and they change **public method signatures**, not just deps:

  | Build | Effect |
  | --- | --- |
  | default (`reqwest/blocking`) | sync API — `native_pay(...)` with no `.await` |
  | `features = ["async"]` | async API — `native_pay(...).await`, `#[tokio::test]` |
  | `features = ["debug-print"]` | real `tracing` output from `debug!`/`error!` + `open_debug()` |
  | `default-features = false, features = ["async"]` | async API without compiling reqwest's blocking client |

- **双模式（sync + async）是有意保留的，不要为了「简化」砍掉 sync**：`maybe-async` 让一份实现
  同时产出两种 API，现在两种模式都能编译、都有测试覆盖。砍掉会带来一次巨大的 diff、牺牲与上游的
  可合并性，而收益只是少写一点测试（`dual_test!` 宏已经处理了这件事）。
- ⚠ **The README's install section is stale.** It claims async is the default and that a `blocking`
  feature selects sync. **No `blocking` feature exists** (`cargo metadata` features are exactly
  `async`, `debug-print`, `default`, `reqwest`, `tracing`, `tracing-subscriber`), and the real
  default is sync. Do not add a `blocking` feature to satisfy the README — fix the README instead.
- `example/` is a workspace member but `exclude = ["example"]` keeps it out of the published package.
  It depends on the lib via `path = ".."` with `features = ["debug-print", "async"]`, so it exercises
  the async mode exclusively.
- Releases are just a `Cargo.toml` version bump in a Chinese commit message (e.g. `版本升级`,
  `添加退款申请`). No git tags, no CHANGELOG, no release automation — match the existing commit style.

## Testing & QA

**两层测试，都不需要凭证**，`cargo test` 在干净检出上就是绿的：

| 层 | 位置 | 内容 |
| --- | --- | --- |
| 离线集成测试 | `tests/offline.rs` | 用 `Mock`（手写 HTTP 服务，支持 keep-alive 与连接计数）替代微信网关；40 个带断言的用例（含编译期 Send/Sync 断言） |
| 纯逻辑单测 | `src/pay.rs`、`src/async_impl/pay.rs`、`src/retry.rs` | 签名/加解密助手等无外部依赖用例；`retry.rs` 覆盖退避曲线、抖动边界与可重试判定矩阵 |

**`tests/offline.rs` 覆盖的契约（改这些行为必须同步改测试）：**

- Authorization 头格式 `WECHATPAY2-SHA256-RSA2048 mchid="…",nonce_str="…",signature="…",timestamp="…",serial_no="…"`
- 签名串 `"{method}\n{url}\n{timestamp}\n{nonce}\n{body}\n"` —— 测试**独立重建并验签**，不复用 SDK 自己的实现
- **GET 的签名必须覆盖 query string**（`query_order` 的 `?mchid=…`）
- `pay()` 注入的 `appid` / `mchid` / `notify_url` 来自配置而非入参；`close_order` 的请求体**只有** `mchid`
- **非 2xx → `Err(PayError::ApiError)`，且 `code` / `message` / `detail` 不丢失**
- 非 JSON / 非微信形状的错误体原样保留、超长体截断、空体有显式标记
- **关单 204 无 body 必须被当作成功**（走 `request_no_content`）
- AES-256-GCM 回调解密往返
- 回调验签：合法通过 / 篡改 body 失败 / 非法签名失败
- **平台证书轮换**：轮换期两张证书都进索引；按 `Wechatpay-Serial` 选键；未知 serial 报 `UnknownPlatformSerial`
- **防重放**：±300s 时间窗（含边界）、非法时间戳拒绝
- **错误信封**：2xx + `{"code":…}` → `Err`；且成功响应不被误判（含 `code: null` / `code: ""` 两种负例）
- **连接复用**：同一客户端两次请求只占 1 条 TCP 连接；独立客户端占 2 条（后者同时证明计数不是恒为 1）
- **超时**：只接受连接不回响应的服务端会触发超时（`reqwest::Error::is_timeout()`）而不是挂死
- **`sign_data` 前缀**：APP 支付的 `package` 是裸 `prepay_id`，JSAPI / 付款码必须带 `prepay_id=` 前缀（历史回归点，见提交 `95cf80a`）
- **申请退款成功路径**：解析出退款单并断言 `status == PROCESSING`（受理 ≠ 成功）
- **`get_weixin`**：能从 H5 页面抠出 `weixin://` 链接、带 Referer、页面无链接时报 `WeixinNotFound`
- **X.509 助手**：`x509_to_pem` 取出的公钥必须能验证配套私钥的签名（不只是「长得像 PEM」）；非 PEM 输入返回 `Err` 而非 panic
- **重试触发**：429 / 503 会被重试，且断言 mock **实际收到的请求次数**（`max_attempts` 用尽即停）
- **重试的安全边界（关键）**：写接口**超时不重试**、只读接口超时重试；「响应体没读完」这类
  **已处理**的失败，写和读都**不重试**（`is_decode()` 而非 `is_body()`）；连接被拒时连写接口也重试
- **重试后的请求是新签的**：两次尝试的 `nonce_str` 必须不同（复用旧签名会跨过 5 分钟窗口后 401）
- **`202 Accepted`**：会按官方要求重发；重试用尽时降级成带状态码 202 的 `ApiError`，而不是 JSON 解析错误
- **退款策略独立**：退款退避是分钟级且与通用策略互不牵连（`with_refund_retry` 不改变 `retry_policy`）

**写新测试请用 `dual_test!`** —— 一份测试体在两种 feature 下各生成一个测试函数：

```rust
dual_test! {
    fn my_case() {
        let mock = Mock::start(vec![MockResponse::json(200, r#"{"prepay_id":"wx1"}"#)]);
        let wechat_pay = client_for(&mock.base_url);
        let response = call!(wechat_pay.jsapi_pay(params)).expect("ok");
        assert_eq!(mock.requests()[0].path, "/v3/pay/transactions/jsapi");
    }
}
```

`call!(...)` 在 sync 模式直接求值、async 模式补 `.await`。注意 `verify_signature` 与
`decrypt_paydata` **不是** `maybe_async` 方法，两种模式下都是同步的，**不要**用 `call!` 包裹。
`Mock` 是顺序应答的（`Vec<MockResponse>` 按请求先后返回），并会记录每个请求的方法/路径/头/体。

`tests/offline.rs` 内嵌了一次性测试私钥（`TEST_PRIVATE_KEY`），**不对应任何真实商户账号**，
切勿用于生产。

**需要真实凭证的测试（16 个，默认 `#[ignore]`）** —— 全是只打印不断言的冒烟测试：

Requirements to run them at all:

1. **Credentials + live network** — most tests call `WechatPay::from_env()`, which `.expect()`s and
   panics without these env vars (typically via an untracked `.env`, loaded with `dotenvy::dotenv()`):

   `WECHAT_APPID`, `WECHAT_MCH_ID`, `WECHAT_PRIVATE_KEY`, `WECHAT_SERIAL_NO`, `WECHAT_V3_KEY`,
   `WECHAT_NOTIFY_URL`

   ⚠ `WECHAT_PRIVATE_KEY` is a **path to a PEM file**, not the key contents (`from_env` does
   `std::fs::read_to_string` on it).

2. **Untracked PEM fixtures in the repo root** (`.gitignore` excludes `*.pem` and `.env`): the tests
   read `./apiclient_key.pem`, `pub.pem`, `signature.txt`, `message.txt`, and write `pubkey.pem`.
   Run `cargo test` from the repo root or these CWD-relative paths break.

运行方式：`cargo test --lib -- --ignored`（需要 `.env` 与上述 fixture）。

遗留的 `#[cfg]` 双模式写法（新测试请用 `dual_test!`，不要再手抄）：

```rust
#[tokio::test]
#[cfg(feature = "async")]
pub async fn test_native_pay() { /* … .await */ }

#[test]
#[cfg(not(feature = "async"))]
pub fn test_native_pay() { /* … sync … */ }
```

**已知陷阱：** `init_log()` 用的是 `tracing_subscriber::fmt().init()` 而非 `try_init()`，
同一个测试二进制里第二次调用会 panic（"global default trace dispatcher has already been set"）。
另见 Development Commands 中关于 `cargo test --doc` 的说明。

没有覆盖率工具，也没有覆盖率门槛。

## 已知问题与后续方向

来自一次代码 / 依赖 / 许可证审计，按优先级排列：

1. **生产环境不要开 `debug-print`** —— 会记录 Authorization 头（含签名与 serial_no）、完整请求体
   （openid、订单号、金额）以及被签名的原文。不含私钥，但属敏感数据。
2. **幂等未内建** —— `notify::verify_notify` 只解决「是不是微信发的、是不是刚发的」，
   按 `out_trade_no` / `transaction_id` 落库去重仍需业务侧实现（微信最多重试 15 次）。
3. **账单下载未实现** —— 对账用的交易账单接口还没封装。
4. **`get_weixin` 是 SSRF 面** —— 对调用方传入的 URL 发 GET 且无白名单；`h5_url` 绝不能来自用户输入。
5. **`cargo check --no-default-features` 失败** —— 既有问题：`src/error.rs` 无条件引用
   `reqwest::Error`，而 reqwest 是 optional 依赖。
6. **`.json()` → `from_str` 的解码差异** —— 非法 UTF-8 的 2xx 响应体现在会被有损替换为 U+FFFD
   后解析成功，而不是报错（微信返回的 JSON 始终是合法 UTF-8，实际无影响）。
7. **超时后的「查单」仍需业务侧自己发起** —— SDK 已不对写接口超时做自动重试
   （判据见 `PayError::may_have_taken_effect`），但也**不会**替你调 `query_order`：
   查到状态之后怎么处置（继续等用户付款 / 关单 / 退款）是业务决策，SDK 无从代劳。
   完整口径见 `src/retry.rs` 的模块文档。
8. **不发布到 crates.io（crate name 与上游冲突）** —— `name = "wechat-pay-rust-sdk"` 在 crates.io
   已被上游占用，`Cargo.toml` 已设 `publish = false`，误执行 `cargo publish` 会在本地就失败。
   作为 git / path 依赖使用无需改动；将来真要发布，需先改名（会牵动 `example/Cargo.toml` 的
   依赖声明）并删掉 `publish = false`。

### 已完成（P0 / P1 / P2 / 规范 / 重试）

- ~~错误吞噬：非 2xx 响应被解析成成功~~ → `send_and_check` 先查状态码，新增 `PayError::ApiError`
- ~~缺少订单查询 / 关单 / 退款查询~~ → `query_order` / `close_order` / `query_refund`
- ~~回调无防重放~~ → `notify::verify_notify`（±300s 窗口 → 按 serial 选键 → 验签）
- ~~只支持单张平台证书，轮换期验签全挂~~ → `cert::PlatformKeys` + `fetch_platform_keys`
- ~~`WechatPay` 字段全 `pub`、`Debug` 泄露私钥~~ → 字段私有 + 脱敏 `Debug` + `with_base_url`
- ~~无离线测试、无 CI~~ → `tests/offline.rs`（19 个用例）+ GitHub Actions
- ~~README doctest 20/20 红、`--features async --all-targets` 编译失败~~ → `doctest = false` + 补齐 cfg 门
- ~~许可证元数据与 LICENSE 文件不一致~~ → 统一 Apache-2.0 + `NOTICE`
- ~~22 处冗余 `unsafe impl Send/Sync`~~ → 全部删除 + `#![forbid(unsafe_code)]` + 编译期 Send/Sync 断言
- ~~`PayError` 只有一个大枚举，无法区分处置策略~~ → `PayError::kind()` 分三层（Network / Api / Local）
- ~~每次请求 `Client::new()`，无超时、无连接池~~ → `WechatPay` 持有复用客户端 + `HttpTimeouts`（默认 5s / 10s / 90s）
- ~~HTTP 200 + 错误信封仍返回 `Ok`~~ → `is_error_envelope` 在 `request_json` 兜住；删除已无意义的 `WeChatResponse`
- ~~179 处公开项缺文档~~ → 全部补齐；`[lints.rust] missing_docs = "warn"` + CI `-D warnings` 作硬门槛
- ~~仓库未 rustfmt 化~~ → 全仓格式化 + CI `cargo fmt --all -- --check`
- ~~MSRV 未声明且会随依赖解析漂移~~ → 声明 `rust-version = "1.89"` + CI MSRV 作业（用 1.89.0 验证）
- ~~版本号未体现破坏性变更~~ → `0.2.21` → `0.3.0`，并设 `publish = false`（fork 不发布到 crates.io）
- ~~README 写死 crates.io 版本号安装~~ → 改为 git / path 引入（fork 未发布，写版本号会拿到上游代码）
- ~~自动重试未实现~~ → `src/retry.rs`：按「失败时微信侧处于什么状态」分类，默认只重试**确定没送到**
  （连接被拒/连接超时）与**官方明确未受理**（429/500/502/503/202）的失败；写接口超时不重试
- ~~`202 Accepted` 被当成 2xx 成功~~ → 按官方「请使用原参数重复请求一遍」处理，不再报误导性的 JSON 错误
- ~~超时后无法区分「确定没生效」与「结果未知」~~ → `PayError::may_have_taken_effect()`
