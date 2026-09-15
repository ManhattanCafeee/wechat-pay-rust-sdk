use base64::engine::general_purpose;
use base64::{DecodeError, Engine};
use rsa::pkcs8::DecodePublicKey;
use rsa::sha2::{Digest, Sha256};
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::error::PayError;

/// 用 PEM 公钥验证 RSA-SHA256（PKCS#1 v1.5）签名，`signature_b64` 为 base64。
///
/// 微信支付 v3 的应答验签与回调验签都走这条路径；平台证书轮换时由调用方
/// 先从证书列表里挑出对应公钥再传进来。
pub fn verify_rsa_sha256(
    pub_key_pem: &str,
    message: &str,
    signature_b64: &str,
) -> Result<(), PayError> {
    let pub_key = RsaPublicKey::from_public_key_pem(pub_key_pem)
        .map_err(|e| PayError::VerifyError(format!("public key parser error: {e}")))?;
    let hashed = Sha256::new().chain_update(message).finalize();
    let signature = base64_decode(signature_b64)?;
    pub_key
        .verify(Pkcs1v15Sign::new::<Sha256>(), &hashed, signature.as_slice())
        .map_err(|e| PayError::VerifyError(e.to_string()))
}

/// 生成一个随机的商户订单号：UUID v4 去掉连字符后的 32 位十六进制串。
pub fn random_trade_no() -> String {
    Uuid::new_v4().simple().to_string()
}

/// 当前 Unix 时间戳（秒）。
///
/// 签名串里的 `timestamp` 字段与回调通知的防重放校验都用它取「现在」。
/// 直接读系统时钟；若系统时钟早于 1970-01-01（`duration_since` 返回 `Err`），
/// 返回 0 而不是 panic。
pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Base64 编码（STANDARD 字母表，带 padding）。
///
/// 微信的签名、密文、证书都走这一套，不要换成 URL-safe 字母表。
pub fn base64_encode<S>(content: S) -> String
where
    S: AsRef<[u8]>,
{
    general_purpose::STANDARD.encode(content)
}

/// Base64 解码（STANDARD 字母表）。
pub fn base64_decode<S>(content: S) -> Result<Vec<u8>, DecodeError>
where
    S: AsRef<[u8]>,
{
    general_purpose::STANDARD.decode(content.as_ref())
}

/// 把平台的 **PEM 证书**转换为 PEM 格式的**公钥**。
///
/// 用于 `GET /v3/certificates`：`encrypt_certificate.ciphertext` 解密出来的证书是
/// **PEM**（不是 DER），这里取出其中的 SubjectPublicKeyInfo，再重新包成
/// `-----BEGIN PUBLIC KEY-----`，供 [`verify_rsa_sha256`] 验签。
/// 换行由 [`pem::encode_config`] 按 LF、每行 64 列输出。
pub fn x509_to_pem(content: &[u8]) -> Result<String, Box<dyn Error>> {
    let cert_pem = pem::parse(content)?;
    let (_, cert) = x509_parser::parse_x509_certificate(cert_pem.contents())?;
    let pub_key = pem::Pem::new("PUBLIC KEY", cert.public_key().raw);
    let config = pem::EncodeConfig::new().set_line_ending(pem::LineEnding::LF);
    Ok(pem::encode_config(&pub_key, config))
}

/// 返回 `(证书当前是否有效, not_after 的 unix 秒时间戳)`。
///
/// 用于平台证书的到期过滤：`GET /v3/certificates` 的响应里可能带着已过期的证书
/// （轮换期新旧两张都在有效期内时两张都会保留，过期的则应当丢弃）。
///
/// 解析失败返回 [`PayError::VerifyError`]：与库其余错误类型一致，调用方可以直接 `?`。
pub fn x509_is_valid(content: &[u8]) -> Result<(bool, i64), PayError> {
    fn invalid(error: impl std::fmt::Display) -> PayError {
        PayError::VerifyError(format!("平台证书解析失败: {error}"))
    }
    let pem = pem::parse(content).map_err(invalid)?;
    let (_, cert) = x509_parser::parse_x509_certificate(pem.contents()).map_err(invalid)?;
    // 读取到证书的有效期
    let expire_time = cert.validity().is_valid();
    Ok((expire_time, cert.validity.not_after.timestamp()))
}

#[cfg(test)]
mod tests {
    use super::random_trade_no;

    #[test]
    fn random_trade_no_is_32_ascii_hex_chars() {
        let trade_no = random_trade_no();
        assert_eq!(trade_no.len(), 32);
        assert!(
            trade_no.chars().all(|c| c.is_ascii_hexdigit()),
            "订单号应为 ASCII 十六进制串，实际为 {trade_no}"
        );
    }

    #[test]
    fn random_trade_no_differs_across_calls() {
        assert_ne!(random_trade_no(), random_trade_no(), "两次调用应不同");
    }
}
