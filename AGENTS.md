# Repository Guidelines

微信支付 APIv3 的 Rust 客户端库（**社区维护 fork，非腾讯官方**）。
包名 `wechat-pay-rust-sdk`，Rust 代码里的 crate 名是 `wechat_pay_rust_sdk`。

---

## Project Overview

- **用途**：封装微信支付 APIv3 的 HTTP 接口，供商户后端调用。**没有 CLI、没有服务端、没有代码生成**。
- **已封装的端点**：JSAPI / APP / H5 / Native / 付款码 下单（⚠ 付款码那条名不副实，见「已知限制与陷阱」）、
  统一下单入口 `pay()`、申请退款、查单、关单、查退款、证书列表与平台证书拉取、
  H5 页面取 `weixin://` 链接，以及回调的验签与 AES-256-GCM 解密。
- **未实现**：账单下载、分账、转账。不要以为它们在某个模块里没被找到 —— 确实没有。
- **这是 fork，不发布到 crates.io**：`Cargo.toml` 设了 `publish = false`（该名字在 crates.io 属于上游）。
  引入只能用 **git（固定 tag）或 path**；写成 `wechat-pay-rust-sdk = "0.3.0"` 会解析到上游代码或直接失败。
- `README.md` 通过 `#![doc = include_str!("../README.md")]`（`src/lib.rs:1`）**直接充当 crate 文档** —— 改 README 就是改公开文档。

## Architecture & Data Flow

### 一份实现，两个模式

全仓只有**一份** HTTP 实现（`src/async_impl/pay.rs`），同步与异步 API 由 `maybe-async` 在**编译期**
从同一份源码改写而来。⚠ `src/async_impl/` 这个目录名有误导性：它同时产出 sync 与 async 两套 API。

- `src/pay.rs`：`WechatPay` 的定义、配置、签名（`build_header`）、加解密、两个 trait。
- `src/async_impl/pay.rs`：所有端点方法 + 传输层 + 重试循环。
- 属性别名在 `src/async_impl/pay.rs:29-32`：async 用 `maybe_async::maybe_async`，sync 用 `maybe_async::must_be_sync`。

### 一次请求经过四层

```
端点方法 (jsapi_pay / query_order / refunds / close_order …)
   ↓
传输助手 (pay / get_pay / request_json / request_no_content)
   ↓
WechatPay::request(method, url, body, kind)      ← 签名 → 发送 → 错误归一 → 重试循环
   ↓
send_and_check(builder)                          ← 只检查 HTTP 状态码，返回 (status, text)
```

**两道错误检查缺一不可**，且分处两层：

1. **非 2xx** → `send_and_check()` 直接构造 `PayError::api_error(status, body)`。
   若像 `.json::<R>()` 那样直接解析，错误体会被塞进字段全为 `Option` 的成功类型
   （下单失败会伪装成 `Ok(JsapiResponse { code: Some("PARAM_ERROR"), prepay_id: None })`）。
2. **2xx + 错误信封** → 在 `request()` 内用 `is_error_envelope()` 判定（顶层出现**非空字符串** `code`），
   命中即归一成 `Err`。这一步放在 `request()` 而不是 `request_json()` 是有意的：只有归一成 `Err`，
   它才能参与重试判定 —— 于是 `SYSTEM_ERROR` 信封会像 HTTP 500 一样被重试，
   而关单（`request_no_content`，原先根本不看响应体）也不会再把「HTTP 200 + 错误信封」当成成功。

### 签名（每次尝试都重算）

`WechatPay::build_header()`（`src/pay.rs`）在 `request()` 的**循环体内**被调用，因此每次重试都会
重新生成 `timestamp` / `nonce_str` 并重签 —— 重试可能跨过 5 分钟的签名有效窗口，复用旧签名会 401。

- 签名串固定 5 行：`"{method}\n{url}\n{timestamp}\n{nonce_str}\n{body}\n"`
- 算法：RSA PKCS#1 v1.5 + SHA-256（`src/sign.rs::sha256_sign`），Base64 用 **STANDARD**
- 头：`Authorization: WECHATPAY2-SHA256-RSA2048 mchid="…",nonce_str="…",signature="…",timestamp="…",serial_no="…"`
- ⚠ `url` **原样参与签名**，所以 GET 的查询串必须拼进 `url`（查单的 `?mchid=` 就是靠这个进签名的）

### 重试：全仓最需要记住的不变式

重试安不安全，**只取决于上一次尝试有没有被微信处理过**。判定 = `RequestKind`（重放语义）
× `Delivery`（失败分类），`RetryPolicy` **只控制次数与退避**：

| `Delivery` | 典型场景 | 只读 | 写 | 退款 |
| --- | --- | --- | --- | --- |
| `NotSent` | 连接被拒、连接超时 | 重试 | 重试 | 重试 |
| `Rejected` | 429 / 500 / 502 / 503、`SYSTEM_ERROR` | 重试 | 重试 | 重试 |
| `Accepted` | HTTP 202 | 重试 | 重试 | 重试 |
| `Unknown` | 读写超时、504 等官方未定义的 5xx | 重试 | **不重试** | **不重试** |
| `Processed` | 响应体没读完就断连 | 不重试 | 不重试 | 不重试 |

- 判据全部来自 **reqwest 的错误标志位实测**，不是猜的：`is_connect()` 先判（它同时覆盖连接被拒
  与连接超时——两者都确定没送到）；读响应体失败报的是 **`is_decode()` 而不是 `is_body()`**
  （实测 `is_body()` 为 false），按直觉写 `is_body()` 会把「服务端已处理」误判成可重试。
- **写接口超时不重试**：官方口径是先用查单接口确认状态。用
  `PayError::may_have_taken_effect()` 区分「确定没生效」与「结果未知」，前者才会返回 `false`。
- **退款走独立策略**（`RetryPolicy::for_refund`）：最多 2 次、首次退避 60s、**不加抖动**。
  官方要求退款重试「间隔 1 分钟」且失败时报错限流仅 6QPS；抖动是**向下**取随机的，
  开了就等于把「至少间隔 1 分钟」变成「可能立刻重发」。
- 详细依据与官方引文见 `src/retry.rs` 的模块文档。

## Key Directories

```
src/                 库本体（12 个公开模块 + 1 个 pub(crate) 模块）
src/async_impl/      ⚠ 名字有误导性：sync 与 async 的**共享**实现（maybe-async 编译期改写）
src/retry.rs         重试策略、失败分类与可重试判定（改动前先读模块文档）
src/pay.rs           WechatPay 定义 / 配置 / build_header 签名 / 加解密 / 两个 trait
src/cert.rs          平台证书按 serial 选键（轮换期必须用）
src/notify.rs        回调验签 + 防重放（±300s 窗口 → 选键 → 验签）
src/error.rs         PayError / ErrorKind 三层归类
src/model.rs         请求与回调模型（Serialize）
src/response.rs      响应模型（Deserialize）
src/{sign,util}.rs   RSA-SHA256 签名；Base64 / X.509 / 随机单号助手
src/{request,pay_type}.rs  HttpMethod；PayType（**未被 crate 使用的公开 API**）
tests/offline.rs     唯一测试文件：本地 mock 网关，不需要凭证
example/             actix-web 回调服务器（workspace 成员，发布时被 exclude）
.github/workflows/   唯一的 CI（ci.yml）
```

**没有**这些东西，别去找：`rustfmt.toml`、`clippy.toml`、`rust-toolchain.toml`、`Makefile`、
`justfile`、`scripts/`、`.editorconfig`、第二个 workflow。

## Development Commands

CI（`.github/workflows/ci.yml`）实际只跑这 8 条命令 —— 本地复现 CI 就照这个来：

```bash
# Test & lint 作业（stable，已装 clippy/rustfmt）
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features async -- -D warnings
cargo clippy --all-targets --all-features -- -D warnings
cargo test                     # lib 11 passed / 14 ignored + offline 44 passed
cargo test --features async    # 同一套测试在 async 模式下再跑一遍
cargo check -p example

# MSRV 作业（1.89.0）
cargo +1.89.0 check --all-targets
```

- **两个 feature 模式都必须能编译**是防回归的关键，改完至少把两条 `cargo test` 都跑一遍。
- **CI 不跑**：`cargo build`、任何 `--release`、`cargo run`、`cargo test --doc`。
  前三者不是本仓库的验证命令；`--doc` 是**有意**不跑的（见「Testing & QA」）。
- 三个 clippy 组合都要过：`debug-print` 会切换 `src/macros.rs` 的两份宏定义，
  只测一种组合会漏掉一半 cfg 分支（历史上就漏过一次：`--all-features` 下 `not(debug-print)` 的宏不存在）。
- 需要真实商户凭证的在线用例：`cargo test --lib -- --ignored`（见「Testing & QA」的注意事项）。

## Code Conventions & Common Patterns

### 文档与命名

- **注释与文档一律用中文**；字段注释格式为 `///【字段名】 说明`（`src/model.rs` 全篇如此）。
- `[lints.rust] missing_docs = "warn"` + CI 的 `-D warnings` 让它成为硬门槛：**公开项必须写文档**。
- 命名：文件 `snake_case.rs`，类型 `CamelCase`，方法 `snake_case`；`WechatPay` 字段全部私有。

### 新增一个微信接口时

必须**显式**选一个 `RequestKind`（`Read` / `Write` / `Refund`）传给 `request_json()` 或
`request_no_content()`。枚举**没有默认值**，漏传是编译错误 —— 这是有意的：
新增端点时必须先想清楚它能不能重放。只读查询用 `Read`，下单 / 关单用 `Write`，退款用 `Refund`。

### 双模式写法（改 `src/async_impl/` 时最先看这段）

- 方法签名照抄邻居：`#[maybe_async_attr]` + `async fn`，返回 `Result<T, PayError>`。
- 需要在两个模式下行为不同的系统调用要**写两份、按 feature 择一编译**
  （见 `src/async_impl/pay.rs` 的 `sleep_for`：异步用 `tokio::time::sleep`，同步用 `thread::sleep`）。
  ⚠ 同步那份**刻意不是 `async fn`** —— 否则调用方拿到一个永不轮询的 Future，睡不成。
- 测试里**绝不能直接写 `.await`**，模式相关的调用一律用 `call!` 宏包（见「Testing & QA」）。

### 错误处理

`PayError`（`src/error.rs`）+ `ErrorKind` 三层归类，按处置策略而非来源划分：

| 归类 | 含义 | 处置 |
| --- | --- | --- |
| `Network` | 传输 / 连接层失败 | 交给 `src/retry.rs` 分类；结果未知的写接口应先查单 |
| `Api` | 微信业务拒绝（非 2xx，或 2xx 错误信封） | 429/500/502/503/`SYSTEM_ERROR` 会自动重试，其余按 `response.code` 分支 |
| `Local` | 签名、解密、Base64、JSON 解析、验签失败、回调超窗 | 不重试，通常是配置或数据问题，应当告警 |

- `PayError` 新增变体时，`kind()` 是**穷尽匹配**，编译器会强制你做出归类决定。
- 错误消息里刻意保留了排查线索：原始响应体（超长截断并标注、空体有显式标记）、微信的
  `code` / `message` / `detail`。⚠ `Display` 会带出 `detail`（含出错字段路径与取值），外发日志前需脱敏。

### 敏感数据

- `WechatPay` 的 `Debug` 是**手写**的（`private_key` / `v3_key` 打成 `<redacted>`）；加字段时别退回 `derive`。
- `debug-print` feature 会把 `Authorization` 头（含签名与 serial_no）、请求体、被签名的原文写进日志 ——
  **生产环境不要开**。日志宏是 crate 本地的 `debug!` / `error!`（`src/macros.rs`），
  未启用该 feature 时展开为空操作、**参数不求值**，也正因如此没有直接用 `tracing` 宏。
  （现状：`error!` 在 `src/` 内零调用点。）

### 加密与验签不变量

- Base64 分工明确：RSA 签名用 **STANDARD**，AEAD 用 URL-safe 时另算 —— 别混用。
- 回调验签顺序固定：**时间戳新鲜度（±300s）→ 按 `Wechatpay-Serial` 选键 → 验签**。
  轮换期微信会同时下发新旧两张平台证书，写死单张会让回调验签全部失败（等于订单不发货）。
- 微信解密后给的是 **PEM 而不是 DER**；`util::x509_to_pem` / `x509_is_valid` 处理这个差异。

### 参数注入的位置

`appid` / `mchid` / `notify_url` **只在 `pay()` 里注入**。其余端点走 `request_json()` / `request_no_content()`，
不注入任何字段 —— 关单的 body 只要 `mchid`，查单的 `mchid` 在 query string。
无脑注入这三个字段会让这些端点直接失败。

## Important Files

| Path | Why it matters |
| --- | --- |
| `Cargo.toml` | feature 矩阵、`[lints]`、`rust-version`、`publish = false`、`[lib] doctest = false`、workspace 成员 |
| `src/lib.rs` | crate 文档来源（include README）、`#![forbid(unsafe_code)]`、模块清单 |
| `src/async_impl/pay.rs` | 所有端点方法 + 传输层 + 重试循环 —— 改动最集中的文件 |
| `src/retry.rs` | 重试策略与失败分类；模块文档里有完整的官方依据 |
| `src/pay.rs` | `WechatPay` 定义与配置面（`with_base_url` / `with_timeouts` / `with_retry` / `with_refund_retry`） |
| `src/error.rs` | `PayError` / `ErrorKind` / `may_have_taken_effect()` |
| `tests/offline.rs` | 唯一的测试文件；自建 mock + `dual_test!` 宏也在里面 |
| `README.md` | **同时是 crate 文档**；含安装方式（git / path）与各能力的使用示例 |
| `CHANGELOG.md` | 破坏性与行为变更的权威记录，含未发布节 |
| `NOTICE` / `LICENSE` | Apache-2.0 归属声明与全文；许可证口径必须三处一致 |
| `example/src/main.rs` | actix-web 回调服务器：**验签 → 解密 → 幂等 → 应答** 的完整参考实现（刻意不开 `debug-print`） |
| `.github/workflows/ci.yml` | 唯一的 CI；上文的命令清单就来自这里 |

**测试专用的公开 API**：`WechatPay::with_base_url()` 是测试把请求指向本地 mock 的唯一入口
（字段私有）。它的同类风险是真实的 —— 能改 `base_url` 就能把你签好名的请求（含 openid、订单信息）
改送到第三方域名，所以 ⚠ **生产代码绝不能让用户输入接触 `base_url`**。

## Runtime/Tooling Preferences

- **纯 Cargo**，edition 2024，**MSRV 1.89**（`rust-version = "1.89"`），无 toolchain 文件。
  声明 `rust-version` 不只是记录下限 —— 它会让 cargo **优先挑兼容该版本的依赖**。本仓库按库的惯例
  不提交 `Cargo.lock`，不声明的话实际 MSRV 会随解析漂移（实测 1.85 / 1.88 都曾因 `icu_*`、`time`、
  `aes` 的新版本编译失败）。
- `Cargo.lock` 被 `.gitignore` 忽略（库约定）—— **不要提交它，也不要把它在磁盘上的存在当成问题**。
- **feature 矩阵**（它们是唯一的构建开关，且会改变公开方法的签名）：

  | feature | 作用 | 备注 |
  | --- | --- | --- |
  | `default` | `reqwest/blocking` → 同步 API | 同步是**默认**行为，不存在 `blocking` feature |
  | `async` | `reqwest` + `dep:tokio` → 异步 API | 见下 |
  | `debug-print` | `tracing` + `tracing-subscriber` | 会打印敏感信息，生产禁用 |

- `async` 之所以依赖 `tokio`：异步重试退避要 `tokio::time::sleep`。用 `dep:tokio` 前缀是为了
  **不额外暴露一个叫 `tokio` 的公开 feature**（写成 `"tokio"` 就会），改 feature 列表时保留这个区分。
  ⚠ 库只开 `time`，**不带 runtime** —— 下游启用 `async` 必须自带带 timer 的 tokio runtime，
  否则 `tokio::time::sleep` 会 panic。测试能跑是因为 `[dev-dependencies]` 另有一份 `tokio`（`full`）与之做 feature 合并。
- `cargo check --no-default-features` **会编译失败**，属既有缺口（见「已知限制与陷阱」）。
- 没有 rustfmt / clippy 配置文件：格式与 lint 全部走默认 + CI 参数。

## Testing & QA

### 三层测试

| 层 | 位置 | 怎么跑 | 需要什么 |
| --- | --- | --- | --- |
| 离线集成 | `tests/offline.rs`（唯一） | `cargo test` / `--features async` | 只需要本地监听端口 |
| 纯逻辑单测 | `src/pay.rs`、`src/retry.rs`、`src/async_impl/pay.rs` 的 `#[cfg(test)]` | 同上，默认执行 | 无 |
| 在线冒烟 | 同上文件里的 `#[ignore]` | `cargo test --lib -- --ignored` | 真实凭证 + 公网 + 仓库根 PEM fixture |

**精确计数（单次运行）**：`cargo test` → lib **11 passed / 14 ignored**，offline **44 passed**；
`cargo test --features async` → lib **11 passed / 7 ignored**，offline **44 passed**。

- offline 的 44 = 42 个 `dual_test!` + 2 个顶层 `#[test]`（`refund_uses_a_separate_minute_scaled_policy`、
  `public_types_are_send_and_sync`）。可复现：`grep -c '^dual_test! {' tests/offline.rs` → 42。
- ⚠ 计 `#[test]` 时要按**行首**（`^#\[test\]`）锚定：直接数 `#[test]` 会把 `dual_test!` 宏定义体内的
  那一次（缩进）和文档注释里提到的一次也算进去。
- 「需凭证的用例」有两种口径：跨模式去重共 **16 个函数**，但单次运行只列出 **14**（sync）或 **7**（async），
  因为另一半被 `cfg(feature)` 排除了。写文档时要说清用的是哪种口径。

### `dual_test!`：同一份测试体跑两种模式

```rust
dual_test! {
    fn my_case() {
        let mock = Mock::start(vec![MockResponse::json(200, r#"{"prepay_id":"wx1"}"#)]);
        let wechat_pay = client_for(&mock.base_url);
        let resp = call!(wechat_pay.jsapi_pay(JsapiParams::new("商品", "O1", 1.into(), "openid".into())))
            .expect("200 应成功");
        assert_eq!(resp.prepay_id.as_deref(), Some("wx1"));
        assert_eq!(mock.requests()[0].path, "/v3/pay/transactions/jsapi");
    }
}
```

- 宏只接受 `fn 名字() { … }` 这一种形状：**不要**自己写 `#[test]` / `#[tokio::test]`，
  函数体不能有参数或返回值（失败靠 `expect` / `panic!`，不要用 `?`）。
- **模式相关的方法调用一律用 `call!` 包**（async 展开成 `.await`，sync 展开成原表达式）；
  测试体里**绝不能直接写 `.await`**，否则 sync 模式编译不过。
- 同步方法**不要**用 `call!` 包（例如 `verify_signature` / `decrypt_paydata`）。

### mock 与可用的辅助函数

`tests/offline.rs` 自带一个手写 HTTP mock（不引入额外依赖）：支持脚本化响应序列、keep-alive、
连接计数（验证连接复用）、永不响应的黑洞服务（验证超时）、以及声称有 100 字节却只发 10 字节的
截断响应（验证 `is_decode` 分支）。

常用辅助：`Mock` / `MockResponse` / `client_for()` / `settled_captures()` /
`auth_field()` / `nonce_of()` / `sign_rsa()`，以及内嵌的测试私钥、v3 key 与平台证书 PEM fixture。

- ⚠ **脚本响应的条数必须与预期请求数严格一致**：队列耗尽后 mock 会返回 `500 {}`，
  多出来的一次请求会拿到 500（可能触发重试），失败信息会误导到「服务端错误」。
- ⚠ **断言请求次数前先等捕获稳定**（用 `settled_captures()`）：黑洞 mock 永不回响应，
  客户端返回与 mock 读取线程之间没有同步点，直接断言会偶发失败。
- 涉及退避的用例把延迟压到毫秒级（`fast_retry()`），别让测试真等 200ms 起步的退避。

### 覆盖期望

测试钉住的是**契约**，改行为必须同步改测试。主题：Authorization 头格式与签名串（测试会**独立重建并验签**，
不复用 SDK 自己的实现）、GET 的查询串必须进签名、参数注入规则、非 2xx 与 2xx 错误信封都返回 `Err`、
错误体保留 / 截断 / 空体标记、关单 204、AES-GCM 往返、回调验签与防重放（含 ±300s 边界）、
平台证书轮换选键、连接复用与超时、`sign_data` 前缀差异、退款成功路径、以及

- **重试的安全边界**：写接口超时不重试、只读接口超时重试、已处理的请求一律不重放、连接失败连写接口也重试；
- **重试触发与次数**：429 / 503 会重试，且断言 mock **实际收到的请求次数**；
- **每次重试重新签名**（两次尝试的 `nonce_str` 必须不同）；
- **202 与 `SYSTEM_ERROR` 信封**的行为（含关单不再把 200 + 信封当成功）；
- **退款策略真的被选中**（关掉通用策略后仍会有第 2 次请求）；
- **`sign_data` 的 JSON 键名对齐官方**（`timeStamp` / `nonceStr` / `package` / `signType` /
  `paySign` / `appId`，注意 `timeStamp` 的大写 S）：前端报「缺少参数」时不会指向 SDK，
  所以有测试钉住键名，并**反向断言不得残留 snake_case**。

给新增测试定标准时注意：仓内有**两个不构成回归保护的用例**（`src/pay.rs::test_uuid_v4` 只打印，
`src/async_impl/pay.rs::test_str` 只做 split 且无断言）。别把它们当范例。

## 已知限制与陷阱

**构建**

- `cargo check --no-default-features` 失败：`reqwest` 是 optional 依赖，但 `src/error.rs` 的错误枚举
  无条件持有 `reqwest::Error`，`src/pay.rs` / `src/async_impl/pay.rs` 也无条件 `use reqwest::header::…`，
  `src/retry.rs` 还调用 `reqwest::Error` 的方法。要修得同时动这几处，改 `error.rs` 一处不够。
- `cargo test --doc` 会失败（实际收集 **32** 个 doctest 后编译失败）—— 这是**有意**接受的：
  README 的示例需要真实凭证与公网，所以设了 `[lib] doctest = false`。CI 不跑它。
  ⚠ 同一开关也关掉了**源码文档注释里**的示例（`src/retry.rs`、`src/error.rs`、`src/pay.rs`、`src/notify.rs`），
  改这些示例时没有任何编译兜底。
- README 示例**不被编译**，因此会悄悄腐烂。已修掉一处（小程序支付那段曾调用
  `micro_pay(JsapiParams::new(…))`，与真实签名 `micro_pay(params: MicroParams)` 不符），
  但这只是**已知的一处** —— 改 README 示例时请人工核对 API 签名，CI 不会发现。

**代码**

- `micro_pay()` 自称「付款码支付」，但实现 POST 到 **JSAPI 下单端点** `/v3/pay/transactions/jsapi`，
  且 `MicroParams` 没有付款码支付必需的 `auth_code` 字段。文档与实现自相矛盾 ——
  使用前请先核实官方端点，不要按方法名假定它可用。
  （README 已改为指向 `jsapi_pay`：**小程序支付就是 JSAPI**，与公众号 JSAPI 只差 openid 来源。）
- `get_weixin(h5_url, referer)` 对调用方传入的 URL 发 GET 且**无白名单**，是 SSRF 面：
  `h5_url` 只能来自你自己的服务端或微信返回的 `h5_url`，**绝不能来自用户输入**。
- `.json()` → `from_str` 的解码差异：非法 UTF-8 的 2xx 响应体会被有损替换为 U+FFFD 后解析成功，
  而不是报错（微信返回的 JSON 始终合法，实际无影响）。
- `PayType`（`src/pay_type.rs`）是未被 crate 使用的公开 API。

**业务侧必须自己做的事**

- **幂等**：`verify_notify` 只解决「是不是微信发的、是不是刚发的」，按 `out_trade_no` / `transaction_id`
  落库去重仍需业务侧实现（微信最多重试 15 次通知）。
- **超时后查单**：SDK 不做写接口超时的自动重试，也**不会**替你调 `query_order` ——
  查到状态之后怎么处置（继续等 / 关单 / 退款）是业务决策。判据用 `PayError::may_have_taken_effect()`。
- **账单下载**（对账用）尚未封装。

**测试**

- 在线 `#[ignore]` 用例若用 `cargo test --lib -- --ignored` 一次跑完，第二个起会因为
  `init_log` 重复初始化全局 subscriber 而 panic；要跑就逐个 `--exact <名字>`。
  它们依赖 `WechatPay::from_env()`（缺环境变量会 panic，且 `WECHAT_PRIVATE_KEY` 是**文件路径**）
  与仓库根下未入库的 PEM fixture，所以必须在仓库根运行。
- 测试证书 fixture 的有效期到 **2036-09-08**，`x509_helpers_extract_the_certificate_public_key`
  断言其有效 —— 2036 年之后该用例会失败（已知的长期失效点）。
