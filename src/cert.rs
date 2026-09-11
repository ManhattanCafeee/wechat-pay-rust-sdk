//! 平台证书 / 微信支付公钥的按键索引。
//!
//! 微信平台证书**轮换期会同时下发新旧两张且都在有效期内**，验签必须按请求头
//! `Wechatpay-Serial` 选择对应的密钥。写死单张证书的写法在轮换时会直接导致回调
//! 验签全部失败 —— 也就是订单不发货。
//!
//! 本模块提供：
//!
//! * [`PlatformKeys`] —— `serial_no -> 公钥 PEM` 的索引，支持按 serial 选键；
//! * [`WechatPay::fetch_platform_keys`] —— 拉取 `GET /v3/certificates`、逐张解密并建索引。
//!
//! 建议用法：启动时拉一次，之后每 [`REFRESH_INTERVAL_SECS`]（或更短）刷新一次；
//! 回调验签用 `PlatformKeys::verify_notify`，一旦拿到
//! [`PayError::UnknownPlatformSerial`] 就**立即重新拉取**再重试。

use std::collections::HashMap;

use crate::error::PayError;
use crate::pay::{PayNotifyTrait, WechatPay};
use crate::response::{Certificate, EncryptCertificate};
use crate::util;

#[cfg(feature = "async")]
use maybe_async::maybe_async as maybe_async_attr;
#[cfg(not(feature = "async"))]
use maybe_async::must_be_sync as maybe_async_attr;

/// 官方要求至少每 12 小时重新拉取一次平台证书列表。
pub const REFRESH_INTERVAL_SECS: i64 = 12 * 60 * 60;

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
    /// 手里的旧索引，而不是逐条合并。
    ///
    /// 拉取成功后请调用 [`PlatformKeys::mark_refreshed`] 记录时间，以便
    /// [`PlatformKeys::needs_refresh`] 判断下次刷新时机。
    #[maybe_async_attr]
    pub async fn fetch_platform_keys(&self) -> Result<PlatformKeys, PayError> {
        let response: crate::response::CertificateResponse =
            self.get_pay("/v3/certificates").await?;

        let mut keys = PlatformKeys::new();
        for cert in response.data.unwrap_or_default() {
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
            let public_key_pem = util::x509_to_pem(plaintext.as_slice())
                .map_err(|e| PayError::VerifyError(format!("平台证书解析失败: {e}")))?;
            keys.insert(serial_no, public_key_pem);
        }

        Ok(keys)
    }
}
