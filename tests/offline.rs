//! 离线集成测试：用本地 mock HTTP 服务替代微信支付网关。
//!
//! 这些测试**不联网、不需要任何真实商户凭证**，可在 CI 中运行。
//! 之所以可以这样测，是因为 `with_base_url` 允许把实例指向本地 mock 服务（字段本身是私有的）。
//!
//! 同一份测试体在两种 feature 下都会编译并运行：
//! - 默认（sync）：`reqwest::blocking`，普通 `#[test]`
//! - `--features async`：`reqwest` + `#[tokio::test]`

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use wechat_pay_rust_sdk::cert::{PlatformKeys, REFRESH_INTERVAL_SECS};
use wechat_pay_rust_sdk::error::{ErrorKind, PayError};
use wechat_pay_rust_sdk::model::{
    AmountInfo, AppParams, Currency, GoodsDetail, JsapiParams, MicroParams, NativeParams,
    OrderDetail, PayerInfo, RefundsParams, SceneInfo, SettleInfo,
};
use wechat_pay_rust_sdk::notify::NotifyHeaders;
use wechat_pay_rust_sdk::pay::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT, HttpTimeouts, PayNotifyTrait, WechatPay,
};
use wechat_pay_rust_sdk::request::HttpMethod;
use wechat_pay_rust_sdk::retry::RetryPolicy;
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
    /// 已接受的 TCP 连接数。客户端复用连接时它不会增长。
    connections: Arc<AtomicUsize>,
}

impl Mock {
    /// 启动 mock 服务；`responses` 按请求先后顺序依次返回。
    ///
    /// 支持 keep-alive：一条连接上可以连续跑多个请求，用于验证客户端是否复用连接。
    fn start(responses: Vec<MockResponse>) -> Self {
        let (base_url, captured, connections) = spawn_mock(responses);
        Self {
            base_url,
            captured,
            connections,
        }
    }

    fn requests(&self) -> Vec<CapturedRequest> {
        self.captured.lock().expect("lock captured").clone()
    }

    /// 已接受的连接数。
    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// 启动 mock 的公共部分，返回 `(base_url, 已捕获请求, 连接计数)`。
fn spawn_mock(
    responses: Vec<MockResponse>,
) -> (String, Arc<Mutex<Vec<CapturedRequest>>>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let addr = listener.local_addr().expect("local_addr");
    let captured: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let connections = Arc::new(AtomicUsize::new(0));
    let responses = Arc::new(Mutex::new(VecDeque::from(responses)));

    let captured_bg = Arc::clone(&captured);
    let connections_bg = Arc::clone(&connections);
    thread::spawn(move || {
        // 测试进程结束即终止。
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            connections_bg.fetch_add(1, Ordering::SeqCst);
            let captured = Arc::clone(&captured_bg);
            let responses = Arc::clone(&responses);
            // 每条连接一个线程：这样一条连接上可以连续处理多个请求（keep-alive）。
            thread::spawn(move || handle_connection(stream, &captured, &responses));
        }
    });

    (format!("http://{addr}"), captured, connections)
}

/// 启动一个「只接受连接、不回任何响应」的 mock，用于验证超时。
///
/// 连接被持有不关闭，客户端只能等到自己的超时。
fn hanging_mock_base_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind hanging mock");
    let addr = listener.local_addr().expect("local_addr");
    thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            held.push(stream); // 持有连接，不写任何字节
        }
    });
    format!("http://{addr}")
}

/// 在一条连接上循环处理请求（keep-alive），直到对端关闭。
fn handle_connection(
    stream: TcpStream,
    captured: &Arc<Mutex<Vec<CapturedRequest>>>,
    responses: &Arc<Mutex<VecDeque<MockResponse>>>,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut writer = stream;

    while let Some(request) = read_request(&mut reader) {
        captured.lock().expect("lock captured").push(request);

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
        // 刻意不发 `Connection: close`：客户端会把连接放回池里复用，
        // 这正是 connections() 计数能验证的东西。
        let out = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            response.status,
            reason,
            response.body.len(),
            response.body
        );
        if writer.write_all(out.as_bytes()).is_err() || writer.flush().is_err() {
            return;
        }
    }
}

/// 读一个完整的 HTTP 请求；对端关闭或读到空行时返回 `None`。
fn read_request(reader: &mut BufReader<TcpStream>) -> Option<CapturedRequest> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 || request_line.trim().is_empty() {
        return None;
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
    if content_length > 0 && reader.read_exact(&mut body).is_err() {
        return None;
    }

    Some(CapturedRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
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
        // 关掉重试：本用例只关心「单个响应体有没有被完整保留」。502 按官方口径是可重试的，
        // 开着重试会让最终错误变成第 N 次尝试的响应体（重试行为另有专门用例覆盖）。
        let wechat_pay = client_for(&mock.base_url).with_retry(RetryPolicy::disabled());

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
        // 同上：只验证「非微信形状的错误体被保留原文」，与重试无关。
        let wechat_pay = client_for(&mock.base_url).with_retry(RetryPolicy::disabled());

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
        // 关掉重试：本用例只验证截断行为。（504 目前在分类里属「结果未知」，
        // 对写接口本就不会重试 —— 但别让这个用例依赖那个间接结论。）
        let wechat_pay = client_for(&mock.base_url).with_retry(RetryPolicy::disabled());

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
        // 同上：只验证「空响应体有显式标记」，与重试无关。
        let wechat_pay = client_for(&mock.base_url).with_retry(RetryPolicy::disabled());

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

// ---------------------------------------------------------------------------
// 平台证书轮换（P1-3）与回调防护（P1-4）
// ---------------------------------------------------------------------------

/// 测试用自签名平台证书（PEM）。
///
/// 微信 `GET /v3/certificates` 的 `encrypt_certificate.ciphertext` 解密后得到的是
/// **PEM 证书**（不是 DER），`util::x509_to_pem` 也按 PEM 解析 —— 所以这里直接放 PEM。
///
/// 由 `TEST_PRIVATE_KEY` 签发，证书里的公钥与测试私钥配对，可直接验证 `sign_rsa`。
/// ⚠ 一次性测试证书，不对应任何真实平台证书。
const TEST_PLATFORM_CERT_PEM: &str = "\
-----BEGIN CERTIFICATE-----
MIIDJTCCAg2gAwIBAgIUZANuXDsu1bA4uXs2PNYdtb8+1LowDQYJKoZIhvcNAQEL
BQAwIjEgMB4GA1UEAwwXd2VjaGF0cGF5LXRlc3QtcGxhdGZvcm0wHhcNMjYwOTEx
MDgwOTQyWhcNMzYwOTA4MDgwOTQyWjAiMSAwHgYDVQQDDBd3ZWNoYXRwYXktdGVz
dC1wbGF0Zm9ybTCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAMbvGuI4
yr8IO5qECuACMmxAZPyAEqkdZeuUJsWQkPrmgu5RNXKW4bVtEvKFbOqHtY65CohO
ZRX6zUM83yW5qI1tV+TsLt7RKW9y4R/ILkTgT/ED3uKkOeO1XMxdNZ5EyJvOptPe
vV2SDOTTokawqCslCrSapzRR9QtRUn5v6KgTjCNgDtRs9CaiSP8kCTmKjLd8If5C
d8Oy4sy9jQZYK1dz+65Z+avLXw8/V85X/+c3z5MZG1MWHH4U0sK8qaZSrmkgaUr8
zLm2R/xHJwa/w7Yoowl+J44klWdo+HGSCaCqXWo2AxUcZp+iqN6EU9M81Zh3YqKj
ZG4J/5jH9sP9UWsCAwEAAaNTMFEwHQYDVR0OBBYEFG36siVExw68hXtvpY1MPj4x
fkcFMB8GA1UdIwQYMBaAFG36siVExw68hXtvpY1MPj4xfkcFMA8GA1UdEwEB/wQF
MAMBAf8wDQYJKoZIhvcNAQELBQADggEBAJ9//mpwWZ1hQdDO4RDe1LdyD7JDUCUN
+c69yyvRJlwXKAEUdTiRO2i99bR18bXorkFtdKA2NcruQRkeoNsJRmTKhkV/H4hT
mlvsIx4arU63etNw2674lbVl2KJMKf87i6+9grKTrzKIqsVAyRYnvESF0ApRmt24
FPGMiEwdWiFmnyBE3oLazze6Ro4L1GbR9nU6za5OLF80EXkvIjdiUrstNxEGIIku
8GHvNejrW1l90THjfbsGQw+QOldwc+1ZG5vPsdOhudpINADDFR5SK7ME4pj2SvB9
ubZoRgaS3Vxyvr5CRzkZcoNsfQedoxGkzOkrUhIfcMWJ0AyrDcUHkNU=
-----END CERTIFICATE-----
";

/// 用 `TEST_V3_KEY` 加密一段明文，构造微信证书接口里 `encrypt_certificate` 的形状。
fn encrypt_certificate(plaintext: &[u8], nonce: &str) -> String {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};

    let cipher = Aes256Gcm::new_from_slice(TEST_V3_KEY.as_bytes()).expect("cipher");
    let nonce_bytes: [u8; 12] = nonce.as_bytes().try_into().expect("nonce 必须是 12 字节");
    let encrypted = cipher
        .encrypt(
            &Nonce::from(nonce_bytes),
            Payload {
                msg: plaintext,
                aad: b"certificate",
            },
        )
        .expect("encrypt");
    util::base64_encode(&encrypted)
}

/// 构造 `GET /v3/certificates` 的响应体；`entries` 为 `(serial_no, nonce)`。
fn certificates_response(entries: &[(&str, &str)]) -> String {
    let cert_pem = TEST_PLATFORM_CERT_PEM.as_bytes();
    let items: Vec<String> = entries
        .iter()
        .map(|(serial, nonce)| {
            format!(
                r#"{{"serial_no":"{serial}","effective_time":"2026-01-01T00:00:00+08:00","expire_time":"2031-01-01T00:00:00+08:00","encrypt_certificate":{{"algorithm":"AEAD_AES_256_GCM","nonce":"{nonce}","associated_data":"certificate","ciphertext":"{}"}}}}"#,
                encrypt_certificate(cert_pem, nonce)
            )
        })
        .collect();
    format!(r#"{{"data":[{}]}}"#, items.join(","))
}

dual_test! {
    fn fetch_platform_keys_indexes_both_certificates_during_rotation() {
        // 轮换期微信会同时下发新旧两张且都在有效期内 —— 两张都要索引，
        // 否则灰度期间会有一半回调验签失败。
        let body = certificates_response(&[("SERIAL_OLD", "nonce_old_01"), ("SERIAL_NEW", "nonce_new_01")]);
        let mock = Mock::start(vec![MockResponse::json(200, &body)]);
        let wechat_pay = client_for(&mock.base_url);

        let keys = call!(wechat_pay.fetch_platform_keys()).expect("拉取并解密平台证书");

        assert_eq!(keys.len(), 2, "两张证书都要索引，实际: {:?}", keys.serials());
        assert_eq!(keys.serials(), vec!["SERIAL_NEW", "SERIAL_OLD"]);
        for serial in ["SERIAL_OLD", "SERIAL_NEW"] {
            let pem = keys.get(serial).expect("serial 应在索引里");
            assert!(pem.contains("BEGIN PUBLIC KEY"), "解密结果应是 PEM 公钥: {pem}");
        }

        assert_eq!(mock.requests()[0].path, "/v3/certificates");
        assert_eq!(mock.requests()[0].method, "GET");
    }
}
dual_test! {
    fn platform_keys_refresh_window() {
        let mut keys = PlatformKeys::new();
        assert!(keys.needs_refresh(0), "从未拉取过就该刷新");
        keys.mark_refreshed(1_000);
        assert!(!keys.needs_refresh(1_000 + 3600), "一小时后不必刷新");
        assert!(
            keys.needs_refresh(1_000 + REFRESH_INTERVAL_SECS),
            "达到刷新间隔就该刷新"
        );
        assert_eq!(keys.fetched_at(), Some(1_000));
    }
}
dual_test! {
    fn verify_notify_selects_key_by_serial_and_rejects_replay() {
        let body_json = certificates_response(&[("SERIAL_A", "nonce_cert_1")]);
        let mock = Mock::start(vec![MockResponse::json(200, &body_json)]);
        let wechat_pay = client_for(&mock.base_url);
        let keys = call!(wechat_pay.fetch_platform_keys()).expect("拉取平台证书");

        let now: i64 = 1_780_000_000;
        let callback_body = r#"{"id":"evt_1","event_type":"TRANSACTION.SUCCESS"}"#;
        let timestamp = now.to_string();
        let nonce = "notify_nonce_1";
        let signature = sign_rsa(&format!("{timestamp}\n{nonce}\n{callback_body}\n"));
        let headers = NotifyHeaders::new("SERIAL_A", &timestamp, nonce, &signature);

        // 1) 新鲜 + serial 命中 -> 通过
        keys.verify_notify_at(&headers, callback_body, now)
            .expect("合法回调必须通过");

        // 2) 时间戳超窗 -> 判定重放。缺了这一步，抓到一次合法回调就能无限重放。
        let err = keys
            .verify_notify_at(&headers, callback_body, now + 301)
            .expect_err("超窗必须被拒绝");
        assert!(matches!(err, PayError::StaleNotify(_)), "应为 StaleNotify，实际 {err:?}");

        // 窗口边界：恰好 300s 仍应接受
        keys.verify_notify_at(&headers, callback_body, now + 300)
            .expect("恰好在窗口边界内应接受");

        // 3) serial 不在索引里 -> 提示重新拉取，而不是拿别的密钥去试
        let unknown = NotifyHeaders::new("SERIAL_UNKNOWN", &timestamp, nonce, &signature);
        let err = keys
            .verify_notify_at(&unknown, callback_body, now)
            .expect_err("未知 serial 必须失败");
        assert!(
            matches!(err, PayError::UnknownPlatformSerial(_)),
            "应为 UnknownPlatformSerial（提示重新拉取），实际 {err:?}"
        );

        // 4) 篡改 body -> 验签失败
        let tampered = r#"{"id":"evt_1","event_type":"TRANSACTION.FAIL"}"#;
        let err = keys
            .verify_notify_at(&headers, tampered, now)
            .expect_err("被篡改的回调必须失败");
        assert!(matches!(err, PayError::VerifyError(_)), "应为 VerifyError，实际 {err:?}");

        // 5) 时间戳不是整数 -> 拒绝，且不能 panic
        let bad = NotifyHeaders::new("SERIAL_A", "not-a-number", nonce, &signature);
        let err = keys
            .verify_notify_at(&bad, callback_body, now)
            .expect_err("非法时间戳必须被拒绝");
        assert!(matches!(err, PayError::StaleNotify(_)), "应为 StaleNotify，实际 {err:?}");
    }
}
dual_test! {
    fn notify_headers_from_pairs_is_case_insensitive_and_requires_all_four() {
        let headers = NotifyHeaders::from_pairs([
            ("Wechatpay-Serial", "S1"),
            ("wechatpay-timestamp", "123"),
            ("Wechatpay_Nonce", "n1"),
            ("WECHATPAY-SIGNATURE", "sig"),
        ])
        .expect("四个头齐全");
        assert_eq!(headers.serial, "S1");
        assert_eq!(headers.timestamp, "123");
        assert_eq!(headers.nonce, "n1");
        assert_eq!(headers.signature, "sig");

        // 缺头必须报错，而不是拿空串去验签（那会静默失败）
        let err = NotifyHeaders::from_pairs([("Wechatpay-Serial", "S1")])
            .expect_err("缺头应报错");
        assert!(matches!(err, PayError::VerifyError(_)), "实际 {err:?}");
    }
}
dual_test! {
    fn query_order_signs_the_url_including_query_string() {
        // 微信签名串的第二行是 path + "?" + query。漏掉查询串会直接 401，
        // 所以这里用捕获到的 Authorization 头独立重建签名串并验签。
        let mock = Mock::start(vec![MockResponse::json(
            200,
            r#"{"appid":"wx_test_appid","mchid":"1900000001","out_trade_no":"ORDER_0009","trade_state":"NOTPAY","trade_state_desc":"订单未支付"}"#,
        )]);
        let wechat_pay = client_for(&mock.base_url);

        let response = call!(wechat_pay.query_order("ORDER_0009")).expect("200 应解析成功");
        assert_eq!(response.out_trade_no, "ORDER_0009");
        assert_eq!(response.trade_state, "NOTPAY");
        assert_eq!(response.transaction_id, None, "未支付时不下发微信订单号");
        assert!(response.amount.is_none(), "本响应未带 amount");

        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.path,
            "/v3/pay/transactions/out-trade-no/ORDER_0009?mchid=1900000001",
            "mchid 是唯一的查询参数，且必须出现在 URL 里"
        );

        // 关键断言：签名覆盖的是**含查询串**的 URL
        let auth = request.header("authorization").expect("authorization header");
        let message = format!(
            "GET\n{}\n{}\n{}\n{}\n",
            request.path,
            auth_field(auth, "timestamp"),
            auth_field(auth, "nonce_str"),
            request.body,
        );
        assert_valid_signature(&message, &auth_field(auth, "signature"));
    }
}
dual_test! {
    fn close_order_sends_only_mchid_and_accepts_204() {
        // 关单成功返回 204 No Content 且**无响应体**。
        // 若走 request_json 会因空 body 解析失败 —— 所以它用 request_no_content。
        let mock = Mock::start(vec![MockResponse::json(204, "")]);
        let wechat_pay = client_for(&mock.base_url);

        call!(wechat_pay.close_order("ORDER_0010")).expect("204 必须被当作成功");

        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.path,
            "/v3/pay/transactions/out-trade-no/ORDER_0010/close"
        );

        // 请求体只能有 mchid。这正是把底层 request() 抽出来的原因：
        // pay() 会无条件注入 appid / mchid / notify_url，而关单不接受后两者。
        let sent: serde_json::Value = serde_json::from_str(&request.body).expect("json body");
        let object = sent.as_object().expect("json object");
        assert_eq!(
            object.len(),
            1,
            "关单请求体只应有 mchid 一个字段，实际: {sent}"
        );
        assert_eq!(sent["mchid"], TEST_MCH_ID);
    }
}
dual_test! {
    fn query_refund_sends_no_query_params() {
        let refund = r#"{"refund_id":"50000000382019052709732678859","out_refund_no":"R_0002","transaction_id":"4200000000000000000000000000","out_trade_no":"ORDER_0011","channel":"ORIGINAL","user_received_account":"支付用户零钱","create_time":"2026-09-11T12:00:00+08:00","status":"SUCCESS","funds_account":"UNSETTLED","amount":{"total":1,"refund":1,"payer_total":1,"payer_refund":1,"settlement_refund":1,"settlement_total":1,"discount_refund":0,"currency":"CNY"}}"#;
        let mock = Mock::start(vec![MockResponse::json(200, refund)]);
        let wechat_pay = client_for(&mock.base_url);

        let response = call!(wechat_pay.query_refund("R_0002")).expect("200 应解析成功");
        assert_eq!(response.status, "SUCCESS");
        assert_eq!(response.amount.refund, 1);
        assert_eq!(response.amount.total, 1);

        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        // 该端点没有任何查询参数：mchid 由 Authorization 头隐含
        assert_eq!(requests[0].path, "/v3/refund/domestic/refunds/R_0002");
    }
}
dual_test! {
    fn query_order_surfaces_order_not_exist_as_api_error() {
        // 订单不存在时微信返回 404 ORDER_NOT_EXIST —— 这是业务结果，不是传输故障，
        // 调用方需要能从 response.code 里区分出来。
        let mock = Mock::start(vec![MockResponse::json(
            404,
            r#"{"code":"ORDER_NOT_EXIST","message":"订单不存在"}"#,
        )]);
        let wechat_pay = client_for(&mock.base_url);

        let result = call!(wechat_pay.query_order("ORDER_XXXX"));

        match result {
            Err(PayError::ApiError { status, response }) => {
                assert_eq!(status, 404);
                assert_eq!(response.code.as_deref(), Some("ORDER_NOT_EXIST"));
            }
            other => panic!("应返回 Err(PayError::ApiError)，实际得到 {other:?}"),
        }
    }
}

/// 编译期断言：这些公开类型必须是 `Send + Sync`。
///
/// 原先由 **22 处 `unsafe impl Send/Sync`** 手工保证，现已全部删除，改为依赖自动派生
/// （`src/lib.rs` 同时加了 `#![forbid(unsafe_code)]` 防止再引入）。这个测试就是替代品：
/// 一旦有人给这些类型加上 `Rc`、裸指针之类非 Send/Sync 的字段，**本测试会编译失败**，
/// 而不是等到线上把 `WechatPay` 放进 web 框架共享状态时才炸。
///
/// 断言是编译期的，所以函数体故意留空。
#[test]
fn public_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    // 需要跨线程共享的核心类型
    assert_send_sync::<WechatPay>();
    assert_send_sync::<PayError>();
    assert_send_sync::<PlatformKeys>();
    assert_send_sync::<NotifyHeaders>();
    assert_send_sync::<HttpMethod>();

    // 原先被 `unsafe impl` 覆盖过的模型类型
    assert_send_sync::<Currency>();
    assert_send_sync::<AmountInfo>();
    assert_send_sync::<PayerInfo>();
    assert_send_sync::<GoodsDetail>();
    assert_send_sync::<OrderDetail>();
    assert_send_sync::<SceneInfo>();
    assert_send_sync::<SettleInfo>();
    assert_send_sync::<NativeParams>();
    assert_send_sync::<JsapiParams>();
}

dual_test! {
    fn error_envelope_with_200_status_is_err() {
        // 微信偶尔会以 HTTP 200 返回错误信封。成功响应类型字段全是 Option，
        // 能把它照单全收 —— 单靠状态码检查覆盖不到这一半。
        let mock = Mock::start(vec![MockResponse::json(
            200,
            r#"{"code":"PARAM_ERROR","message":"参数错误","detail":{"field":"/payer/openid"}}"#,
        )]);
        let wechat_pay = client_for(&mock.base_url);

        let result = call!(wechat_pay.jsapi_pay(JsapiParams::new(
            "测试商品",
            "ORDER_E1",
            1.into(),
            "openid".into(),
        )));

        match result {
            Err(PayError::ApiError { status, response }) => {
                // status 如实记录真实状态码（200），业务原因看 response.code
                assert_eq!(status, 200);
                assert_eq!(response.code.as_deref(), Some("PARAM_ERROR"));
                assert_eq!(
                    response.detail.as_ref().expect("detail")["field"],
                    "/payer/openid"
                );
            }
            other => panic!("200 + 错误信封必须返回 Err，实际得到 {other:?}"),
        }
    }
}

dual_test! {
    fn success_bodies_are_not_mistaken_for_error_envelopes() {
        // 反向断言：正常成功响应不能被误判。顺带覆盖最容易误伤的一种写法 ——
        // 顶层出现 `code` 键但值为 null（不作为信封）。
        let mock = Mock::start(vec![
            MockResponse::json(200, r#"{"prepay_id":"wx_envelope_ok"}"#),
            MockResponse::json(200, r#"{"data":[]}"#),
            MockResponse::json(
                200,
                r#"{"appid":"wx_test_appid","mchid":"1900000001","out_trade_no":"O","trade_state":"NOTPAY","trade_state_desc":"未支付","code":null}"#,
            ),
            MockResponse::json(200, r#"{"code":"","message":""}"#),
        ]);
        let wechat_pay = client_for(&mock.base_url);

        let jsapi = call!(wechat_pay.jsapi_pay(JsapiParams::new("a", "O", 1.into(), "o".into())))
            .expect("正常下单响应不应被当成错误");
        assert_eq!(jsapi.prepay_id.as_deref(), Some("wx_envelope_ok"));

        let certs = call!(wechat_pay.certificates()).expect("证书响应不应被当成错误");
        assert_eq!(certs.data.map(|list| list.len()), Some(0));

        let order = call!(wechat_pay.query_order("O")).expect("查单响应不应被当成错误");
        assert_eq!(order.trade_state, "NOTPAY");

        // code 为空串同样不算信封（微信不会这么返回，但不能因此误判）
        call!(wechat_pay.jsapi_pay(JsapiParams::new("b", "O2", 1.into(), "o".into())))
            .expect("空字符串 code 不应被当成错误信封");

        assert_eq!(mock.requests().len(), 4);
    }
}

dual_test! {
    fn request_times_out_instead_of_hanging() {
        let base_url = hanging_mock_base_url();
        let wechat_pay = client_for(&base_url).with_timeouts(HttpTimeouts {
            request: Duration::from_millis(400),
            ..HttpTimeouts::default()
        });

        let started = Instant::now();
        let err = call!(wechat_pay.jsapi_pay(JsapiParams::new(
            "测试商品",
            "ORDER_TIMEOUT",
            1.into(),
            "openid".into(),
        )))
        .expect_err("服务端不响应必须触发超时，而不是无限等待");
        let elapsed = started.elapsed();

        assert_eq!(err.kind(), ErrorKind::Network, "超时应归为网络层: {err}");
        match &err {
            PayError::RequestError(e) => assert!(e.is_timeout(), "应为超时错误: {e}"),
            other => panic!("应为 RequestError，实际 {other:?}"),
        }
        assert!(
            elapsed < Duration::from_secs(5),
            "应在超时后尽快返回，实际耗时 {elapsed:?}"
        );

        // 默认配置本身就带超时（不是无限等待）
        let defaults = WechatPay::new("a", "b", "c", "d", "e", "f").timeouts();
        assert_eq!(defaults.request, DEFAULT_REQUEST_TIMEOUT);
        assert_eq!(defaults.connect, DEFAULT_CONNECT_TIMEOUT);
    }
}

dual_test! {
    fn client_reuses_connections_across_requests() {
        let mock = Mock::start(vec![
            MockResponse::json(200, r#"{"prepay_id":"wx_1"}"#),
            MockResponse::json(200, r#"{"prepay_id":"wx_2"}"#),
            MockResponse::json(200, r#"{"prepay_id":"wx_3"}"#),
        ]);
        let wechat_pay = client_for(&mock.base_url);

        // 同一客户端连续两次请求：连接池应复用同一条 TCP 连接
        call!(wechat_pay.jsapi_pay(JsapiParams::new("a", "O1", 1.into(), "o".into()))).unwrap();
        call!(wechat_pay.jsapi_pay(JsapiParams::new("b", "O2", 1.into(), "o".into()))).unwrap();

        assert_eq!(mock.requests().len(), 2);
        assert_eq!(
            mock.connections(),
            1,
            "同一客户端应复用连接（连接池），而不是每次重新建连"
        );

        // 反证：另一个客户端有独立连接池，必然新开一条连接。
        // 这条同时证明 connections() 计数不是恒为 1（否则上面的断言没有意义）。
        let other = client_for(&mock.base_url);
        call!(other.jsapi_pay(JsapiParams::new("c", "O3", 1.into(), "o".into()))).unwrap();
        assert_eq!(mock.connections(), 2, "独立客户端应使用独立连接池");
    }
}

dual_test! {
    fn error_kind_classifies_the_three_layers() {
        // 1) 网络层：指向一个必然拒绝连接的端口
        let offline = client_for("http://127.0.0.1:1");
        let err = call!(offline.jsapi_pay(JsapiParams::new(
            "测试商品",
            "ORDER_K1",
            1.into(),
            "openid".into(),
        )))
        .expect_err("连接被拒应报错");
        assert_eq!(err.kind(), ErrorKind::Network, "实际: {err}");

        // 2) 业务层：微信以非 2xx 拒绝
        let mock = Mock::start(vec![MockResponse::json(
            404,
            r#"{"code":"ORDER_NOT_EXIST","message":"订单不存在"}"#,
        )]);
        let wechat_pay = client_for(&mock.base_url);
        let err = call!(wechat_pay.query_order("ORDER_K2")).expect_err("404 应报错");
        assert_eq!(err.kind(), ErrorKind::Api, "实际: {err}");
        // 订单不存在是**正常业务结果**，调用方要能拿到 code 做分支
        match &err {
            PayError::ApiError { response, .. } => {
                assert_eq!(response.code.as_deref(), Some("ORDER_NOT_EXIST"));
            }
            other => panic!("应为 ApiError，实际 {other:?}"),
        }

        // 3) 本地层：v3_key 不对导致解密失败
        let wrong_key = WechatPay::new(
            TEST_APPID,
            TEST_MCH_ID,
            TEST_PRIVATE_KEY,
            TEST_SERIAL_NO,
            "ffffffffffffffffffffffffffffffff",
            TEST_NOTIFY_URL,
        );
        let err = wrong_key
            .decrypt_paydata("AAAA", "abcdefghijkl", "transaction")
            .expect_err("错误密钥应解密失败");
        assert_eq!(err.kind(), ErrorKind::Local, "实际: {err}");

        // 4) 本地层：验签失败
        //    注意 verify_signature 是同步方法（非 maybe_async），两种模式下都不要用 call!
        let err = wechat_pay
            .verify_signature(&public_key_pem(), "1", "n", "bm90LWEtc2lnbmF0dXJl", "{}")
            .expect_err("非法签名应失败");
        assert_eq!(err.kind(), ErrorKind::Local, "实际: {err}");
    }
}

dual_test! {
    fn sign_data_prefix_differs_between_app_and_jsapi() {
        // 历史回归点：提交 95cf80a「修复app支付"prepay_id="多余签名参数」。
        // APP 支付的 package 用裸 prepay_id，JSAPI / 付款码必须带 "prepay_id=" 前缀；
        // 传错会导致前端拉起支付失败，而且失败现象在客户端，很难排查。
        let prepay = "wx_prepay_0001";
        let ok_body = format!(r#"{{"prepay_id":"{prepay}"}}"#);
        let mock = Mock::start(vec![
            MockResponse::json(200, &ok_body),
            MockResponse::json(200, &ok_body),
            MockResponse::json(200, &ok_body),
        ]);
        let wechat_pay = client_for(&mock.base_url);

        let app = call!(wechat_pay.app_pay(AppParams::new("测试", "O_APP", 1.into())))
            .expect("app_pay");
        let jsapi = call!(wechat_pay.jsapi_pay(JsapiParams::new("测试", "O_JSAPI", 1.into(), "o".into())))
            .expect("jsapi_pay");
        let micro = call!(wechat_pay.micro_pay(MicroParams::new("测试", "O_MICRO", 1.into(), "o".into())))
            .expect("micro_pay");

        let app_sign = app.sign_data.expect("APP 必须返回 sign_data");
        let jsapi_sign = jsapi.sign_data.expect("JSAPI 必须返回 sign_data");
        let micro_sign = micro.sign_data.expect("付款码必须返回 sign_data");

        assert_eq!(
            app_sign.package, prepay,
            "APP 支付的 package 不应带 `prepay_id=` 前缀"
        );
        assert_eq!(
            jsapi_sign.package,
            format!("prepay_id={prepay}"),
            "JSAPI 的 package 必须带 `prepay_id=` 前缀"
        );
        assert_eq!(
            micro_sign.package,
            format!("prepay_id={prepay}"),
            "付款码的 package 必须带 `prepay_id=` 前缀"
        );

        // wx.requestPayment 需要的其余字段
        assert_eq!(app_sign.sign_type, "RSA");
        assert_eq!(app_sign.app_id, TEST_APPID);
        assert!(!app_sign.pay_sign.is_empty());
        assert!(!app_sign.nonce_str.is_empty());
        assert!(!app_sign.timestamp.is_empty());

        // 三个端点分别是各自的 URL
        let paths: Vec<String> = mock.requests().iter().map(|r| r.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                "/v3/pay/transactions/app",
                "/v3/pay/transactions/jsapi",
                "/v3/pay/transactions/jsapi",
            ]
        );
    }
}

dual_test! {
    fn refunds_success_returns_refund_order() {
        // 只测过失败路径还不够：申请退款成功的解析也要有断言。
        let body = r#"{"refund_id":"50000000382019052709732678859","out_refund_no":"R_S1","transaction_id":"4200000000000000000000000000","out_trade_no":"ORDER_S1","channel":"ORIGINAL","user_received_account":"支付用户零钱","create_time":"2026-09-11T12:00:00+08:00","status":"PROCESSING","funds_account":"UNSETTLED","amount":{"total":1,"refund":1,"payer_total":1,"payer_refund":1,"settlement_refund":1,"settlement_total":1,"discount_refund":0,"currency":"CNY"}}"#;
        let mock = Mock::start(vec![MockResponse::json(200, body)]);
        let wechat_pay = client_for(&mock.base_url);

        let refund = call!(wechat_pay.refunds(RefundsParams::new(
            "R_S1",
            1,
            1,
            None,
            Some("ORDER_S1"),
        )))
        .expect("受理成功应返回退款单");

        assert_eq!(refund.out_refund_no, "R_S1");
        assert_eq!(refund.status, "PROCESSING", "刚受理时是处理中，不是成功");
        assert_eq!(refund.amount.refund, 1);

        let requests = mock.requests();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v3/refund/domestic/refunds");
    }
}

dual_test! {
    fn get_weixin_extracts_deeplink_from_h5_page() {
        // H5 支付返回的页面里嵌着 weixin:// 拉起链接，SDK 逐行扫描把它抠出来。
        let page = "<html>\n<body>\n deeplink : \"weixin://wap/pay?prepayid%3Dwx123&package=1&noncestr=2&sign=3\"\n</body>\n</html>";
        let mock = Mock::start(vec![MockResponse::json(200, page)]);
        let wechat_pay = client_for(&mock.base_url);

        // 注意：get_weixin 接收的是**绝对 URL**（微信返回的 h5_url），
        // 不会拼 base_url —— 所以这里直接指向 mock。
        let page_url = format!("{}/h5-page", mock.base_url);
        let url = call!(wechat_pay.get_weixin(
            page_url.as_str(),
            "https://referer.example.com",
        ))
        .expect("页面里有链接时应成功");

        assert_eq!(
            url.as_deref(),
            Some("weixin://wap/pay?prepayid%3Dwx123&package=1&noncestr=2&sign=3")
        );
        // Referer 会随请求发出（微信 H5 页依赖它）
        assert_eq!(
            mock.requests()[0].header("referer"),
            Some("https://referer.example.com")
        );
    }
}

dual_test! {
    fn get_weixin_reports_not_found_without_deeplink() {
        let mock = Mock::start(vec![MockResponse::json(200, "<html>no link here</html>")]);
        let wechat_pay = client_for(&mock.base_url);
        let page_url = format!("{}/h5-page", mock.base_url);

        let err = call!(wechat_pay.get_weixin(
            page_url.as_str(),
            "https://referer.example.com"
        ))
        .expect_err("页面里没有链接时必须是 WeixinNotFound");

        assert!(matches!(err, PayError::WeixinNotFound), "实际: {err}");
    }
}

dual_test! {
    fn x509_helpers_extract_the_certificate_public_key() {
        // 微信下发的证书是 PEM（不是 DER）。这里验证 x509_to_pem 取出的是**正确**的公钥 ——
        // 判据是它必须能验证由配套私钥签出的签名，而不只是「长得像 PEM」。
        let pem = TEST_PLATFORM_CERT_PEM.as_bytes();

        let public_key = util::x509_to_pem(pem).expect("应能从证书中取出公钥");
        assert!(public_key.contains("-----BEGIN PUBLIC KEY-----"));
        assert!(public_key.contains("-----END PUBLIC KEY-----"));

        let message = "hello-x509";
        let signature = sign_rsa(message);
        util::verify_rsa_sha256(&public_key, message, &signature)
            .expect("证书里的公钥应能验证配套私钥的签名");

        let (valid, not_after) = util::x509_is_valid(pem).expect("应能读到有效期");
        assert!(valid, "测试证书自签发起 10 年有效期，应当仍然有效");
        assert!(not_after > 1_700_000_000, "not_after 应是 unix 秒：{not_after}");

        // 非 PEM 输入必须返回 Err，而不是 panic
        assert!(util::x509_to_pem(b"not a pem").is_err());
        assert!(util::x509_is_valid(b"not a pem").is_err());
    }
}

// ---------------------------------------------------------------------------
// 自动重试
// ---------------------------------------------------------------------------

/// 启动一个「收下请求、但永不响应」的 mock，并捕获收到的请求。
///
/// 用来把**超时**这条路走实：客户端每次尝试都会等到自己的请求超时，
/// 而服务端能如实数出它到底尝试了几次 —— 这正是「写接口超时不得重试」的判据。
fn black_hole_mock() -> (String, Arc<Mutex<Vec<CapturedRequest>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind black hole mock");
    let addr = listener.local_addr().expect("local_addr");
    let captured: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_bg = Arc::clone(&captured);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let captured = Arc::clone(&captured_bg);
            thread::spawn(move || {
                // 读走请求（让客户端确认「已发出」），然后既不响应也不关闭连接。
                // 客户端超时后会自己断开，此时 read_request 返回 None，线程结束。
                let mut reader = BufReader::new(stream);
                while let Some(request) = read_request(&mut reader) {
                    captured.lock().expect("lock captured").push(request);
                }
            });
        }
    });
    (format!("http://{addr}"), captured)
}

/// 启动一个「声称有 100 字节响应体、实际只发 10 字节就断连」的 mock。
///
/// 这是「微信**已经处理过**这次请求」的真实形态：响应都开始返回了，只是没读完。
/// 哪怕客户端最终拿到的是个传输层错误，这种请求也绝不能重放。
/// ⚠ reqwest 给这类失败打的标志位是 `is_decode()` 而**不是** `is_body()` ——
/// 按直觉写 `is_body()` 会把它误判成可以重试。
fn truncated_body_mock() -> (String, Arc<Mutex<Vec<CapturedRequest>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind truncated mock");
    let addr = listener.local_addr().expect("local_addr");
    let captured: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_bg = Arc::clone(&captured);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let captured = Arc::clone(&captured_bg);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut writer = stream;
                if let Some(request) = read_request(&mut reader) {
                    captured.lock().expect("lock captured").push(request);
                    let _ = writer.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"prepay_",
                    );
                    let _ = writer.flush();
                }
                // 连接在此关闭，客户端读到一半就断了。
            });
        }
    });
    (format!("http://{addr}"), captured)
}

/// 从 Authorization 头里取出 `nonce_str`。
fn nonce_of(request: &CapturedRequest) -> String {
    let auth = request
        .header("authorization")
        .expect("请求应当带 Authorization 头");
    auth_field(auth, "nonce_str")
}

/// 等 mock 的读取线程把请求收完，返回**稳定后**的请求数。
///
/// `Mock` / `truncated_body_mock` 有「先 push 再写响应」的顺序保证 —— 客户端拿到响应或
/// 报错时，捕获一定已经写入。但 `black_hole_mock` 永远不回响应，客户端返回与读取线程
/// 之间只剩调度窗口，直接断言就是一条理论上有几率失败的用例。
///
/// 这里等计数**连续 [`SETTLE_WINDOW`] 不变**才认账；窗口取得比「一次尝试的最长间隔」
/// （请求超时 + 退避）更长，所以既不会提前认账，也不会漏掉迟到的重试。
fn settled_captures(captured: &Arc<Mutex<Vec<CapturedRequest>>>) -> usize {
    /// 必须大于 `request` 超时 + 退避：否则一次尝试与下一次之间会被误判成「已经稳定」。
    const SETTLE_WINDOW: Duration = Duration::from_millis(800);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = captured.lock().expect("lock captured").len();
    loop {
        thread::sleep(SETTLE_WINDOW);
        let now = captured.lock().expect("lock captured").len();
        if now == last || Instant::now() >= deadline {
            return now;
        }
        last = now;
    }
}

/// 把退避压到毫秒级，避免测试真的等下去（默认策略是 200ms 起步、上限 2s）。
fn fast_retry(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        base_delay: Duration::from_millis(10),
        max_delay: Duration::from_millis(10),
        jitter: false,
    }
}

dual_test! {
    fn retries_on_frequency_limit_then_succeeds() {
        let mock = Mock::start(vec![
            MockResponse::json(429, r#"{"code":"FREQUENCY_LIMITED","message":"频率超限"}"#),
            MockResponse::json(204, ""),
        ]);
        let wechat_pay = client_for(&mock.base_url).with_retry(fast_retry(3));

        call!(wechat_pay.close_order("RETRY_429")).expect("429 之后应当重试并成功");

        assert_eq!(
            mock.requests().len(),
            2,
            "官方对 429 的措辞是「请求未受理，请降低操作频率后重试」，应当重试一次"
        );
    }
}

dual_test! {
    fn retries_until_max_attempts_then_gives_up() {
        let unavailable = r#"{"code":"SERVICE_UNAVAILABLE","message":"服务不可用"}"#;
        let mock = Mock::start(vec![
            MockResponse::json(503, unavailable),
            MockResponse::json(503, unavailable),
            MockResponse::json(503, unavailable),
            // 第 4 个响应不该被用到：max_attempts = 3
            MockResponse::json(204, ""),
        ]);
        let wechat_pay = client_for(&mock.base_url).with_retry(fast_retry(3));

        let err = call!(wechat_pay.close_order("RETRY_503")).expect_err("三次都失败应当返回错误");

        assert_eq!(mock.requests().len(), 3, "max_attempts = 3 应当恰好尝试 3 次");
        assert_eq!(err.kind(), ErrorKind::Api, "最终错误应是微信侧错误，实际: {err}");
    }
}

dual_test! {
    fn disabled_policy_sends_exactly_once() {
        let mock = Mock::start(vec![
            MockResponse::json(429, r#"{"code":"FREQUENCY_LIMITED","message":"频率超限"}"#),
            MockResponse::json(204, ""),
        ]);
        let wechat_pay = client_for(&mock.base_url).with_retry(RetryPolicy::disabled());

        call!(wechat_pay.close_order("NO_RETRY")).expect_err("关掉重试后应当直接失败");

        assert_eq!(mock.requests().len(), 1, "RetryPolicy::disabled 必须只发一次");
    }
}

dual_test! {
    fn each_retry_is_signed_again() {
        let mock = Mock::start(vec![
            MockResponse::json(429, r#"{"code":"FREQUENCY_LIMITED","message":"频率超限"}"#),
            MockResponse::json(204, ""),
        ]);
        let wechat_pay = client_for(&mock.base_url).with_retry(fast_retry(3));

        call!(wechat_pay.close_order("RESIGN")).expect("重试之后应当成功");

        let requests = mock.requests();
        assert_eq!(requests.len(), 2);
        assert_ne!(
            nonce_of(&requests[0]),
            nonce_of(&requests[1]),
            "每次重试都必须重新签名：重试可能跨过 5 分钟的签名有效窗口，\
             复用旧 nonce / timestamp 会直接 401"
        );
    }
}

dual_test! {
    fn accepted_202_is_retried_because_wechat_says_to_resend() {
        let mock = Mock::start(vec![
            MockResponse::json(202, ""),
            MockResponse::json(204, ""),
        ]);
        let wechat_pay = client_for(&mock.base_url).with_retry(fast_retry(3));

        call!(wechat_pay.close_order("ACCEPTED")).expect("202 之后应当重发并成功");

        assert_eq!(
            mock.requests().len(),
            2,
            "官方对 202 的要求是「服务器已接受请求，但尚未处理，请使用原参数重复请求一遍」"
        );
    }
}

dual_test! {
    fn accepted_202_that_cannot_be_retried_reports_the_status_not_a_json_error() {
        let mock = Mock::start(vec![MockResponse::json(202, "")]);
        let wechat_pay = client_for(&mock.base_url).with_retry(RetryPolicy::disabled());

        let err = call!(wechat_pay.close_order("ACCEPTED_OFF")).expect_err("202 不该当成成功");

        match err {
            PayError::ApiError { status, .. } => assert_eq!(status, 202, "应当保留真实状态码"),
            other => panic!("202 应当降级成 ApiError，而不是一个与真实原因无关的解析错误: {other:?}"),
        }
    }
}

dual_test! {
    fn write_timeout_is_not_retried_but_read_timeout_is() {
        // ① 写接口超时：请求确实发出去了，但结果未知 —— 官方口径是「先查单」。
        let (write_url, write_captured) = black_hole_mock();
        let writer = client_for(&write_url)
            .with_timeouts(HttpTimeouts {
                request: Duration::from_millis(200),
                ..HttpTimeouts::default()
            })
            .with_retry(fast_retry(3));

        let err = call!(writer.close_order("TIMEOUT_WRITE")).expect_err("超时应当报错");
        assert!(err.may_have_taken_effect(), "超时属于「结果未知」");
        assert_eq!(
            settled_captures(&write_captured),
            1,
            "写接口超时**不得**重试：结果未知，应当由调用方查单确认，而不是再发一次"
        );

        // ② 只读接口超时：重放纯读没有副作用，应当重试到 max_attempts。
        //    用独立的 mock 与客户端，避免复用上面那条已经超时作废的连接。
        let (read_url, read_captured) = black_hole_mock();
        let reader = client_for(&read_url)
            .with_timeouts(HttpTimeouts {
                request: Duration::from_millis(200),
                ..HttpTimeouts::default()
            })
            .with_retry(fast_retry(3));

        let err = call!(reader.query_order("TIMEOUT_READ")).expect_err("超时应当报错");

        assert!(err.may_have_taken_effect());
        assert_eq!(
            settled_captures(&read_captured),
            3,
            "只读接口超时应当重试：max_attempts = 3 就是 3 次尝试"
        );
    }
}

dual_test! {
    fn response_body_that_dies_midway_is_never_retried() {
        // ① 写接口：响应都开始返回了，说明微信**已经处理过**这次请求。
        let (write_url, write_captured) = truncated_body_mock();
        let writer = client_for(&write_url).with_retry(fast_retry(3));

        let err = call!(writer.close_order("TRUNC_WRITE")).expect_err("读响应体失败应当报错");
        assert!(
            err.may_have_taken_effect(),
            "响应已开始返回 —— 这次请求在微信侧很可能已经生效"
        );
        assert_eq!(
            write_captured.lock().expect("lock captured").len(),
            1,
            "「已处理」的请求绝不能重放（这类失败在 reqwest 里是 is_decode()，不是 is_body()）"
        );

        // ② 只读接口同样不得重放：判据是「服务端已处理」，与重放语义无关。
        let (read_url, read_captured) = truncated_body_mock();
        let reader = client_for(&read_url).with_retry(fast_retry(3));

        let err = call!(reader.query_order("TRUNC_READ")).expect_err("读响应体失败应当报错");

        assert!(err.may_have_taken_effect());
        assert_eq!(
            read_captured.lock().expect("lock captured").len(),
            1,
            "已处理的请求，只读接口也不得重放"
        );
    }
}

dual_test! {
    fn connection_refused_is_retried_even_for_writes() {
        // 127.0.0.1:1 上没有服务，连接会被立刻拒绝 —— 请求**确定没送出去**。
        // 这是唯一一种连写接口都能安全重放的场景（官方 Java SDK 默认就只重试这类失败）。
        let wechat_pay = client_for("http://127.0.0.1:1").with_retry(RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(120),
            max_delay: Duration::from_millis(120),
            jitter: false,
        });

        let started = Instant::now();
        let err = call!(wechat_pay.close_order("REFUSED")).expect_err("连不上应当报错");
        let elapsed = started.elapsed();

        assert_eq!(err.kind(), ErrorKind::Network);
        assert!(
            !err.may_have_taken_effect(),
            "连接都没建立起来，请求确定没送到微信"
        );
        assert!(
            elapsed >= Duration::from_millis(240),
            "连接失败应当重试：3 次尝试之间有 2 段 120ms 退避，实际只等了 {elapsed:?}"
        );
    }
}

/// 退款走独立的分钟级策略，且与通用策略**互不牵连**。
///
/// 不通过真实请求验证 —— 那要等 60 秒。这里钉的是公开配置契约：
/// 速率分档是「官方要求退款间隔 1 分钟 + 失败限流 6QPS」的直接后果，
/// 一旦有人把两档合并成一个 `RetryPolicy`，这个用例会立刻失败。
#[test]
fn refund_uses_a_separate_minute_scaled_policy() {
    let wechat_pay = client_for("http://127.0.0.1:1");

    assert!(
        wechat_pay.refund_retry_policy().base_delay >= Duration::from_secs(30),
        "退款退避必须是分钟级，实际 {:?}",
        wechat_pay.refund_retry_policy().base_delay
    );
    assert!(
        wechat_pay.retry_policy().base_delay < Duration::from_secs(1),
        "其余接口应当是毫秒级退避，实际 {:?}",
        wechat_pay.retry_policy().base_delay
    );

    // 单独关掉退款重试，不得连带影响通用重试。
    let tuned = client_for("http://127.0.0.1:1").with_refund_retry(RetryPolicy::disabled());
    assert_eq!(
        tuned.refund_retry_policy().max_attempts,
        1,
        "with_refund_retry 应当只作用于退款"
    );
    assert!(
        tuned.retry_policy().max_attempts > 1,
        "改退款策略不应把通用重试也关掉"
    );
}

dual_test! {
    fn refund_retry_actually_uses_the_refund_policy() {
        // 上面那条只断言两个 getter 的默认值，证明不了**退款真的走了退款策略** ——
        // 把 refunds() 的 kind 从 Refund 改成 Write、或 policy_for 丢掉 Refund 分支，
        // 所有测试都还是绿的，而退款会退回毫秒级退避（违反官方「间隔 1 分钟」）。
        let body = r#"{"refund_id":"50000000382019052709732678859","out_refund_no":"R_RETRY","transaction_id":"4200000000000000000000000000","out_trade_no":"ORDER_RETRY","channel":"ORIGINAL","user_received_account":"支付用户零钱","create_time":"2026-09-11T12:00:00+08:00","status":"PROCESSING","funds_account":"UNSETTLED","amount":{"total":1,"refund":1,"payer_total":1,"payer_refund":1,"settlement_refund":1,"settlement_total":1,"discount_refund":0,"currency":"CNY"}}"#;
        let mock = Mock::start(vec![
            MockResponse::json(429, r#"{"code":"FREQUENCY_LIMITED","message":"频率超限"}"#),
            MockResponse::json(200, body),
        ]);
        // 通用策略关掉、只放开退款策略：只有「退款确实选中 refund_retry」才会有第 2 次请求。
        let wechat_pay = client_for(&mock.base_url)
            .with_retry(RetryPolicy::disabled())
            .with_refund_retry(fast_retry(2));

        call!(wechat_pay.refunds(RefundsParams::new(
            "R_RETRY",
            1,
            1,
            None,
            Some("ORDER_RETRY"),
        )))
        .expect("429 之后应当按退款策略重试并成功");

        assert_eq!(
            mock.requests().len(),
            2,
            "退款必须选中退款策略；kind 标错就会退化成通用策略（这里已关掉）而只发 1 次"
        );
    }
}

dual_test! {
    fn close_order_does_not_treat_a_200_error_envelope_as_success() {
        // 关单走 request_no_content，而它不看响应体 —— 在此次改动之前，
        // 「HTTP 200 + 错误信封」会被当成关单成功，尽管 README 承诺过
        // 「包括 HTTP 200 但 body 是错误信封，都返回 Err」。
        let mock = Mock::start(vec![MockResponse::json(
            200,
            r#"{"code":"RULE_LIMIT","message":"业务规则限制"}"#,
        )]);
        let wechat_pay = client_for(&mock.base_url).with_retry(RetryPolicy::disabled());

        let err = call!(wechat_pay.close_order("ENVELOPE")).expect_err("200 + 错误信封不是成功");

        match err {
            PayError::ApiError { response, .. } => {
                assert_eq!(response.code.as_deref(), Some("RULE_LIMIT"));
            }
            other => panic!("应当返回 ApiError，实际得到 {other:?}"),
        }
    }
}

dual_test! {
    fn system_error_envelope_on_200_is_retried_like_a_500() {
        // 同一个系统级失败，载体是 HTTP 500 还是 HTTP 200 + 信封，不该有不同处置 ——
        // 官方对 SYSTEM_ERROR 的要求是「请用相同参数重新调用」。
        let mock = Mock::start(vec![
            MockResponse::json(200, r#"{"code":"SYSTEM_ERROR","message":"系统异常"}"#),
            MockResponse::json(204, ""),
        ]);
        let wechat_pay = client_for(&mock.base_url).with_retry(fast_retry(3));

        call!(wechat_pay.close_order("SYSTEM_ERROR_200")).expect("SYSTEM_ERROR 之后应当重试并成功");

        assert_eq!(
            mock.requests().len(),
            2,
            "错误码应当优先于状态码：HTTP 200 的 SYSTEM_ERROR 同样要重试"
        );
    }
}
