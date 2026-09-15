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
传输助手 (pay / get_pay / request_json / request_no_content)   ← 先 ensure_keys()，拿到密钥才发请求
   ↓
WechatPay::request(method, url, body, kind, check)   ← 签名 → 发送 → 应答验签 → 错误归一 → 重试循环
   ↓
send_and_check(builder)              ← 只发请求，取回 (status, headers, 原始字节 body)
```

**三道检查缺一不可**，顺序**不可调换**：

1. **应答验签**（默认强制，R1）：`verify_response()` 用四个 `Wechatpay-*` 头验签，且必须在
   **错误归一之前** —— 否则一个能改应答的中间层可以伪造 4xx 信封，把「来源无法验证」伪装成
   微信的业务拒绝（例如 `ORDER_NOT_EXIST`）。缺头规则：2xx 拒绝、4xx 拒绝（消息带状态码与原文）、
   5xx 放行并在错误里标 `[未验签]`。
2. **非 2xx** → `request()` 构造 `PayError::api_error(status, body)`。⚠ 这一步原先在
   `send_and_check()` 里，搬到 `request()` 是**验签优先**的必然结果。若像 `.json::<R>()` 那样直接解析，
   错误体会被塞进字段全为 `Option` 的成功类型
   （下单失败会伪装成 `Ok(JsapiResponse { code: Some("PARAM_ERROR"), prepay_id: None })`）。
3. **2xx + 错误信封** → 在 `request()` 内用 `is_error_envelope()`（吃**原始字节**，不是 `&str`）判定
   （顶层出现**非空字符串** `code`），命中即归一成 `Err`。这一步放在 `request()` 而不是 `request_json()` 是有意的：只有归一成 `Err`，
   它才能参与重试判定 —— 于是 `SYSTEM_ERROR` 信封会像 HTTP 500 一样被重试，
   而关单（`request_no_content`，原先根本不看响应体）也不会再把「HTTP 200 + 错误信封」当成成功。

### 签名（每次尝试都重算）

`WechatPay::build_header()`（`src/pay.rs`）在 `request()` 的**循环体内**被调用，因此每次重试都会
重新生成 `timestamp` / `nonce_str` 并重签 —— 重试可能跨过 5 分钟的签名有效窗口，复用旧签名会 401。

- 签名串固定 5 行：`"{method}\n{url}\n{timestamp}\n{nonce_str}\n{body}\n"`
- 算法：RSA PKCS#1 v1.5 + SHA-256（`src/sign.rs::sha256_sign`），Base64 用 **STANDARD**
- 头：`Authorization: WECHATPAY2-SHA256-RSA2048 mchid="…",nonce_str="…",signature="…",timestamp="…",serial_no="…"`
- ⚠ `url` **原样参与签名**，所以 GET 的查询串必须拼进 `url`（查单的 `?mchid=` 就是靠这个进签名的）

### 应答验签（默认强制）

微信对**应答**也签名（不只是回调）：四个头 `Wechatpay-Serial` / `-Timestamp` / `-Nonce` /
`-Signature`，验签串是 **3 行** `"{timestamp}\n{nonce}\n{body}\n"` —— 与请求签名的 5 行不同，
而且**不绑定请求**（所以 ±300s 窗口是唯一的防重放手段）。body 必须是**响应原始字节**：
`Response::bytes()` 收下后一路 `&[u8]`（`is_error_envelope` / 验签都吃字节，JSON 解析走
`from_slice`），转成 `String` 再序列化会改变字节、验签必然失败。

- 密钥索引由 `WechatPay` 自己维护（`RwLock<PlatformKeys>`，锁内只查表 + 一次 RSA 验签，
  **绝不跨 `.await` 持锁**）：`request_json` / `request_no_content` 发送前调 `ensure_keys()`
  （冷启动就是首次拉取），之后按 12 小时窗口刷新。⚠ 这一步**必须**在发送前、且不能放进
  `request()`（它也在**同步**模式下被证书拉取复用 —— 放进去在异步模式下会让
  `request → 拉证书 → request` 的 Future 无限大，编译报 E0733）。
- **单飞覆盖全部刷新路径**（`fetch_and_install_keys` 里 CAS `RefreshGuard`）：同一时刻只有一个人
  打 `/v3/certificates`，冷启动与 12 小时窗口的并发请求也一样（否则 N 个并发请求发 N 次）。
  等待者的完成信号只能是「`in_flight` 已释放」，**不能**用 `needs_refresh`（命中未知 serial 时
  索引可能既新鲜又不含那个 serial）。标记用 **RAII 释放**：异步任务被 `timeout` / `select!` /
  abort 丢弃时手动复位不会执行，标记会永久为真。
- 未知 serial → `refresh_platform_keys_for_unknown_serial()`（**60s 最小间隔** + 单飞）后**重验**：
  复用已缓冲的响应，**绝不重发业务请求**。该方法**公开**：回调验签拿到
  `UnknownPlatformSerial` 时也用它（回调的 serial 是未鉴权输入，不带限流的
  `refresh_platform_keys()` 会被伪造 serial 变成放大器）。⚠ 刷新失败必须**归一成
  `UnknownPlatformSerial`**：那个错误来自另一条请求，若以原类型逃进业务请求的失败分类，
  可重试类会**重发已收到应答的写请求**，4xx 类会让调用方以为「确定没受理」。
- 安装前在写锁内**复查**静态密钥模式：`set_platform_keys` / `with_platform_public_key` 与一次
  飞行中的拉取撞上时，覆盖用户密钥是不可逆的（`static_keys` 只挡「发起」，挡不住「安装」）。
- **`GET /v3/certificates` 的应答一律自校验**（`ResponseCheck::CertificateSelfCheck`，公开的
  `certificates()` 与内部刷新都走）：用本次响应内 `Wechatpay-Serial` 指向的那张证书验它自己。
  轮换期微信用**新**证书签名，本地索引里还没有 —— 没有这条路径，「未知 serial → 刷新」会在
  首次轮换时自锁。⚠ 它不是信任锚（公钥来自同一个尚未验签的 body），真正的保证是 TLS + AEAD。
- **微信支付公钥模式**（serial 形如 `PUB_KEY_ID_…`）的公钥不在平台证书列表里：
  用 `with_platform_public_key(id, pem)` 配置；遇到该前缀**不会**去刷新证书（白打限流接口）。
- `set_platform_keys` / `with_platform_public_key` 之后是**静态密钥模式**（`static_keys`）：
  不自动拉取、不自动替换。否则手工灌入的索引 `fetched_at = None` 会让 `needs_refresh()` 恒为 true
  （每个请求前都拉一次证书），而「整体替换」会把公钥模式配置的公钥抹掉。
- 刷新是**整体替换**（不是合并，合并会把微信已撤下的旧证书永久留下），并丢弃
  `x509_is_valid` 判定为已过期的证书；刷新失败**不动**已有索引。
- `src/cert.rs` 的 `indirect!` 宏：`request → verify_response → refresh → fetch_keys_from_api
  → request` 构成调用环，异步模式下必须在其中一条边引入 `Box::pin`（选在罕见的「拉证书」
  那条边，避免热路径每请求多一次分配）；同步模式直接求值。

### 重试：全仓最需要记住的不变式

重试安不安全，**只取决于上一次尝试有没有被微信处理过**。判定 = `RequestKind`（重放语义）
× `Delivery`（失败分类），`RetryPolicy` **只控制次数与退避**：

| `Delivery` | 典型场景 | 只读 | 写 | 退款 |
| --- | --- | --- | --- | --- |
| `NotSent` | 连接被拒、连接超时 | 重试 | 重试 | 重试 |
| `Rejected` | 429 / 500 / 502 / 503、`SYSTEM_ERROR` | 重试 | 重试 | 重试 |
| `Accepted` | HTTP 202 | 重试 | 重试 | 重试 |
| `Unknown` | 读写超时、504 等官方未定义的 5xx | 重试 | **不重试** | **不重试** |
| `Processed` | 响应体没读完就断连、**应答验签失败** | 不重试 | 不重试 | 不重试 |

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
src/cert.rs          平台证书 / 微信支付公钥索引：按 serial 选键、到期过滤、刷新（单飞 + 自校验）
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

CI（`.github/workflows/ci.yml`）实际只跑这 9 条命令 —— 本地复现 CI 就照这个来：

```bash
# Test & lint 作业（stable，已装 clippy/rustfmt）
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features async -- -D warnings
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo test                     # lib 13 passed / 14 ignored + offline 71 passed
cargo test --features async    # lib 14 passed / 7 ignored + offline 71 passed
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

应答验签不需要端点做任何事：它发生在 `request()` 内部（默认强制）。唯一显式的选择是
`ResponseCheck`（`Strict` / `CertificateSelfCheck`），而它只对 `GET /v3/certificates` 用
`CertificateSelfCheck` —— 普通端点照抄邻居即可。

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

- `WechatPay` / `WechatPayConfig` 的 `Debug` 都是**手写**的（`private_key` / `v3_key` 打成
  `<redacted>`）；加字段时别退回 `derive`（`tests/offline.rs::debug_output_redacts_secrets` 守着这个）。
  `WechatPay` 的 Debug **有意**保留 `response_verify` 与平台证书 serial 列表：两者都不是机密，
  而「我是不是误关了验签 / 现在认了哪几张证书」是升级后的头号排查问题。
- `debug-print` feature 会把 `Authorization` 头（含签名与 serial_no）、请求体、被签名的原文写进日志 ——
  **生产环境不要开**。日志宏是 crate 本地的 `debug!`（`src/macros.rs`），未启用该 feature 时展开为
  空操作、**参数不求值**，也正因如此没有直接用 `tracing` 宏。
  它用 `pub(crate) use` 暴露（**不是** `#[macro_export]`）—— 后者会把内部宏发布到 crate 根，
  等于把这段临时语法冻进公开 API。

### 加密与验签不变量

- Base64 分工明确：RSA 签名用 **STANDARD**，AEAD 用 URL-safe 时另算 —— 别混用。
- 回调验签顺序固定：**时间戳新鲜度（±300s）→ 按 `Wechatpay-Serial` 选键 → 验签**。
  轮换期微信会同时下发新旧两张平台证书，写死单张会让回调验签全部失败（等于订单不发货）。
- 微信解密后给的是 **PEM 而不是 DER**；`util::x509_to_pem` / `x509_is_valid` 处理这个差异
  （`x509_is_valid` 返回 `PayError`，与库其余错误一致）。
- **应答验签**不变量：验签串 3 行且用**原始字节**；时间窗与回调共用
  `notify::check_timestamp_skew`（含 `i64::MIN` 防溢出）**只有一份实现**；`GET /v3/certificates`
  的应答是唯一豁免（自校验），且该豁免**不能**限制成「索引为空」——轮换期它会自锁。
- ⚠ **不要**把「验签失败」重新归到 `may_have_taken_effect() == false`：应答是完整收到之后
  才验签的，官方还会故意下发错误签名探测（`WECHATPAY/SIGNTEST/`），判 `false` 会诱导写接口
  换个单号重开。三个验签类变体在 `classify()` 里映射到 `Delivery::Processed`。

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
同理，`set_platform_keys()` / `with_platform_public_key()` 是测试与固定证书来源的入口：
它们会把客户端切成静态密钥模式，能改它就等于能换掉验签用的公钥。

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
| 纯逻辑单测 | `src/pay.rs`、`src/retry.rs`、`src/util.rs`、`src/pay_type.rs`、`src/async_impl/pay.rs` 的 `#[cfg(test)]` | 同上，默认执行 | 无 |
| 在线冒烟 | 同上文件里的 `#[ignore]` | `cargo test --lib -- --ignored` | 真实凭证 + 公网 + 仓库根 PEM fixture |

**精确计数（单次运行）**：`cargo test` → lib **13 passed / 14 ignored**，offline **71 passed**；
`cargo test --features async` → lib **14 passed / 7 ignored**，offline **71 passed**。

- offline 的 71 = 69 个 `dual_test!` + 2 个顶层 `#[test]`（`refund_uses_a_separate_minute_scaled_policy`、
  `public_types_are_send_and_sync`）。可复现：`grep -c '^dual_test! {' tests/offline.rs` → 69。
- lib 的 async 多一个用例：`src/async_impl/pay.rs::public_futures_are_send`（`cfg(feature = "async")`）。
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

**mock 会为每条响应现算四个签名头**（`response_signature_headers`：用测试平台证书配对的私钥签
**实际写出去的 body 字节**），所以既有用例不必逐个改；要造异常场景就链一个开关：

```rust
MockResponse::json(200, body)          // 默认：正确签名（serial = TEST_PLATFORM_SERIAL）
    .signed_with_serial("SERIAL_NEW")  // 换 serial（轮换用）
    .signed_at(i64::MIN)               // 指定时间戳（超窗 / 极值用例）
    .invalid_signature()               // 签名对不上 body
    .without_signature_headers()       // 模拟代理 / CDN 过滤掉 Wechatpay-* 头
    .stale_timestamp()                 // 时间戳超 ±300s
    .probe_signature()                 // WECHATPAY/SIGNTEST/ 探测签名
    .with_delay(Duration::from_millis(500))  // 让并发用例稳定重叠
    .for_path("/v3/certificates")      // 只服务该路径（并发用例防串包）
```

⚠ 三条容易踩的：

- **`client_for()` 会预置平台密钥并因此进入静态密钥模式** —— 这是为了让既有用例的
  `requests()[0].path` 断言与精确脚本条数保持成立（否则每个用例都会先拉一次 `/v3/certificates`）。
  要测冷启动就**别用 `client_for`**，直接 `WechatPay::from_config(test_config()).with_base_url(...)`。
- **证书列表的应答必须用它自己下发的 serial 签名**（`.signed_with_serial("SERIAL_OLD")`），
  因为客户端对 `/v3/certificates` 走的是**自校验**（R5）；顺手也把 body 里要出现的 serial 写在同一处。
- **响应队列是跨连接的全局 FIFO**：并发用例里出队顺序取决于 mock 先读到哪条连接，
  用 `.for_path(..)` 钉住「刷新用的证书响应」，否则它可能被误发给业务请求
  （失败信息会指向一个与真实原因无关的 JSON 错误）。

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

给新增测试定标准时注意：**用例必须能对着一个坏实现失败** —— 例如抖动测试要断言「确实取过上限
以下的值」（否则「抖动恒等于计算值」的实现照样全绿）、`verify_notify` 有一条走**真实墙钟**的用例
（否则时间源换成毫秒时真实回调会全线被拒而测试全绿）、回调时间戳的极值用例（`i64::MIN` 曾经
让偏差计算算术溢出 panic）。原先两个只打印 / 无断言的用例（`test_uuid_v4`、`test_str`）
已按这个标准删除。

## 已知限制与陷阱

**构建**

- `cargo check --no-default-features` 失败：`reqwest` 是 optional 依赖，但 `src/error.rs` 的错误枚举
  无条件持有 `reqwest::Error`，`src/pay.rs` / `src/async_impl/pay.rs` 也无条件 `use reqwest::header::…`，
  `src/retry.rs` 还调用 `reqwest::Error` 的方法。要修得同时动这几处，改 `error.rs` 一处不够。
- `cargo test --doc` 会失败（实测收集 **35** 个 doctest：`src/` 里 7 个示例能**编译**通过
  （`no_run`），其余 **28** 个来自 README，需要真实凭证与公网）—— 这是**有意**接受的：README 的示例需要真实凭证与
  公网，所以设了 `[lib] doctest = false`（默认的 `cargo test` 因此不收集它们），CI 也不跑 `--doc`。
  ⚠ 但这意味着源码文档注释里的示例（`src/retry.rs`、`src/error.rs`、`src/pay.rs`、`src/notify.rs`、
  `src/cert.rs`）只在手动跑 `cargo test --doc` 时被**编译**（`no_run` 不执行）；改完请手动跑一次，
  CI 不会替你跑。
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
- **应答验签会改变 4xx 的错误类型**：无签名头的 4xx 返回 `VerifyError`（`Local`）而不是
  `ApiError`，此时**拿不到** `response.code` —— README 里「订单不存在是 `ApiError` +
  `ORDER_NOT_EXIST`」的处置路径在验签失败时**不适用**，必须按「结果未知」处置（查单 + 告警），
  不得当成「订单不存在」。5xx 无签名头仍返回 `ApiError`，但消息里带 `[未验签]` 标记
  （靠 `api_error_unverified`，它保留 `code` / `detail`，所以 `SYSTEM_ERROR` 仍会重试）。
- **微信支付公钥模式**（serial 形如 `PUB_KEY_ID_…`）必须显式配置公钥，否则所有应答都以
  `UnknownPlatformSerial` 失败；该公钥**不在** `/v3/certificates` 里，遇到这个前缀**不会**
  去刷新证书（那是白打限流接口）。
- `set_platform_keys` / `with_platform_public_key` 之后客户端进入**静态密钥模式**：不自动拉取、
  不自动替换。手工灌入的索引 `fetched_at` 为空 ⇒ `needs_refresh()` 恒为 true，如果不做静态判定，
  每个请求前都会多打一次 `/v3/certificates`。
- 应答验签给每个响应加一次 RSA 验签（`verify_rsa_sha256` 每次都会重新解析 PEM 公钥，
  微秒~毫秒级）；要抠这部分开销就在索引里缓存已解析的公钥，别改成「跳过验签」。
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
