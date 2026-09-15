//! 平台证书 / 微信支付公钥的按键索引。
//!
//! 微信平台证书**轮换期会同时下发新旧两张且都在有效期内**，验签必须按请求头
//! `Wechatpay-Serial` 选择对应的密钥。写死单张证书的写法在轮换时会直接导致回调
//! 验签全部失败 —— 也就是订单不发货。
//!
//! 本模块提供：
//!
//! * [`PlatformKeys`](crate::cert::PlatformKeys) —— `serial_no -> 公钥 PEM` 的索引，
//!   支持按 serial 选键；
//! * [`WechatPay::fetch_platform_keys`](crate::pay::WechatPay::fetch_platform_keys) ——
//!   拉取 `GET /v3/certificates`、逐张解密并建索引（**无状态**，返回给调用方）；
//! * [`WechatPay::refresh_platform_keys`](crate::pay::WechatPay::refresh_platform_keys) ——
//!   拉取并**整体替换**客户端内部那份索引（出站应答验签用的就是它）。
//!
//! 客户端内部那份索引由 SDK 自己维护：每次请求前按 12 小时窗口判断是否该刷新
//! （冷启动时就是首次拉取），遇到未知 `Wechatpay-Serial` 会**再刷新一次并重验**。
//! 回调验签请用手里的 [`PlatformKeys`](crate::cert::PlatformKeys)：`PlatformKeys::verify_notify`，
//! 一旦拿到
//! [`PayError::UnknownPlatformSerial`](crate::error::PayError::UnknownPlatformSerial)
//! 就**立即重新拉取**再重试。

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reqwest::header::HeaderMap;

use crate::async_impl::pay::{ResponseCheck, sleep_for};
use crate::error::PayError;
use crate::macros::debug;
use crate::pay::{PayNotifyTrait, RefreshGuard, ResponseVerify, WechatPay};
use crate::request::HttpMethod;
use crate::response::{Certificate, CertificateResponse, EncryptCertificate};
use crate::retry::RequestKind;
use crate::util;

#[cfg(feature = "async")]
use maybe_async::maybe_async as maybe_async_attr;
#[cfg(not(feature = "async"))]
use maybe_async::must_be_sync as maybe_async_attr;

/// 官方要求至少每 12 小时重新拉取一次平台证书列表。
pub const REFRESH_INTERVAL_SECS: i64 = 12 * 60 * 60;

/// 由**未知 `Wechatpay-Serial`** 触发的刷新之间，至少间隔这么多秒。
///
/// 伪造一个 serial 就能触发刷新，而 `GET /v3/certificates` 是官方要求「至少每 12 小时」
/// 的接口 —— 没有这道闸，应答验签本身就成了可被外部触发的流量放大器。
pub const UNKNOWN_SERIAL_REFRESH_MIN_INTERVAL_SECS: i64 = 60;

/// 等待并发刷新结束时的轮询间隔与轮数（有界：异步模式下不能无限等）。
///
/// 2s 的等待预算：够一次正常的证书拉取（含一次重试），又不至于让业务请求长时间挂在
/// 「等别人刷证书」上。等不到也没关系 —— 失败是**响亮且保守**的（`UnknownPlatformSerial`，
/// 对写接口是「结果未知」），不会把没验签的应答当成功。
const REFRESH_WAIT_INTERVAL: Duration = Duration::from_millis(200);
const REFRESH_WAIT_ROUNDS: u32 = 10;

/// 微信支付公钥模式的 `Wechatpay-Serial` 前缀。
///
/// 这种公钥**不在** `GET /v3/certificates` 里（要从商户平台下载自行更新），
/// 所以对它刷新平台证书是白打接口 —— 直接给出可操作的错误。
pub(crate) const PUBLIC_KEY_ID_PREFIX: &str = "PUB_KEY_ID_";

/// 打破 `request` ↔ 证书刷新 这个异步调用环。
///
/// 环的形状：`request` → `verify_response` → `verify_signed` → `refresh_for_unknown_serial`
/// → `fetch_and_install_keys` → `fetch_keys_from_api` → `request`。异步 fn 的 Future 会在
/// 类型层面内联，环会让它**无限大**（编译期 E0733），必须在其中一条边上引入间接。
///
/// 选在「拉证书」这条边上：它每次刷新（12 小时 / 轮换）才走一次，而不是每请求一次 ——
/// 顺带避免在热路径上每请求多一次 `Box::pin` 分配。同步模式下没有这个问题（普通递归调用），
/// 因此这份宏在同步模式下直接求值。
#[cfg(feature = "async")]
macro_rules! indirect {
    ($future:expr) => {
        Box::pin($future).await
    };
}

/// 同步模式：无需间接（见异步版本的说明）。
#[cfg(not(feature = "async"))]
macro_rules! indirect {
    ($future:expr) => {
        $future
    };
}

/// `serial_no -> 公钥 PEM` 的索引。
///
/// 不含网络逻辑：由调用方决定何时拉取与刷新，因此可以离线单测、也便于放进
/// 应用状态里跨请求复用。
#[derive(Debug, Default, Clone)]
pub struct PlatformKeys {
    keys: HashMap<String, String>,
    fetched_at: Option<i64>,
}

impl PlatformKeys {
    /// 构造一个空索引。
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入或覆盖一个密钥；返回是否覆盖了已有条目。
    pub fn insert(
        &mut self,
        serial_no: impl Into<String>,
        public_key_pem: impl Into<String>,
    ) -> bool {
        self.keys
            .insert(serial_no.into(), public_key_pem.into())
            .is_some()
    }

    /// 按 `Wechatpay-Serial` 取公钥 PEM。
    pub fn get(&self, serial_no: &str) -> Option<&str> {
        self.keys.get(serial_no).map(String::as_str)
    }

    /// 索引中已有的密钥数量。轮换期正常会有 2 个。
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// 索引是否为空（从未成功拉取过，或返回的证书列表为空）。
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// 当前索引里的全部 serial_no（排序后），用于排查「到底认了哪几张证书」。
    pub fn serials(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.keys.keys().map(String::as_str).collect();
        out.sort_unstable();
        out
    }

    /// 记录一次成功拉取的时间（unix 秒）。
    pub fn mark_refreshed(&mut self, now_unix_secs: i64) {
        self.fetched_at = Some(now_unix_secs);
    }

    /// 最近一次成功拉取的时间（unix 秒）；从未拉取过则为 `None`。
    pub fn fetched_at(&self) -> Option<i64> {
        self.fetched_at
    }

    /// 是否该重新拉取了。从未拉取过、或距上次拉取已达
    /// [`REFRESH_INTERVAL_SECS`] 都返回 `true`。
    pub fn needs_refresh(&self, now_unix_secs: i64) -> bool {
        match self.fetched_at {
            None => true,
            Some(fetched) => now_unix_secs.saturating_sub(fetched) >= REFRESH_INTERVAL_SECS,
        }
    }

    /// 用 `serial_no` 对应的密钥验证 `timestamp\nnonce\nbody\n` 的签名。
    ///
    /// serial 不在索引里时返回 [`PayError::UnknownPlatformSerial`] —— 这通常意味着
    /// 微信正在轮换证书，应当重新拉取列表后重试，**不要**退化成用别的密钥去试。
    pub fn verify(
        &self,
        serial_no: &str,
        timestamp: &str,
        nonce: &str,
        body: &str,
        signature: &str,
    ) -> Result<(), PayError> {
        let public_key = self
            .get(serial_no)
            .ok_or_else(|| PayError::UnknownPlatformSerial(serial_no.to_string()))?;
        let message = format!("{timestamp}\n{nonce}\n{body}\n");
        util::verify_rsa_sha256(public_key, &message, signature)
    }
}

impl WechatPay {
    /// 拉取 `GET /v3/certificates`，解密每一张平台证书并建立 `serial_no -> 公钥 PEM` 索引。
    ///
    /// 返回值是**全新**的索引：轮换期微信会同时下发新旧两张，因此调用方应当整体替换
    /// 手里的旧索引，而不是逐条合并（合并会把微信已经撤下的旧证书永久留在索引里）。
    /// 已过期的证书会被丢弃。
    ///
    /// ⚠ 这是**无状态**的：只把索引返回给调用方，不写进客户端 —— 客户端内部那份由
    /// [`WechatPay::refresh_platform_keys`] 维护，出站应答验签用的是它。
    ///
    /// 拉取成功后请调用 [`PlatformKeys::mark_refreshed`] 记录时间，以便
    /// [`PlatformKeys::needs_refresh`] 判断下次刷新时机。
    #[maybe_async_attr]
    pub async fn fetch_platform_keys(&self) -> Result<PlatformKeys, PayError> {
        self.fetch_keys_from_api().await
    }

    /// 拉取平台证书，并把新索引**整体替换**进客户端（出站应答验签用的就是它）。
    ///
    /// **单飞**：同一时刻只有一个人打证书接口，其余调用等它结束后用它的结果。
    /// 刷新失败不会动已有索引：宁可继续用旧证书，也不要变成「没有密钥可用」。
    ///
    /// 静态密钥模式（[`WechatPay::with_platform_public_key`] /
    /// [`WechatPay::set_platform_keys`]）下返回错误：平台证书列表里没有你配置的那些
    /// key，整体替换只会把它们抹掉。
    ///
    /// ⚠ 触发源是**未鉴权输入**时（回调里的 `Wechatpay-Serial`）请改用
    /// [`WechatPay::refresh_platform_keys_for_unknown_serial`] —— 它多一道 60s 限流。
    #[maybe_async_attr]
    pub async fn refresh_platform_keys(&self) -> Result<PlatformKeys, PayError> {
        if !self.auto_refresh_keys() {
            return Err(PayError::VerifyError(
                "当前客户端使用调用方提供的密钥（公钥模式 / set_platform_keys），不会拉取平台证书；\
                 要更新请重新调用 set_platform_keys 或重建客户端"
                    .to_string(),
            ));
        }
        self.fetch_and_install_keys().await
    }

    /// 遇到未知 `Wechatpay-Serial` 时的刷新入口：**60s 最小间隔 + 单飞**。
    ///
    /// 出站应答的验签由 SDK 内部走这条路；**回调验签**拿到
    /// [`PayError::UnknownPlatformSerial`] 时也应当调它，而不是
    /// [`WechatPay::refresh_platform_keys`] —— 回调里的 serial 是未鉴权输入，
    /// 伪造一个就能触发刷新，没有限流等于把 `/v3/certificates`（官方要求 12 小时一次的
    /// 接口）变成可被外部触发的放大器。
    ///
    /// 返回 `Ok` 只表示刷新流程结束（也可能由并发的另一次刷新完成）：调用方应重新取
    /// [`WechatPay::platform_keys`] 快照再验一次，仍未知就按「轮换尚未完成」处理。
    #[maybe_async_attr]
    pub async fn refresh_platform_keys_for_unknown_serial(
        &self,
        serial: &str,
    ) -> Result<(), PayError> {
        if !self.auto_refresh_keys() {
            return Err(PayError::UnknownPlatformSerial(format!(
                "{serial}:当前客户端使用调用方提供的密钥（公钥模式 / set_platform_keys），\
                 不会自动拉取平台证书"
            )));
        }
        // 限流只在「没有人正在刷」时判断：等同伴不算「刚刷新过」。
        if !self.key_refresh.in_flight.load(Ordering::SeqCst) {
            let now = crate::util::now_unix_secs();
            let last_attempt = self.key_refresh.last_attempt.load(Ordering::SeqCst);
            if last_attempt != 0
                && now.saturating_sub(last_attempt) < UNKNOWN_SERIAL_REFRESH_MIN_INTERVAL_SECS
            {
                return Err(PayError::UnknownPlatformSerial(format!(
                    "{serial}:距上次刷新平台证书不足 {UNKNOWN_SERIAL_REFRESH_MIN_INTERVAL_SECS}s，\
                     已跳过重复刷新（等微信的证书更新后再试）"
                )));
            }
            self.key_refresh.last_attempt.store(now, Ordering::SeqCst);
        }
        // ⚠ 刷新失败必须归一到 `UnknownPlatformSerial`（R7）：这个错误来自**另一条请求**
        // （拉证书）。若让它以原类型逃进业务请求的失败分类，会同时踩两个坑：
        // 连接类 / 429 / 5xx 会被判成「可重试」→ **重发那条应答已经收到的请求**
        // （R4 明确要求绝不重发业务请求）；4xx 又会让调用方拿到
        // `may_have_taken_effect() == false`（「确定没受理」），可微信明明已经回过它 ——
        // 正是这条无法验签的应答触发了本次刷新。
        self.fetch_and_install_keys()
            .await
            .map(|_| ())
            .map_err(|err| match err {
                // 等待并发刷新超时这类错误已经是同一个结论，别套娃。
                PayError::UnknownPlatformSerial(message) => {
                    PayError::UnknownPlatformSerial(message)
                }
                other => PayError::UnknownPlatformSerial(format!(
                    "{serial}:应答由未知 serial 签名，刷新平台证书也失败了（{other}），无法完成验签"
                )),
            })
    }

    /// 确保密钥索引可用：Auto 模式下按 12 小时窗口刷新（索引为空时就是冷启动引导）。
    ///
    /// 由 `request_json` / `request_no_content` 在**发送前**调用。静态密钥模式与关闭验签时
    /// 什么都不做 —— 密钥要么由调用方负责，要么根本用不上。
    #[maybe_async_attr]
    pub(crate) async fn ensure_keys(&self) -> Result<(), PayError> {
        if self.response_verify == ResponseVerify::Disabled || !self.auto_refresh_keys() {
            return Ok(());
        }
        if !self.keys_need_refresh() {
            return Ok(());
        }
        self.refresh_platform_keys().await?;
        if self.platform_keys_read().is_empty() {
            return Err(PayError::VerifyError(
                "平台证书索引为空：自动拉取平台证书失败或仍在进行中，无法验证应答签名".to_string(),
            ));
        }
        Ok(())
    }

    /// 当前索引是否需要刷新（读锁只在语句内存活，不跨 `.await`）。
    fn keys_need_refresh(&self) -> bool {
        self.platform_keys_read()
            .needs_refresh(crate::util::now_unix_secs())
    }

    /// 拉取 → 自校验 → 整体替换 + 记录刷新时间。失败时不动已有索引。
    ///
    /// **单飞**：同一时刻只有一个人打 `/v3/certificates`；CAS 抢不到标记的人等它结束，
    /// 然后用手里的索引（调用方会重新查表验签，仍未知就按「轮换未完成」处理）。
    /// 冷启动与 12 小时窗口的并发请求同样走这里 —— 否则 N 个并发请求会对官方要求
    /// 「至少每 12 小时一次」的接口发出 N 次请求。
    #[maybe_async_attr]
    async fn fetch_and_install_keys(&self) -> Result<PlatformKeys, PayError> {
        for _ in 0..REFRESH_WAIT_ROUNDS {
            if let Some(_guard) = RefreshGuard::acquire(&self.key_refresh) {
                return self.fetch_and_install_keys_once().await;
            }
            // ⚠ 等待的判据只能是「标记已释放」，不能用 `needs_refresh`：命中未知 serial 时
            // 索引可能**既新鲜又不含那个 serial**（轮到别人去装），用它当完成信号会让等待者
            // 抢在安装之前返回旧索引。
            sleep_for(REFRESH_WAIT_INTERVAL).await;
            if !self.key_refresh.in_flight.load(Ordering::SeqCst) {
                return Ok(self.platform_keys());
            }
        }
        Err(PayError::UnknownPlatformSerial(format!(
            "平台证书正在刷新中，等待超过 {}ms 仍未完成",
            REFRESH_WAIT_INTERVAL.as_millis() * u128::from(REFRESH_WAIT_ROUNDS)
        )))
    }

    /// 真正去拉一次并安装（调用方已持有单飞标记）。
    #[maybe_async_attr]
    async fn fetch_and_install_keys_once(&self) -> Result<PlatformKeys, PayError> {
        // `indirect!` 是为了打破 `request ↔ 证书刷新` 的异步调用环（见宏的说明）。
        let keys = indirect!(self.fetch_keys_from_api())?;
        {
            let mut guard = self.platform_keys_write();
            // 装之前**复查**：这期间调用方可能已经 `set_platform_keys` /
            // `with_platform_public_key` 切到静态密钥模式 —— 那时覆盖掉他灌进来的密钥是
            // 不可逆的（静态模式下不会再有下一次自动拉取）。`static_keys` 只挡「发起」，
            // 挡不住「安装」，所以这道复查必须在写锁内。
            if !self.auto_refresh_keys() {
                debug!("客户端已切到静态密钥模式，放弃安装本次拉取结果");
                return Ok(guard.clone());
            }
            *guard = keys.clone();
            guard.mark_refreshed(crate::util::now_unix_secs());
        }
        Ok(keys)
    }

    /// 拉取证书列表、解密建索引并自校验（不落库）。
    #[maybe_async_attr]
    async fn fetch_keys_from_api(&self) -> Result<PlatformKeys, PayError> {
        // 证书列表的应答**不能**先按索引验签（轮换期它由本地还不认识的新证书签名），
        // 所以这条请求走自校验模式：拿到响应里的证书后再回头验它。
        let raw = self
            .request(
                HttpMethod::GET,
                "/v3/certificates",
                "",
                RequestKind::Read,
                ResponseCheck::CertificateSelfCheck,
            )
            .await?;
        let response: CertificateResponse = serde_json::from_slice(&raw.body)?;
        let keys = self.build_keys_from_response(&response)?;

        if keys.is_empty() {
            return Err(PayError::VerifyError(
                "平台证书列表为空（或全部已过期），无法建立密钥索引".to_string(),
            ));
        }
        if self.response_verify == ResponseVerify::Required {
            self.self_check_certificate_response(&raw.headers, &raw.body, &keys)?;
        }
        Ok(keys)
    }

    /// 把证书列表响应解密成 `serial_no -> 公钥 PEM` 索引，丢弃已过期的证书。
    ///
    /// 不校验「是否为空」：空列表对 [`WechatPay::certificates`] 是合法结果（公钥模式的商户
    /// 可能压根没有平台证书），只有需要拿它验签的调用方才该把空列表当成失败。
    pub(crate) fn build_keys_from_response(
        &self,
        response: &CertificateResponse,
    ) -> Result<PlatformKeys, PayError> {
        let mut keys = PlatformKeys::new();
        for cert in response.data.iter().flatten() {
            let Certificate {
                serial_no,
                encrypt_certificate,
                ..
            } = cert;
            let EncryptCertificate {
                ciphertext,
                nonce,
                associated_data,
                ..
            } = encrypt_certificate;

            let plaintext = self.decrypt_bytes(ciphertext, nonce, associated_data)?;
            // 过期证书直接丢弃：留着它只会让「拿别的密钥去试」看起来可行。
            // （`x509_is_valid` 的第二个返回值是 not_after，需要精确时间时用它。）
            let (valid, _) = util::x509_is_valid(plaintext.as_slice())?;
            if !valid {
                debug!("丢弃已过期的平台证书 {serial_no}");
                continue;
            }
            let public_key_pem = util::x509_to_pem(plaintext.as_slice())
                .map_err(|e| PayError::VerifyError(format!("平台证书解析失败: {e}")))?;
            keys.insert(serial_no.clone(), public_key_pem);
        }
        Ok(keys)
    }

    /// 用**本次响应下发**的、`Wechatpay-Serial` 指向的那张证书验证本次响应。
    ///
    /// ⚠ 这不是信任锚，也不是安全边界：公钥就来自同一个（尚未验签的）body，
    /// 真正的保证是 TLS 与「只有商户和微信持有 `v3_key`」的 AEAD 解密。
    /// 它成立的意义在于**可用性**：轮换期微信用新证书签名，本地索引里还没有新证书 ——
    /// 没有这条自证路径就永远学不到新证书（刷新请求自己验不过，见 R4/R5）。
    pub(crate) fn self_check_certificate_response(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        keys: &PlatformKeys,
    ) -> Result<(), PayError> {
        let headers = crate::notify::response_signature_headers(headers).map_err(|missing| {
            PayError::VerifyError(format!(
                "平台证书应答缺少签名头 {}，无法自校验；若链路上有代理 / CDN，\
                 请检查它们是否过滤了 Wechatpay-* 头",
                missing.join(" / ")
            ))
        })?;
        crate::notify::check_timestamp_skew(&headers.timestamp, crate::util::now_unix_secs())?;
        let body = std::str::from_utf8(body).map_err(|e| {
            PayError::VerifyError(format!(
                "平台证书应答不是合法 UTF-8，无法按原始字节验签: {e}"
            ))
        })?;
        keys.verify(
            &headers.serial,
            &headers.timestamp,
            &headers.nonce,
            body,
            &headers.signature,
        )
        .map_err(|err| match err {
            PayError::UnknownPlatformSerial(serial) => PayError::UnknownPlatformSerial(format!(
                "{serial}:证书列表应答由该 serial 签名，但响应体里没有这张证书，无法自校验"
            )),
            other => other,
        })
    }
}
