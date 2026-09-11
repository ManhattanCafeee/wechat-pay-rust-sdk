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
use crate::response::{CertificateResponse, NativeResponse, TransactionResponse};
use crate::retry::{Delivery, RequestKind, classify, should_retry};
use reqwest::header::{HeaderMap, REFERER};
use serde_json::{Map, Value};
use std::time::Duration;

#[cfg(feature = "async")]
use reqwest::RequestBuilder;
#[cfg(not(feature = "async"))]
use reqwest::blocking::RequestBuilder;

#[cfg(feature = "async")]
use maybe_async::maybe_async as maybe_async_attr;
#[cfg(not(feature = "async"))]
use maybe_async::must_be_sync as maybe_async_attr;

/// 发送请求并做 HTTP 状态检查，返回 `(状态码, 响应体文本)`。
///
/// 微信支付在失败时返回**非 2xx 状态码** + `{"code","message","detail"}` 响应体。
/// 必须先看状态码再看 body：若像 `.json::<R>()` 那样直接解析，错误响应体会被塞进
/// 字段全为 `Option` 的成功响应类型（例如 `JsapiResponse`），于是下单失败会伪装成
/// `Ok(JsapiResponse { code: Some("PARAM_ERROR"), prepay_id: None })`，
/// 调用方只能靠 `prepay_id` 为空去猜，且微信的错误码会全部丢失。
///
/// ⚠ 这里只处理**非 2xx**。「2xx + 错误信封」由 [`is_error_envelope`] 在
/// [`WechatPay::request_json`] 里兜住 —— 两道检查缺一不可。
#[maybe_async_attr]
async fn send_and_check(builder: RequestBuilder) -> Result<(u16, String), PayError> {
    let response = builder.send().await?;
    let status = response.status();
    let text = response.text().await?;
    debug!("status: {} body: {}", status, text);
    if !status.is_success() {
        return Err(PayError::api_error(status.as_u16(), &text));
    }
    Ok((status.as_u16(), text))
}

/// 重试前的退避等待。
///
/// 同步模式阻塞当前线程；异步模式让出执行权（依赖 `tokio` 的 `time`）。
/// 两份定义按 feature 择一编译，同步版本**刻意不是 `async fn`** ——
/// 否则调用方在同步模式下拿到的是一个永不轮询的 Future，睡不成。
#[cfg(feature = "async")]
async fn sleep_for(delay: Duration) {
    tokio::time::sleep(delay).await;
}

/// 重试前的退避等待（同步实现，见异步版本的说明）。
#[cfg(not(feature = "async"))]
fn sleep_for(delay: Duration) {
    std::thread::sleep(delay);
}

/// 只取顶层 `code`，用于识别「状态码是 2xx、body 却是错误信封」。
#[derive(serde::Deserialize)]
struct EnvelopeProbe {
    code: Option<serde_json::Value>,
}

/// 2xx 的响应体是否是微信的错误信封。
///
/// 微信会以 200 返回 `{"code": "...", "message": "..."}`，而成功响应类型字段全是
/// `Option`，能把它照单全收 —— 于是失败**再一次**伪装成成功。这是单靠状态码检查
/// 覆盖不到的那一半。
///
/// 判据：顶层出现**非空字符串** `code`。本 crate 解析的成功响应
/// （`prepay_id` / `code_url` / `h5_url` / `data` / 交易对象）都不含顶层 `code`，
/// 所以不会误伤。解析失败（非 JSON、数组等）一律视为不是信封。
fn is_error_envelope(text: &str) -> bool {
    match serde_json::from_str::<EnvelopeProbe>(text) {
        Ok(probe) => probe
            .code
            .as_ref()
            .and_then(|value| value.as_str())
            .is_some_and(|code| !code.is_empty()),
        Err(_) => false,
    }
}

impl WechatPay {
    /// 底层请求：签名 → 发送 → 状态检查，失败时按策略重试。
    ///
    /// **不注入任何字段** —— `appid` / `mchid` / `notify_url` 的注入只发生在 `pay()` 里。
    /// 这是有意的：关单只需要 `mchid`、查单的 `mchid` 要放在 query string，
    /// 无脑注入这三个字段会让这些端点直接失败。
    ///
    /// ⚠ 传入的 `url` 会**原样参与签名**，所以 GET 的查询串必须拼进 `url`
    /// （微信要求签名串第二行是 path + `?` + query）。
    ///
    /// # 重试
    ///
    /// **「能不能重试」由 `kind`（重放语义）与失败分类共同决定，策略只控制次数与退避**
    /// —— 详见 [`crate::retry`]。核心是两条：
    ///
    /// - 确定没送到（连接失败）或微信明确未受理（429 / 5xx / 202）→ 任何接口都可重试
    /// - 结果未知（读写超时）→ **只有只读接口重试**；写接口交给调用方查单确认
    ///
    /// 每次尝试都重新签名：重试可能跨过 5 分钟的签名有效窗口，复用旧签名会直接 401。
    #[maybe_async_attr]
    async fn request(
        &self,
        method: HttpMethod,
        url: &str,
        body: &str,
        kind: RequestKind,
    ) -> Result<(u16, String), PayError> {
        let policy = self.policy_for(kind);
        let max_attempts = policy.max_attempts();
        let mut attempt: u32 = 1;

        loop {
            let headers = self.build_header(method.clone(), url, body)?;
            // 复用 `WechatPay` 持有的客户端：连接池跨请求共享，不必每次重新建连 / TLS 握手。
            let client = &self.client;
            let full_url = format!("{}{}", self.base_url(), url);
            debug!("url: {} body: {}", full_url, body);
            let builder = match method {
                HttpMethod::GET => client.get(full_url),
                HttpMethod::POST => client.post(full_url),
                HttpMethod::PUT => client.put(full_url),
                HttpMethod::DELETE => client.delete(full_url),
                HttpMethod::PATCH => client.patch(full_url),
            };

            let outcome = send_and_check(builder.headers(headers).body(body.to_owned())).await;

            // 202 是「已受理但尚未处理」，官方要求「请使用原参数重复请求一遍」，
            // 因此与 429 / 5xx 归为同一类：微信还没处理，可以安全重放。
            let delivery = match &outcome {
                Ok((202, _)) => Some(Delivery::Rejected),
                Ok(_) => None,
                Err(err) => classify(err),
            };
            let Some(delivery) = delivery else {
                return outcome;
            };

            if !should_retry(delivery, kind) || attempt >= max_attempts {
                // 重试用尽（或本就不该重试）。把 202 这类「2xx 但没处理」降级成错误返回，
                // 否则下游会拿空 body 去解析，报出一个与真实原因毫无关系的 JSON 错误。
                return match outcome {
                    Ok((status, text)) => Err(PayError::api_error(status, &text)),
                    Err(err) => Err(err),
                };
            }

            let delay = policy.delay_for(attempt);
            debug!(
                "retry {}/{} after {:?} ({:?})",
                attempt, max_attempts, delay, delivery
            );
            attempt += 1;
            sleep_for(delay).await;
        }
    }

    /// `request` + JSON 解析。
    ///
    /// 解析前先做一次错误信封检查：状态码是 2xx 但 body 是
    /// `{"code": "..."}` 时，同样返回 [`PayError::ApiError`]。
    #[maybe_async_attr]
    async fn request_json<R: ResponseTrait>(
        &self,
        method: HttpMethod,
        url: &str,
        body: &str,
        kind: RequestKind,
    ) -> Result<R, PayError> {
        let (status, text) = self.request(method, url, body, kind).await?;
        if is_error_envelope(&text) {
            // status 会是 200：如实记录真实状态码，业务原因看 response.code
            return Err(PayError::api_error(status, &text));
        }
        Ok(serde_json::from_str::<R>(&text)?)
    }

    /// `request` + 丢弃响应体，用于 **204 No Content**（例如关单）。
    #[maybe_async_attr]
    async fn request_no_content(
        &self,
        method: HttpMethod,
        url: &str,
        body: &str,
        kind: RequestKind,
    ) -> Result<(), PayError> {
        self.request(method, url, body, kind).await?;
        Ok(())
    }

    /// 通用下单：把 `params` 序列化后注入 `appid` / `mchid` / `notify_url` 再签名发送。
    ///
    /// 字段注入**只发生在这里** —— 其余端点走 [`WechatPay::request_json`]，不注入任何字段
    /// （关单的 body 只要 `mchid`，查单的 `mchid` 在 query string 里）。
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
        self.request_json(method, url, &body, RequestKind::Write)
            .await
    }

    /// 通用 GET：签名后请求 `url`，把响应解析成 `R`。
    ///
    /// ⚠ `url` 会**原样参与签名**，带查询参数时必须把查询串一起传进来。
    #[maybe_async_attr]
    pub async fn get_pay<R: ResponseTrait>(&self, url: &str) -> Result<R, PayError> {
        self.request_json(HttpMethod::GET, url, "", RequestKind::Read)
            .await
    }

    /// H5 支付（外部浏览器）：返回拉起微信收银台的 `h5_url`。
    #[maybe_async_attr]
    pub async fn h5_pay(&self, params: H5Params) -> Result<H5Response, PayError> {
        let url = "/v3/pay/transactions/h5";
        self.pay(HttpMethod::POST, url, params).await
    }
    /// APP 支付：返回 `prepay_id` 与给客户端拉起支付用的签名数据。
    ///
    /// ⚠ 签名数据的 `package` 前缀与其他支付方式不同，见 [`WechatPayTrait::mut_sign_data`]。
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
    /// JSAPI 支付（小程序 / 公众号）：返回 `prepay_id` 与给 `wx.requestPayment` 的签名数据。
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
    /// 付款码支付：返回 `prepay_id` 与签名数据。
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
    /// 扫码支付：返回 `code_url`，由商户自行生成二维码。
    #[maybe_async_attr]
    pub async fn native_pay(&self, params: NativeParams) -> Result<NativeResponse, PayError> {
        let url = "/v3/pay/transactions/native";
        self.pay(HttpMethod::POST, url, params).await
    }

    /// 获取平台证书列表（`GET /v3/certificates`）。
    ///
    /// ⚠ 返回的证书是**加密**的，需要解密后才能用。要做回调验签请直接用
    /// [`WechatPay::fetch_platform_keys`]，它会把解密与建索引一次做完。
    #[maybe_async_attr]
    pub async fn certificates(&self) -> Result<CertificateResponse, PayError> {
        let url = "/v3/certificates";
        self.get_pay(url).await
    }
    /// 从 H5 支付返回的页面里抓出 `weixin://` 拉起链接。
    ///
    /// ⚠ 实现是**逐行字符串扫描**（找包含 `weixin://` 的行再按 `"` 切分），
    /// 微信改页面结构就会失效 —— 生产环境建议前端自行处理 `h5_url`。
    /// ⚠ `h5_url` 会被直接 GET，且**没有白名单**：只能传微信返回的 `h5_url`
    /// 或你自己服务端的地址，**绝不能来自用户输入**（SSRF）。
    #[maybe_async_attr]
    pub async fn get_weixin<S>(&self, h5_url: S, referer: S) -> Result<Option<String>, PayError>
    where
        S: AsRef<str>,
    {
        // 同样复用共享客户端（目标是与微信 API 不同的主机，连接池不会命中，
        // 但仍比每次新建客户端好；且自动继承统一的超时配置）。
        let client = &self.client;
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

    /// 申请退款。
    ///
    /// 响应体与「查询退款」一致，因此返回 [`RefundsResponse`]。
    /// 退款是异步的：受理成功不等于退款成功，需用 [`WechatPay::query_refund`] 轮询
    /// `status` 直到离开 `PROCESSING`。
    #[maybe_async_attr]
    pub async fn refunds(&self, params: RefundsParams) -> Result<RefundsResponse, PayError> {
        let url = "/v3/refund/domestic/refunds";
        let body = params.to_json();
        self.request_json(HttpMethod::POST, url, &body, RequestKind::Refund)
            .await
    }

    /// 查询订单（按商户订单号）。
    ///
    /// `GET /v3/pay/transactions/out-trade-no/{out_trade_no}?mchid={mchid}`
    ///
    /// ⚠ `mchid` 是**唯一**的查询参数，且必须拼进 URL —— 微信签名串的第二行要求带上
    /// 查询串，漏掉会直接 401。订单不存在时微信返回 404 `ORDER_NOT_EXIST`，
    /// 会作为 `PayError::ApiError` 返回（`response.code`）。
    #[maybe_async_attr]
    pub async fn query_order(&self, out_trade_no: &str) -> Result<TransactionResponse, PayError> {
        // out_trade_no 与 mchid 的字符集被微信限制为数字/字母/`_`/`-`/`*`，无需 percent-encoding。
        let url = format!(
            "/v3/pay/transactions/out-trade-no/{out_trade_no}?mchid={}",
            self.mch_id()
        );
        self.request_json(HttpMethod::GET, &url, "", RequestKind::Read)
            .await
    }

    /// 关闭订单。
    ///
    /// `POST /v3/pay/transactions/out-trade-no/{out_trade_no}/close`，请求体**只有**
    /// `mchid` 一个字段。成功时微信返回 **204 No Content 且无响应体**，因此这里不解析
    /// body（用 `request_no_content` 而不是 `request_json`）。
    ///
    /// 注意：关单不是退款的替代 —— 已支付的订单只能退款。
    #[maybe_async_attr]
    pub async fn close_order(&self, out_trade_no: &str) -> Result<(), PayError> {
        let url = format!("/v3/pay/transactions/out-trade-no/{out_trade_no}/close");
        let body = serde_json::json!({ "mchid": self.mch_id() }).to_string();
        self.request_no_content(HttpMethod::POST, &url, &body, RequestKind::Write)
            .await
    }

    /// 查询退款（按商户退款单号）。
    ///
    /// `GET /v3/refund/domestic/refunds/{out_refund_no}` —— 该端点**没有查询参数**，
    /// `mchid` 由 Authorization 头隐含。响应体与「申请退款」一致，因此复用
    /// [`RefundsResponse`]。
    ///
    /// 退款是异步的：申请受理不等于退款成功，需要轮询本接口直到 `status` 离开
    /// `PROCESSING`（官方建议申请后每分钟查一次，5 分钟后降频）。
    /// 退款单不存在时微信返回 404 `RESOURCE_NOT_EXISTS`。
    #[maybe_async_attr]
    pub async fn query_refund(&self, out_refund_no: &str) -> Result<RefundsResponse, PayError> {
        let url = format!("/v3/refund/domestic/refunds/{out_refund_no}");
        self.request_json(HttpMethod::GET, &url, "", RequestKind::Read)
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::error::PayError;
    use dotenvy::dotenv;
    // 以下导入只被 `#[cfg(not(feature = "async"))]` 的测试使用，
    // 不加 cfg 会在 async 模式下产生 unused_imports 告警。
    #[cfg(not(feature = "async"))]
    use crate::model::{AppParams, H5Params, H5SceneInfo, JsapiParams, MicroParams};
    use crate::model::{NativeParams, RefundsParams};
    #[cfg(not(feature = "async"))]
    use crate::pay::PayNotifyTrait;
    use crate::pay::WechatPay;
    #[cfg(not(feature = "async"))]
    use crate::response::Certificate;
    #[cfg(not(feature = "async"))]
    use crate::util;
    #[cfg(not(feature = "async"))]
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
            // 受理成功 ≠ 退款成功，需再用 query_refund 轮询 status 到终态
            Ok(body) => debug!(
                "refunds status: {} refund_id: {}",
                body.status, body.refund_id
            ),
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
            Ok(body) => debug!(
                "refunds status: {} refund_id: {}",
                body.status, body.refund_id
            ),
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
            // 受理成功 ≠ 退款成功，需再用 query_refund 轮询 status 到终态
            Ok(body) => debug!(
                "refunds status: {} refund_id: {}",
                body.status, body.refund_id
            ),
            Err(PayError::ApiError { status, response }) => {
                debug!("refunds failed: http {status}, {response}");
            }
            Err(e) => debug!("refunds error: {e}"),
        }
    }
}
