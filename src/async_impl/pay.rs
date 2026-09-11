use crate::debug;
use crate::error::PayError;
use crate::model::AppParams;
use crate::model::H5Params;
use crate::model::JsapiParams;
use crate::model::MicroParams;
use crate::model::NativeParams;
use crate::model::ParamsTrait;
use crate::model::RefundsParams;
use crate::pay::{WechatPay, WechatPayTrait};
use crate::request::HttpMethod;
use crate::response::AppResponse;
use crate::response::H5Response;
use crate::response::JsapiResponse;
use crate::response::MicroResponse;
use crate::response::RefundsResponse;
use crate::response::ResponseTrait;
use crate::response::WeChatResponse;
use crate::response::{CertificateResponse, NativeResponse};
use reqwest::header::{HeaderMap, REFERER};
use serde_json::{Map, Value};

#[cfg(not(feature = "async"))]
use reqwest::blocking::{Client, RequestBuilder};
#[cfg(feature = "async")]
use reqwest::{Client, RequestBuilder};

#[cfg(feature = "async")]
use maybe_async::maybe_async as maybe_async_attr;
#[cfg(not(feature = "async"))]
use maybe_async::must_be_sync as maybe_async_attr;

/// 发送请求并做 HTTP 状态检查，返回响应体文本。
///
/// 微信支付在失败时返回**非 2xx 状态码** + `{"code","message","detail"}` 响应体。
/// 必须先看状态码再看 body：若像 `.json::<R>()` 那样直接解析，错误响应体会被塞进
/// 字段全为 `Option` 的成功响应类型（例如 `JsapiResponse`），于是下单失败会伪装成
/// `Ok(JsapiResponse { code: Some("PARAM_ERROR"), prepay_id: None })`，
/// 调用方只能靠 `prepay_id` 为空去猜，且微信的错误码会全部丢失。
///
/// ⚠ 本函数只覆盖**非 2xx**。若微信以 HTTP 200 返回错误信封，`pay()` / `get_pay()`
/// 仍会把它解析进成功类型并返回 `Ok`（那些类型的字段全是 `Option`）。
/// 因此调用方还必须检查业务字段：`prepay_id` / `code_url` / `h5_url` 是否为空、
/// `code` 是否非空。只有 `refunds()` 通过 `WeChatResponse` 覆盖了 200 带错误码的情况。
#[maybe_async_attr]
async fn send_and_check(builder: RequestBuilder) -> Result<String, PayError> {
    let response = builder.send().await?;
    let status = response.status();
    let text = response.text().await?;
    debug!("status: {} body: {}", status, text);
    if !status.is_success() {
        return Err(PayError::api_error(status.as_u16(), &text));
    }
    Ok(text)
}

impl WechatPay {
    #[maybe_async_attr]
    pub async fn pay<P: ParamsTrait, R: ResponseTrait>(
        &self,
        method: HttpMethod,
        url: &str,
        json: P,
    ) -> Result<R, PayError> {
        let json_str = json.to_json();
        debug!("json_str: {}", json_str);
        let mut map: Map<String, Value> = serde_json::from_str(&json_str)?;
        map.insert("appid".to_owned(), self.appid().into());
        map.insert("mchid".to_owned(), self.mch_id().into());
        map.insert("notify_url".to_owned(), self.notify_url().into());
        let body = serde_json::to_string(&map)?;
        let headers = self.build_header(method.clone(), url, body.as_str())?;
        let client = Client::new();
        let url = format!("{}{}", self.base_url(), url);
        debug!("url: {} body: {}", url, body);
        let builder = match method {
            HttpMethod::GET => client.get(url),
            HttpMethod::POST => client.post(url),
            HttpMethod::PUT => client.put(url),
            HttpMethod::DELETE => client.delete(url),
            HttpMethod::PATCH => client.patch(url),
        };

        let text = send_and_check(builder.headers(headers).body(body)).await?;
        Ok(serde_json::from_str::<R>(&text)?)
    }

    #[maybe_async_attr]
    pub async fn get_pay<R: ResponseTrait>(&self, url: &str) -> Result<R, PayError> {
        let body = "";
        let headers = self.build_header(HttpMethod::GET, url, body)?;
        let client = Client::new();
        let url = format!("{}{}", self.base_url(), url);
        debug!("url: {} body: {}", url, body);
        let text = send_and_check(client.get(url).headers(headers).body(body)).await?;
        Ok(serde_json::from_str::<R>(&text)?)
    }

    #[maybe_async_attr]
    pub async fn h5_pay(&self, params: H5Params) -> Result<H5Response, PayError> {
        let url = "/v3/pay/transactions/h5";
        self.pay(HttpMethod::POST, url, params).await
    }
    #[maybe_async_attr]
    pub async fn app_pay(&self, params: AppParams) -> Result<AppResponse, PayError> {
        let url = "/v3/pay/transactions/app";
        self.pay(HttpMethod::POST, url, params)
            .await
            .map(|mut result: AppResponse| {
                if let Some(prepay_id) = &result.prepay_id {
                    result.sign_data = Some(self.mut_sign_data("", prepay_id));
                }
                result
            })
    }
    #[maybe_async_attr]
    pub async fn jsapi_pay(&self, params: JsapiParams) -> Result<JsapiResponse, PayError> {
        let url = "/v3/pay/transactions/jsapi";
        self.pay(HttpMethod::POST, url, params)
            .await
            .map(|mut result: JsapiResponse| {
                if let Some(prepay_id) = &result.prepay_id {
                    result.sign_data = Some(self.mut_sign_data("prepay_id=", prepay_id));
                }
                result
            })
    }
    #[maybe_async_attr]
    pub async fn micro_pay(&self, params: MicroParams) -> Result<MicroResponse, PayError> {
        let url = "/v3/pay/transactions/jsapi";
        self.pay(HttpMethod::POST, url, params)
            .await
            .map(|mut result: MicroResponse| {
                if let Some(prepay_id) = &result.prepay_id {
                    result.sign_data = Some(self.mut_sign_data("prepay_id=", prepay_id));
                }
                result
            })
    }
    #[maybe_async_attr]
    pub async fn native_pay(&self, params: NativeParams) -> Result<NativeResponse, PayError> {
        let url = "/v3/pay/transactions/native";
        self.pay(HttpMethod::POST, url, params).await
    }

    #[maybe_async_attr]
    pub async fn certificates(&self) -> Result<CertificateResponse, PayError> {
        let url = "/v3/certificates";
        self.get_pay(url).await
    }
    #[maybe_async_attr]
    pub async fn get_weixin<S>(&self, h5_url: S, referer: S) -> Result<Option<String>, PayError>
    where
        S: AsRef<str>,
    {
        let client = Client::new();
        let mut headers = HeaderMap::new();
        headers.insert(REFERER, referer.as_ref().parse().unwrap());
        let text = client
            .get(h5_url.as_ref())
            .headers(headers)
            .send()
            .await?
            .text()
            .await?;
        text.split("\n")
            .find(|line| line.contains("weixin://"))
            .map(|line| {
                line.split(r#"""#)
                    .find(|line| line.contains("weixin://"))
                    .map(|line| line.to_string())
            })
            .ok_or_else(|| PayError::WeixinNotFound)
    }

    #[maybe_async_attr]
    pub async fn refunds(
        &self,
        params: RefundsParams,
    ) -> Result<WeChatResponse<RefundsResponse>, PayError> {
        let url = "/v3/refund/domestic/refunds";
        let body = params.to_json();
        let headers = self.build_header(HttpMethod::POST, url, body.as_str())?;
        let client = Client::new();
        let url = format!("{}{}", self.base_url(), url);
        debug!("url: {} body: {}", url, body);
        let builder = client.post(url);

        let text = send_and_check(builder.headers(headers).body(body)).await?;
        Ok(serde_json::from_str::<WeChatResponse<RefundsResponse>>(
            &text,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use dotenvy::dotenv;
    use crate::error::PayError;
    use crate::model::{
        AppParams, H5Params, H5SceneInfo, JsapiParams, MicroParams, NativeParams, RefundsParams,
    };
    use crate::pay::{PayNotifyTrait, WechatPay};
    use crate::response::Certificate;
    use crate::util;
    use std::io::Write;
    use tracing::debug;

    #[test]
    #[cfg(not(feature = "async"))]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub fn test_jsapi_pay() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();
        let body = wechat_pay
            .jsapi_pay(JsapiParams::new(
                "测试支付1分",
                "1243243",
                1.into(),
                "open_id".into(),
            ))
            .expect("jsapi_pay error");
        debug!("body: {:?}", body);
    }

    #[test]
    #[cfg(not(feature = "async"))]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub fn test_micro_pay() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();
        let body = wechat_pay
            .micro_pay(MicroParams::new(
                "测试支付1分",
                "1243243",
                1.into(),
                "open_id".into(),
            ))
            .expect("micro_pay error");
        debug!("body: {:?}", body);
    }

    #[test]
    #[cfg(not(feature = "async"))]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub fn test_app_pay() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();
        let body = wechat_pay
            .app_pay(AppParams::new("测试支付1分", "1243243", 1.into()))
            .expect("app_pay error");
        debug!("body: {:?}", body);
    }

    #[test]
    pub fn test_str() {
        let str = r#" deeplink : "weixin://wap/pay?prepayid%3Dwx122129234529163c948432e26bc0030000&package=4206921243&noncestr=1705066163&sign=788bc4a9f8f44c6f708aff38c4b48a85""#;
        let _strs = str.split(r#"""#).find(|line| line.contains("weixin://"));
    }

    #[test]
    #[cfg(not(feature = "async"))]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub fn test_h5_pay() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();
        let body = wechat_pay
            .h5_pay(H5Params::new(
                "测试支付1分",
                util::random_trade_no().as_str(),
                1.into(),
                H5SceneInfo::new("183.6.105.141", "ipa软件下载", "https://mydomain.com"),
            ))
            .expect("h5_pay error");
        let weixin_url = wechat_pay
            .get_weixin(body.h5_url.unwrap().as_str(), "https://mydomain.com")
            .unwrap();
        debug!("weixin_url: {}", weixin_url.unwrap());
    }

    #[test]
    #[cfg(not(feature = "async"))]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub fn test_certificates() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();
        let response = wechat_pay.certificates().expect("certificates error");
        let data = response.data.unwrap().first().unwrap().clone();
        let ciphertext = data.encrypt_certificate.ciphertext;
        let nonce = data.encrypt_certificate.nonce;
        let associated_data = data.encrypt_certificate.associated_data;
        let data = wechat_pay
            .decrypt_bytes(ciphertext, nonce, associated_data)
            .unwrap();
        let pub_key = util::x509_to_pem(data.as_slice()).unwrap();
        let mut pub_key_file = std::fs::File::create("pubkey.pem").unwrap();
        pub_key_file.write_all(pub_key.as_bytes()).unwrap();

        let (pub_key_valid, expire_timestamp) = util::x509_is_valid(data.as_slice()).unwrap();
        debug!(
            "pub key valid:{} expire_timestamp:{}",
            pub_key_valid, expire_timestamp
        ); //证书是否可用,过期时间
        debug!("pub key: {}", pub_key);
    }

    #[test]
    #[cfg(not(feature = "async"))]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub fn test_decode_certificates() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();
        let response = wechat_pay.certificates().expect("certificates error");
        let data: Certificate = response.data.unwrap()[0].clone();
        let ciphertext = data.encrypt_certificate.ciphertext;
        let nonce = data.encrypt_certificate.nonce;
        let associated_data = data.encrypt_certificate.associated_data;
        let data = wechat_pay
            .decrypt_bytes(ciphertext, nonce, associated_data)
            .unwrap();
        debug!("data: {}", String::from_utf8_lossy(data.as_ref()));
    }

    #[test]
    #[cfg(not(feature = "async"))]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub fn test_blocking_refunds() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();

        let req = RefundsParams::new("123456", 1, 1, None, Some("123456"));

        match wechat_pay.refunds(req) {
            Ok(body) if body.is_success() => debug!("refunds success: {:?}", body.ok()),
            Ok(body) => debug!("refunds rejected: {:?}", body.err()),
            Err(PayError::ApiError { status, response }) => {
                debug!("refunds failed: http {status}, {response}");
            }
            Err(e) => debug!("refunds error: {e}"),
        }
    }

    #[inline]
    fn init_log() {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_line_number(true)
            .init();
    }

    #[tokio::test]
    #[cfg(feature = "async")]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub async fn test_native_pay() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();
        let body = wechat_pay
            .native_pay(NativeParams::new("测试支付1分", "1243243", 1.into()))
            .await
            .expect("pay fail");
        debug!("body: {:?}", body);
    }
    #[test]
    #[cfg(not(feature = "async"))]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub fn test_native_pay() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();
        let body = wechat_pay
            .native_pay(NativeParams::new("测试支付1分", "1243243", 1.into()))
            .expect("pay fail");
        debug!("body: {:?}", body);
    }

    #[tokio::test]
    #[cfg(feature = "async")]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub async fn test_refunds() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();

        let req = RefundsParams::new("123456", 1, 1, None, Some("123456"));

        match wechat_pay.refunds(req).await {
            Ok(body) if body.is_success() => debug!("refunds success: {:?}", body.ok()),
            Ok(body) => debug!("refunds rejected: {:?}", body.err()),
            Err(PayError::ApiError { status, response }) => {
                debug!("refunds failed: http {status}, {response}");
            }
            Err(e) => debug!("refunds error: {e}"),
        }
    }

    #[test]
    #[cfg(not(feature = "async"))]
    #[ignore = "需要真实商户凭证 / 公网 / 未入库的 PEM fixture；用 `cargo test -- --ignored` 显式运行"]
    pub fn test_refunds() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();

        let req = RefundsParams::new("123456", 1, 1, None, Some("123456"));

        match wechat_pay.refunds(req) {
            Ok(body) if body.is_success() => debug!("refunds success: {:?}", body.ok()),
            Ok(body) => debug!("refunds rejected: {:?}", body.err()),
            Err(PayError::ApiError { status, response }) => {
                debug!("refunds failed: http {status}, {response}");
            }
            Err(e) => debug!("refunds error: {e}"),
        }
    }
}
