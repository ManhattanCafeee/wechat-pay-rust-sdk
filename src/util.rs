use base64::engine::general_purpose;
use base64::{DecodeError, Engine};
use rsa::pkcs8::DecodePublicKey;
use rsa::sha2::{Digest, Sha256};
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use std::error::Error;
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

pub fn random_trade_no() -> String {
    Uuid::new_v4().simple().to_string()
}

pub fn base64_encode<S>(content: S) -> String
where
    S: AsRef<[u8]>,
{
    general_purpose::STANDARD.encode(content)
}

pub fn base64_decode<S>(content: S) -> Result<Vec<u8>, DecodeError>
where
    S: AsRef<[u8]>,
{
    general_purpose::STANDARD.decode(content.as_ref())
}

pub fn x509_to_pem(content: &[u8]) -> Result<String, Box<dyn Error>> {
    let pem = pem::parse(content)?;
    let (_, cert) = x509_parser::parse_x509_certificate(pem.contents())?;
    let pub_key = base64_encode(cert.public_key().raw);
    let pub_key_lines = pub_key
        .chars()
        .collect::<Vec<char>>()
        .chunks(64)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<String>>()
        .join("\n");
    Ok(format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
        pub_key_lines
    ))
}

pub fn x509_is_valid(content: &[u8]) -> Result<(bool, i64), Box<dyn Error>> {
    let pem = pem::parse(content)?;
    let (_, cert) = x509_parser::parse_x509_certificate(pem.contents())?;
    //读取到证书的有效期
    let expire_time = cert.validity().is_valid();
    Ok((expire_time, cert.validity.not_after.timestamp()))
}
