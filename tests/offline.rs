//! 离线集成测试：用本地 mock HTTP 服务替代微信支付网关。
//!
//! 这些测试**不联网、不需要任何真实商户凭证**，可在 CI 中运行。
//! 之所以可以这样测，是因为 `WechatPay` 的字段是 `pub` 的，
//! 可以直接把 `base_url` 指向本地 mock 服务。
//!
//! 同一份测试体在两种 feature 下都会编译并运行：
//! - 默认（sync）：`reqwest::blocking`，普通 `#[test]`
//! - `--features async`：`reqwest` + `#[tokio::test]`

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use wechat_pay_rust_sdk::error::PayError;
use wechat_pay_rust_sdk::model::{JsapiParams, RefundsParams};
use wechat_pay_rust_sdk::pay::{PayNotifyTrait, WechatPay};
use wechat_pay_rust_sdk::util;

/// 测试专用 RSA 私钥（PKCS#8, 2048 bit）。
///
/// ⚠️ 这是随仓库分发的一次性测试密钥，不对应任何真实商户账号，
/// 任何情况下都不得用于生产环境。
const TEST_PRIVATE_KEY: &str = "\
-----BEGIN PRIVATE KEY-----
MIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQDG7xriOMq/CDua
hArgAjJsQGT8gBKpHWXrlCbFkJD65oLuUTVyluG1bRLyhWzqh7WOuQqITmUV+s1D
PN8luaiNbVfk7C7e0SlvcuEfyC5E4E/xA97ipDnjtVzMXTWeRMibzqbT3r1dkgzk
06JGsKgrJQq0mqc0UfULUVJ+b+ioE4wjYA7UbPQmokj/JAk5ioy3fCH+QnfDsuLM
vY0GWCtXc/uuWfmry18PP1fOV//nN8+TGRtTFhx+FNLCvKmmUq5pIGlK/My5tkf8
RycGv8O2KKMJfieOJJVnaPhxkgmgql1qNgMVHGafoqjehFPTPNWYd2Kio2RuCf+Y
x/bD/VFrAgMBAAECggEAAi+fc9acQml56jKYQf+ULpoDN2ka4LkzelloQanbMKlM
IrKewTxvjNmpfc5siiPlTAUJh2zrxx2N7YwOMbEZbTsYiP9KwTplglWDVsuRgpfq
bk4UwBh+OwaDrLU7gRqQD8eUurr1tBZbm5TIcY7AZ6pNCftcagKajD4fshxTCgHE
BDEh9ZtnI0zhVkIBlf9zTNSM8CSvOn0v2xyFaDnGjQKH44Q2D0r4Opq4YnoqyZNK
xZU/0Yup8PhrCueWmsPqN9jtM5ouZ5TDJ29pSxUdL+5BwjsKvatCiCsi0OJJoX7T
TG5ACwLATaKVO4Kv2cn7ZNqmLBgheQj5a2A5MxrzRQKBgQDt1Oscq4s+So+KsFtH
5lzTkHgFrPEPA9PHquTuvepHjjlVai7XgnSvkFFgokEgdrWAK4VxXc01s62yHvHw
vhsJ283/3CCcWb221YbRooDVPxkFqor0qEYCh6hhX6L1APYDlNGlrsJRyV3hJkwB
PLVi7/s1FOcSj5p+gpVtmJ7sXwKBgQDWIX7nlojuyBndAdIcvUZDc0JIRKe3Xawm
lDvZFdPuSh5OkOpKrxrH0cXcNF2EltAJELcgsyxKZNjBfsawuVjiqqgBF2VmMe/k
juG5385E43DjSPtELpA9K9K5iztiMbN56bIp4cY2HTnd50olRhLn9p1YmQ/zX19n
15VUnsj2dQKBgQCqtdbg6Fz1JE2uDfInRLnCfgM4h68ryOJ9gjP7DcSZAgQzRBlF
RXV+Awf2ZeB7bdnPmu2YtuyyLDt0C/Q7iikcRXKywY2CzIN5NgEkfhEdf8H1KDm/
bP17mWYKJrxwQfVUEsD8vNjsHa7OClAp3yqPTpQwwMUvtHX/crnRRehk3wKBgQDK
VfZbkVQtBaniu0C2ZWeKftPoA+/TBdGQ1stCkyyiYykGJkstbQ7KN/9V16lyiytj
FYdlf8jfNzHWjRvkjA9gh8+e0GPBUHiVKSpEgCWh1KSsMB81yyYCl3FUYCsp2zrz
fQ8cIjowkidG9rGKTQ+6Xr9Jo8B9wOYe8ogp4KyWrQKBgQCPo/46FoSz+eSsWo6W
UhCFzTScrcMhz6lB5CVYqUvMZlNq8bzaNUQeqmdegHYeb0SrWbtDXVivdEXUrdDd
J+iSJlsL9z7y6qZK5rUF8NxALu+f5MeBP2Fh7oOAJMxaBYqdfXTJ/0+aCV35kcov
fYtwPYhLM923PSdHDNZRS9YXyA==
-----END PRIVATE KEY-----
";

/// APIv3 密钥，必须是 32 字节（AES-256）
const TEST_V3_KEY: &str = "0123456789abcdef0123456789abcdef";

const TEST_APPID: &str = "wx_test_appid";
const TEST_MCH_ID: &str = "1900000001";
const TEST_SERIAL_NO: &str = "TEST_SERIAL_NO";
const TEST_NOTIFY_URL: &str = "https://example.com/notify";

// ---------------------------------------------------------------------------
// 双模式测试骨架
// ---------------------------------------------------------------------------

/// sync 模式直接求值；async 模式补上 `.await`。
#[cfg(feature = "async")]
macro_rules! call {
    ($e:expr) => {
        $e.await
    };
}
#[cfg(not(feature = "async"))]
macro_rules! call {
    ($e:expr) => {
        $e
    };
}

/// 同一份测试体，在两种 feature 下各生成一个测试函数。
macro_rules! dual_test {
    (fn $name:ident() $body:block) => {
        #[cfg(not(feature = "async"))]
        #[test]
        fn $name() $body

        #[cfg(feature = "async")]
        #[tokio::test]
        async fn $name() $body
    };
}

// ---------------------------------------------------------------------------
// 极简 mock HTTP 服务（仅服务于本文件，不引入额外依赖）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl CapturedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

struct MockResponse {
    status: u16,
    body: String,
}

impl MockResponse {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_string(),
        }
    }
}

struct Mock {
    base_url: String,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl Mock {
    /// 启动 mock 服务；`responses` 按请求先后顺序依次返回。
    fn start(responses: Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().expect("local_addr");
        let captured: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));

        let captured_bg = Arc::clone(&captured);
        thread::spawn(move || {
            // 测试进程结束即终止；每个连接处理一个请求后主动关闭。
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => handle_connection(&stream, &captured_bg, &responses),
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            captured,
        }
    }

    fn requests(&self) -> Vec<CapturedRequest> {
        self.captured.lock().expect("lock captured").clone()
    }
}

fn handle_connection(
    stream: &TcpStream,
    captured: &Arc<Mutex<Vec<CapturedRequest>>>,
    responses: &Arc<Mutex<VecDeque<MockResponse>>>,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let value = v.trim().to_string();
            if key == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((key, value));
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }

    captured
        .lock()
        .expect("lock captured")
        .push(CapturedRequest {
            method,
            path,
            headers,
            body: String::from_utf8_lossy(&body).to_string(),
        });

    let response = responses
        .lock()
        .expect("lock responses")
        .pop_front()
        .unwrap_or(MockResponse::json(500, "{}"));

    let reason = match response.status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status,
        reason,
        response.body.len(),
        response.body
    );
    let mut stream = stream;
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.flush();
}

/// 构造一个指向本地 mock 服务的客户端。
/// 走公开的 `with_base_url`，不依赖字段可见性。
fn client_for(base_url: &str) -> WechatPay {
    WechatPay::new(
        TEST_APPID,
        TEST_MCH_ID,
        TEST_PRIVATE_KEY,
        TEST_SERIAL_NO,
        TEST_V3_KEY,
        TEST_NOTIFY_URL,
    )
    .with_base_url(base_url)
}

// ---------------------------------------------------------------------------
// 签名辅助：用于独立验证 SDK 产出的签名，而不是复用它的实现
// ---------------------------------------------------------------------------

fn private_key() -> rsa::RsaPrivateKey {
    use rsa::pkcs8::DecodePrivateKey;
    rsa::RsaPrivateKey::from_pkcs8_pem(TEST_PRIVATE_KEY).expect("parse test private key")
}

fn public_key_pem() -> String {
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    private_key()
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .expect("encode public key")
}

fn sign_rsa(message: &str) -> String {
    use rsa::Pkcs1v15Sign;
    use rsa::sha2::{Digest, Sha256};
    let hashed = Sha256::new().chain_update(message).finalize();
    let signature = private_key()
        .sign(Pkcs1v15Sign::new::<Sha256>(), &hashed)
        .expect("sign");
    util::base64_encode(signature)
}

/// 断言 `signature`（base64）是 `message` 的合法 RSA-SHA256 签名。
fn assert_valid_signature(message: &str, signature_b64: &str) {
    use rsa::Pkcs1v15Sign;
    use rsa::sha2::{Digest, Sha256};
    let hashed = Sha256::new().chain_update(message).finalize();
    let signature = util::base64_decode(signature_b64).expect("base64 decode signature");
    private_key()
        .to_public_key()
        .verify(Pkcs1v15Sign::new::<Sha256>(), &hashed, signature.as_slice())
        .expect("signature must verify against the public key");
}

/// 从 `mchid="xxx"` 形式的 Authorization 头中取出 `key` 对应的值。
fn auth_field(auth: &str, key: &str) -> String {
    let needle = format!("{key}=\"");
    let start = auth.find(&needle).expect("field present") + needle.len();
    let rest = &auth[start..];
    let end = rest.find('"').expect("closing quote");
    rest[..end].to_string()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

dual_test! {
    fn jsapi_pay_sends_signed_request_and_parses_success() {
        let prepay_id = "wx26112221580621e9b8f7b2f1d0a4c3e5";
        let mock = Mock::start(vec![MockResponse::json(
            200,
            &format!(r#"{{"prepay_id":"{prepay_id}"}}"#),
        )]);
        let wechat_pay = client_for(&mock.base_url);

        let response = call!(wechat_pay.jsapi_pay(JsapiParams::new(
            "测试商品",
            "ORDER_0001",
            1.into(),
            "oUpF8uMuAJO_M2pxb1Q9zNjWeS6o".into(),
        )))
        .expect("200 响应必须解析成功");

        assert_eq!(response.prepay_id.as_deref(), Some(prepay_id));

        // 必须自带可直接交给 wx.requestPayment 的签名数据
        let sign_data = response.sign_data.expect("sign_data");
        assert_eq!(sign_data.sign_type, "RSA");
        assert_eq!(sign_data.package, format!("prepay_id={prepay_id}"));
        assert_eq!(sign_data.app_id, TEST_APPID);
        assert!(!sign_data.pay_sign.is_empty());
        assert!(!sign_data.nonce_str.is_empty());
        assert!(!sign_data.timestamp.is_empty());

        let requests = mock.requests();
        assert_eq!(requests.len(), 1, "应当只发出一次请求");
        let request = &requests[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v3/pay/transactions/jsapi");

        // 请求体必须由 SDK 注入 appid/mchid/notify_url，且商户号来自配置而非入参
        let sent: serde_json::Value =
            serde_json::from_str(&request.body).expect("request body is json");
        assert_eq!(sent["appid"], TEST_APPID);
        assert_eq!(sent["mchid"], TEST_MCH_ID);
        assert_eq!(sent["notify_url"], TEST_NOTIFY_URL);
        assert_eq!(sent["out_trade_no"], "ORDER_0001");
        assert_eq!(sent["description"], "测试商品");
        assert_eq!(sent["amount"]["total"], 1);
        assert_eq!(sent["payer"]["openid"], "oUpF8uMuAJO_M2pxb1Q9zNjWeS6o");

        // Authorization 头格式（微信协议要求）
        let auth = request.header("authorization").expect("authorization header");
        assert!(
            auth.starts_with("WECHATPAY2-SHA256-RSA2048 "),
            "unexpected auth scheme: {auth}"
        );
        assert_eq!(auth_field(auth, "mchid"), TEST_MCH_ID);
        assert_eq!(auth_field(auth, "serial_no"), TEST_SERIAL_NO);

        // 独立重建签名串并验签：method\nurl\ntimestamp\nnonce\nbody\n
        let message = format!(
            "POST\n/v3/pay/transactions/jsapi\n{}\n{}\n{}\n",
            auth_field(auth, "timestamp"),
            auth_field(auth, "nonce_str"),
            request.body,
        );
        assert_valid_signature(&message, &auth_field(auth, "signature"));
    }
}
dual_test! {
    fn jsapi_pay_returns_err_on_api_error() {
        // 微信 v3 的真实错误体；HTTP 状态码为 400。
        // 回归点：修复前这段会返回 Ok(JsapiResponse { code: Some("PARAM_ERROR"), prepay_id: None })，
        // 把下单失败伪装成成功，并丢掉微信的错误码与 detail。
        let mock = Mock::start(vec![MockResponse::json(
            400,
            r#"{"code":"PARAM_ERROR","message":"参数错误","detail":{"field":"/payer/openid","reason":"不是有效的openid"}}"#,
        )]);
        let wechat_pay = client_for(&mock.base_url);

        let result = call!(wechat_pay.jsapi_pay(JsapiParams::new(
            "测试商品",
            "ORDER_0002",
            1.into(),
            "bad_openid".into(),
        )));

        match result {
            Err(PayError::ApiError { status, response }) => {
                assert_eq!(status, 400);
                assert_eq!(response.code.as_deref(), Some("PARAM_ERROR"));
                assert_eq!(response.message.as_deref(), Some("参数错误"));
                // detail 必须原样保留，否则线上无法定位到具体字段
                assert_eq!(
                    response.detail.as_ref().expect("detail")["field"],
                    "/payer/openid"
                );
            }
            other => panic!("下单失败必须返回 Err(PayError::ApiError)，实际得到 {other:?}"),
        }
    }
}
dual_test! {
    fn refunds_returns_err_on_api_error() {
        let mock = Mock::start(vec![MockResponse::json(
            400,
            r#"{"code":"NOT_ENOUGH","message":"余额不足"}"#,
        )]);
        let wechat_pay = client_for(&mock.base_url);

        let result = call!(wechat_pay.refunds(RefundsParams::new(
            "R_0001",
            1,
            1,
            None,
            Some("ORDER_0003"),
        )));

        match result {
            Err(PayError::ApiError { status, response }) => {
                assert_eq!(status, 400);
                assert_eq!(response.code.as_deref(), Some("NOT_ENOUGH"));
                assert_eq!(response.message.as_deref(), Some("余额不足"));
            }
            other => panic!("退款失败必须返回 Err(PayError::ApiError)，实际得到 {other:?}"),
        }
    }
}
dual_test! {
    fn decrypt_paydata_roundtrip() {
        use aes_gcm::aead::{Aead, KeyInit, Payload};
        use aes_gcm::{Aes256Gcm, Nonce};

        let plaintext = r#"{"mchid":"1900000001","appid":"wx_test_appid","out_trade_no":"ORDER_0004","transaction_id":"4200001234202609110000000000","trade_type":"JSAPI","trade_state":"SUCCESS","trade_state_desc":"支付成功","bank_type":"OTHERS","attach":"","success_time":"2026-09-11T12:00:00+08:00","payer":{"openid":"oUpF8uMuAJO_M2pxb1Q9zNjWeS6o"},"amount":{"total":1}}"#;
        let nonce_bytes: [u8; 12] = *b"abcdefghijkl";
        let associated_data = "transaction";

        let cipher = Aes256Gcm::new_from_slice(TEST_V3_KEY.as_bytes()).expect("cipher");
        let encrypted = cipher
            .encrypt(
                &Nonce::from(nonce_bytes),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: associated_data.as_bytes(),
                },
            )
            .expect("encrypt");
        let ciphertext = util::base64_encode(&encrypted);

        // 此测试不发送任何请求，base_url 无关紧要
        let wechat_pay = client_for("http://127.0.0.1:1");
        // 注意：decrypt_paydata / verify_signature 不是 maybe_async 方法，两种模式下都是同步的
        let data = wechat_pay
            .decrypt_paydata(ciphertext.as_str(), "abcdefghijkl", associated_data)
            .expect("回调解密必须成功");

        assert_eq!(data.mchid, TEST_MCH_ID);
        assert_eq!(data.appid, TEST_APPID);
        assert_eq!(data.out_trade_no, "ORDER_0004");
        assert_eq!(data.trade_state, "SUCCESS");
        assert_eq!(data.amount.total, 1);
    }
}
dual_test! {
    fn verify_signature_accepts_valid_and_rejects_tampered() {
        let timestamp = "1705066785";
        let nonce = "Jh9oPZelCJIQeQ47kz4stzvDKpLEUhCX";
        let body = r#"{"id":"evt_0001","event_type":"TRANSACTION.SUCCESS"}"#;
        let signature = sign_rsa(&format!("{timestamp}\n{nonce}\n{body}\n"));
        let public_key = public_key_pem();

        let wechat_pay = client_for("http://127.0.0.1:1");

        // 合法签名必须通过
        wechat_pay
            .verify_signature(
                public_key.as_str(),
                timestamp,
                nonce,
                signature.as_str(),
                body,
            )
            .expect("合法回调签名必须验签通过");

        // 篡改 body 后必须失败（防止验签被写成恒 Ok 的空壳）
        let tampered = r#"{"id":"evt_0001","event_type":"TRANSACTION.FAIL"}"#;
        let result = wechat_pay.verify_signature(
            public_key.as_str(),
            timestamp,
            nonce,
            signature.as_str(),
            tampered,
        );
        assert!(result.is_err(), "被篡改的回调必须验签失败");

        // 非法签名也必须失败
        let result = wechat_pay.verify_signature(
            public_key.as_str(),
            timestamp,
            nonce,
            "bm90LWEtc2lnbmF0dXJl",
            body,
        );
        assert!(result.is_err(), "非法签名必须验签失败");
    }
}
dual_test! {
    fn non_json_error_body_is_preserved() {
        // 网关故障时可能返回 HTML 而不是微信的错误结构；
        // 这种情况必须保留原文，否则唯一的排查线索就丢了。
        let mock = Mock::start(vec![MockResponse::json(502, "<html>Bad Gateway</html>")]);
        let wechat_pay = client_for(&mock.base_url);

        let result = call!(wechat_pay.jsapi_pay(JsapiParams::new(
            "测试商品",
            "ORDER_0005",
            1.into(),
            "openid".into(),
        )));

        match result {
            Err(err @ PayError::ApiError { .. }) => {
                // Display 必须带出状态码与原始响应体，方便直接看日志定位
                let text = err.to_string();
                assert!(text.contains("502"), "缺状态码: {text}");
                assert!(text.contains("Bad Gateway"), "非 JSON 响应体必须原样保留: {text}");
            }
            other => panic!("必须返回 Err(PayError::ApiError)，实际得到 {other:?}"),
        }
    }
}
dual_test! {
    fn non_wechat_json_error_body_is_preserved() {
        // 回归：ErrorResponse 的字段全是 Option 且未加 deny_unknown_fields，
        // 所以任何 JSON 对象都能解析成功。若只判断「能否解析成 JSON」，
        // WAF / 反向代理的错误体（{"status":403,...} / {"errcode":...}）会被
        // 解析成一个三个字段全为 None 的 ErrorResponse，唯一线索就丢了。
        let mock = Mock::start(vec![MockResponse::json(
            502,
            r#"{"errcode":40001,"errmsg":"invalid credential from upstream"}"#,
        )]);
        let wechat_pay = client_for(&mock.base_url);

        let result = call!(wechat_pay.jsapi_pay(JsapiParams::new(
            "测试商品",
            "ORDER_0006",
            1.into(),
            "openid".into(),
        )));

        match result {
            Err(err @ PayError::ApiError { .. }) => {
                let text = err.to_string();
                assert!(
                    text.contains("invalid credential from upstream"),
                    "非微信形状的 JSON 错误体也必须保留原文: {text}"
                );
            }
            other => panic!("必须返回 Err(PayError::ApiError)，实际得到 {other:?}"),
        }
    }
}
dual_test! {
    fn oversized_error_body_is_truncated() {
        // 网关可能返回整页 HTML；不能让整个 body 复制进错误消息并刷爆日志。
        let huge = format!("<html>{}</html>", "x".repeat(20_000));
        let mock = Mock::start(vec![MockResponse::json(504, &huge)]);
        let wechat_pay = client_for(&mock.base_url);

        let result = call!(wechat_pay.jsapi_pay(JsapiParams::new(
            "测试商品",
            "ORDER_0007",
            1.into(),
            "openid".into(),
        )));

        match result {
            Err(err @ PayError::ApiError { .. }) => {
                let text = err.to_string();
                assert!(text.contains("truncated"), "超长响应体必须标注截断: {text}");
                assert!(text.contains("bytes total"), "应标注原始字节数: {text}");
                assert!(
                    text.len() < 10_000,
                    "错误消息不应保留整个响应体，实际长度 {}",
                    text.len()
                );
            }
            other => panic!("必须返回 Err(PayError::ApiError)，实际得到 {other:?}"),
        }
    }
}
dual_test! {
    fn empty_error_body_is_marked() {
        let mock = Mock::start(vec![MockResponse::json(502, "")]);
        let wechat_pay = client_for(&mock.base_url);

        let result = call!(wechat_pay.jsapi_pay(JsapiParams::new(
            "测试商品",
            "ORDER_0008",
            1.into(),
            "openid".into(),
        )));

        match result {
            Err(err @ PayError::ApiError { .. }) => {
                assert!(
                    err.to_string().contains("<empty body>"),
                    "空响应体应有显式标记而不是空 message: {err}"
                );
            }
            other => panic!("必须返回 Err(PayError::ApiError)，实际得到 {other:?}"),
        }
    }
}
dual_test! {
    fn debug_output_redacts_secrets() {
        // 用哨兵值而不是真实 PEM：Debug 会把换行转义成 \n，
        // 多行字符串包含判断会失真。
        let wechat_pay = WechatPay::new(
            "appid-x",
            "mch-x",
            "SENTINEL_PRIVATE_KEY",
            "serial-x",
            "SENTINEL_V3_KEY",
            "https://example.com/notify",
        );
        let rendered = format!("{wechat_pay:?}");

        assert!(
            !rendered.contains("SENTINEL_PRIVATE_KEY"),
            "Debug 泄露商户私钥: {rendered}"
        );
        assert!(
            !rendered.contains("SENTINEL_V3_KEY"),
            "Debug 泄露 APIv3 密钥: {rendered}"
        );
        // 非机密字段仍应保留，否则排障时无从判断用的是哪个商户号
        assert!(rendered.contains("mch-x"), "非机密字段应保留: {rendered}");
        assert!(rendered.contains("redacted"), "应显式标注脱敏: {rendered}");
    }
}
dual_test! {
    fn with_base_url_overrides_default_gateway() {
        // 默认必须是官方网关；with_base_url 只用于把请求指向 mock / 沙箱。
        let default = WechatPay::new("a", "b", "c", "d", "e", "f");
        assert!(
            format!("{default:?}").contains("https://api.mch.weixin.qq.com"),
            "默认网关应指向微信官方地址"
        );

        let overridden = default.with_base_url("http://127.0.0.1:1234");
        assert!(format!("{overridden:?}").contains("http://127.0.0.1:1234"));
    }
}
