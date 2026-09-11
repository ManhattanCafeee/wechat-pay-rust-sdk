# Repository Guidelines

## Project Overview

`wechat-pay-rust-sdk` (v0.2.21) is a Rust client for the **WeChat Pay v3 HTTP API**. It wraps the
payment endpoints (native / H5 / JSAPI / app / micropay), refunds, platform-certificate download,
and callback (notify) decryption + RSA signature verification behind one `WechatPay` handle.

Single library crate at the repo root, plus one workspace member binary (`example/`, an actix-web
notify server). No CLI, no service, no generated code.

- **MSRV:** unstated. `edition = "2024"`, verified working on `rustc 1.98.0`. No `rust-toolchain.toml`.
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
             │  build_header(method, url, body)        # RSA-SHA256 sign → Authorization header
             │  Client::new().{get,post,…}(base_url + url).body(body).send()
             ▼
        send_and_check(builder)   # 先取 status + text
             ├── 非 2xx → Err(PayError::ApiError { status, response })
             └── 2xx    → serde_json::from_str::<R>()  ──▶  R: ResponseTrait (DeserializeOwned)
```

⚠ **状态码必须先于 body 处理。** 微信失败时返回非 2xx + `{"code","message","detail"}`。若直接
`.json::<R>()`，错误体就会被解析进字段全为 `Option` 的成功类型（如 `JsapiResponse`），
下单失败会伪装成 `Ok(JsapiResponse { code: Some("PARAM_ERROR"), prepay_id: None })`。
所有请求路径都经过 `send_and_check`（`src/async_impl/pay.rs`），新增端点不要绕过它。

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
src/pay.rs            WechatPay struct + config, both traits, from_env(), build_header(), sync tests
src/async_impl/       ⚠ name is misleading — this is the ONLY HTTP layer and serves sync + async
  pay.rs              impl WechatPay { pay, get_pay, *_pay, certificates, get_weixin, refunds }
  mod.rs              `pub mod pay;` (not feature-gated, always compiled)
src/model.rs          request params + notify models (Serialize); ParamsTrait
src/response.rs       response types (Deserialize); ResponseTrait, WeChatResponse<T>
src/sign.rs           sha256_sign — RSA PKCS#1 v1.5 + SHA-256, base64 STANDARD
src/util.rs           base64 helpers, random_trade_no, x509_to_pem, x509_is_valid
src/error.rs          PayError (thiserror); src/request.rs HttpMethod; src/pay_type.rs PayType
src/macros.rs         crate-local `debug!` / `error!` (no-ops unless `debug-print`)
example/              actix-web notify server; async usage reference
tests/offline.rs      离线集成测试：本地 mock 微信网关，无需凭证，CI 跑的就是它
.github/workflows/    CI：两种 feature 模式下的 test / check / clippy
NOTICE                Apache-2.0 归属声明（上游贡献者名单）
```

没有 scripts、没有 `rustfmt.toml`、没有 clippy 配置文件。

## Development Commands

Run everything from the repo root (tests use CWD-relative fixture paths).

```bash
# 完整测试：tests/offline.rs 用本地 mock 服务替代微信网关，不需要凭证或公网
cargo test                    # PASS — lib 2 passed / 14 ignored，offline 6 passed
cargo test --features async   # PASS — 同一套测试在 async 模式下再跑一遍

# 两种 feature 模式都必须能编译（含测试目标）——这是防回归的关键两条
cargo check --all-targets
cargo check --features async --all-targets
cargo check -p example
cargo clippy --all-targets --features async

cargo build --release
cargo run -p example          # actix server on 0.0.0.0:8080

# 需要真实商户凭证的测试默认被 #[ignore]，显式运行：
cargo test --lib -- --ignored
```

**`cargo test` 现在是绿的，可以当作冒烟测试用。** 改动后请同时跑 sync 与 async 两遍。

⚠ 两条**有意不跑**的命令：

- `cargo test --doc` —— README 通过 `#![doc = include_str!("../README.md")]` 充当 crate 文档，
  其示例需要真实商户密钥与公网（还含 actix-web 片段与输出示例）。`[lib] doctest = false` 使普通
  `cargo test` 跳过它们，但**显式**传入 `--doc` 会强制编译 README 代码块并失败（20 个）。
  它不是本仓库受支持的命令；要改 README 示例请自行编译验证。
- `cargo fmt --check` —— 仓库尚未整体格式化：`src/pay.rs`、`src/async_impl/pay.rs`、
  `example/src/main.rs` 存在 import 排序等差异。**新增/修改的文件请保持 rustfmt 干净**，
  不要顺手对整个仓库跑 `cargo fmt`（会让 fork 与上游的 diff 难以审阅）。

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

**API-error contract:** every HTTP path returns `Err(PayError::ApiError)` on non-2xx, with WeChat's
error body preserved. Branch on `response.code` rather than guessing from empty fields:

```rust
match wechat_pay.jsapi_pay(params) {
    Err(PayError::ApiError { status, response }) => {
        // status: u16；response.code / response.message / response.detail 均为 Option
        // detail 是微信的字段级定位信息（serde_json::Value），原样保留
    }
    Err(other) => { /* 网络/序列化/本地错误 */ }
    Ok(response) => { /* … */ }
}
```

`WeChatResponse<T>`（`refunds()` 的返回类型）保留 `is_success()` / `ok()` / `err()`：非 2xx 现在
先变成 `Err`，该类型留给「200 但 body 里带错误码」的边界情况。非 JSON 响应体（例如网关返回
HTML）会被原样放进 `response.message`，不丢线索。

`src/sign.rs` 与 `from_env()` 仍会 `expect()`/panic —— 属既有债务，不要照抄。

**Logging:** use the crate-local `debug!` / `error!` from `src/macros.rs`, which compile to no-ops
unless the `debug-print` feature is on. Prefer them over unconditional `tracing::debug!`.

**Adding an endpoint:** add one `#[maybe_async_attr] pub async fn` inside `impl WechatPay` in
`src/async_impl/pay.rs`, delegate to `self.pay(HttpMethod::POST, "/v3/…", params)` or
`self.get_pay(url)`, and add the params/response types to `src/model.rs` / `src/response.rs`.

**Do not propagate this boilerplate:** `unsafe impl Send for …` / `unsafe impl Sync for …` is sprayed
across `HttpMethod`, `Currency`, `AmountInfo`, `PayerInfo`, `WechatPay` and is redundant — those types
are already `Send + Sync`. Leave existing ones alone; don't add new ones.

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
| `src/lib.rs` | `#![doc = include_str!("../README.md")]` — the README **is** the crate rustdoc; that makes its fences doctests |
| `src/model.rs` | All request/notify models + `ParamsTrait` |
| `src/response.rs` | All response models + `ResponseTrait` + `WeChatResponse<T>` |
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
| 离线集成测试 | `tests/offline.rs` | 用 `Mock`（手写单请求 HTTP 服务）替代微信网关；6 个带断言的用例 |
| 纯逻辑单测 | `src/pay.rs`、`src/async_impl/pay.rs` | `test_uuid_v4`、`test_str` 两个无外部依赖用例 |

**`tests/offline.rs` 覆盖的契约（改这些行为必须同步改测试）：**

- Authorization 头格式 `WECHATPAY2-SHA256-RSA2048 mchid="…",nonce_str="…",signature="…",timestamp="…",serial_no="…"`
- 签名串 `"{method}\n{url}\n{timestamp}\n{nonce}\n{body}\n"` —— 测试**独立重建并验签**，不复用 SDK 自己的实现
- `pay()` 注入的 `appid` / `mchid` / `notify_url` 来自配置而非入参
- **非 2xx → `Err(PayError::ApiError)`，且 `code` / `message` / `detail` 不丢失**
- 非 JSON 错误体原样保留
- AES-256-GCM 回调解密往返
- 回调验签：合法通过 / 篡改 body 失败 / 非法签名失败

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

1. **缺少常用 API** —— 没有订单查询、关单、退款查询、账单下载。⚠ 不能直接复用 `pay()`：它无脑
   注入 `appid`/`mchid`/`notify_url`，而关单只需要 `mchid`、查单的 `mchid` 要放在 **query
   string** 里。需先抽出底层 `request(method, url, body)`，把 `pay()` 降级为它的包装。
2. **回调防护缺失** —— `verify_signature` 只验签名、**不校验时间戳新鲜度**，抓到一次合法回调即可
   无限重放。必须自行补 ±300s 时间窗 + 事件去重 + 按 `out_trade_no` 做幂等。
3. **证书轮换未支持** —— 只接受单个公钥，而微信平台证书在轮换期多张并存，需按请求头
   `Wechatpay-Serial` 选择对应公钥，否则轮换期回调验签会全部失败（订单不发货）。
4. **连接未复用** —— 每次请求都 `Client::new()`，无超时、无连接池。建议持有 `Client` 并设置超时；
   ⚠ 下单接口不可无脑重试（会重复下单），只对幂等的 GET 查单重试。
5. **`WechatPay` 派生 `Debug` 且字段全 `pub`** —— `{:?}` 会把商户私钥写进日志。建议字段改私有 +
   手写脱敏 `Debug`（`base_url` 因此也容易被非预期地改写）。
6. **生产环境不要开 `debug-print`** —— 会记录 Authorization 头（含签名与 serial_no）、完整请求体
   （openid、订单号、金额）以及被签名的原文。不含私钥，但属敏感数据。
7. **`cargo check --no-default-features` 失败** —— 既有问题：`src/error.rs` 无条件引用
   `reqwest::Error`，而 reqwest 是 optional 依赖。
8. **冗余 `unsafe impl Send/Sync`**（6 处）—— 相关类型本就是 `Send + Sync`，不要新增。
9. **仓库尚未整体 rustfmt 化** —— 见 Development Commands。
