//! 微信支付回调服务器（actix-web 参考实现）。
//!
//! 只演示**回调**这一条链路：**验签 → 解密 → 幂等落库 → 应答**。
//! 下单在业务流程里做（小程序/JSAPI 用 `jsapi_pay`），不在回调服务器里演示。
//!
//! # 为什么这几步缺一不可
//!
//! | 少做哪步 | 后果 |
//! | --- | --- |
//! | 不验签 | 任何人都能伪造「支付成功」通知让你发货 |
//! | 不查时间戳 | 抓到一次合法回调即可无限重放 —— 发一次货，重复领取 |
//! | 写死单张证书 | 平台证书轮换期验签全部失败 → 回调被拒 → 订单不发货 |
//! | 不做幂等 | 微信重投 15 次 → 重复发货 |
//!
//! 环境变量见 [`WechatPay::from_env`]：`WECHAT_APPID` / `WECHAT_MCH_ID` /
//! `WECHAT_PRIVATE_KEY`（**PEM 文件路径**，不是密钥内容）/ `WECHAT_SERIAL_NO` /
//! `WECHAT_V3_KEY`（32 字节）/ `WECHAT_NOTIFY_URL`。

use actix_web::web::{Bytes, Data};
use actix_web::{App, HttpRequest, HttpResponse, HttpServer, post};
use wechat_pay_rust_sdk::cert::PlatformKeys;
use wechat_pay_rust_sdk::model::{WechatPayDecodeData, WechatPayNotify};
use wechat_pay_rust_sdk::notify::NotifyHeaders;
use wechat_pay_rust_sdk::pay::{PayNotifyTrait, WechatPay};

/// 失败应答：官方要求校验或处理失败时返回 4XX / 5XX，body 为
/// `{"code":"FAIL","message":"…"}`，微信收到后会按退避策略重投。
fn fail(message: impl std::fmt::Display) -> HttpResponse {
    HttpResponse::BadRequest().json(serde_json::json!({
        "code": "FAIL",
        "message": message.to_string(),
    }))
}

/// 支付结果回调。
///
/// ⚠ 官方要求 **5 秒内**应答，所以真实的落库/发货必须异步化，不要在这个函数里同步做完。
#[post("/pay/notify")]
async fn pay_notify(
    req: HttpRequest,
    body: Bytes,
    keys: Data<PlatformKeys>,
    wechat_pay: Data<WechatPay>,
) -> HttpResponse {
    // ① 提取四个验签请求头（缺任意一个都会返回 Err）。
    let headers = match NotifyHeaders::from_pairs(
        req.headers()
            .iter()
            .map(|(name, value)| (name.as_str(), value.to_str().unwrap_or_default())),
    ) {
        Ok(headers) => headers,
        Err(err) => return fail(err),
    };

    // ⚠ 必须拿**原始** body 验签：先反序列化再序列化会改变字节，验签必然失败。
    let raw_body = match std::str::from_utf8(&body) {
        Ok(raw_body) => raw_body,
        Err(_) => return fail("回调 body 不是合法 UTF-8"),
    };

    // ② 验签三步：时间戳新鲜度（±300s）→ 按 Wechatpay-Serial 选键 → 验签。
    if let Err(err) = keys.verify_notify(&headers, raw_body) {
        // ⚠ 生产代码在这里应当区分两种情况：
        //   UnknownPlatformSerial = 微信正在轮换平台证书，**立即重新拉取**再重试一次；
        //   StaleNotify            = 疑似重放，直接拒绝并告警。
        return fail(err);
    }

    // ③ 解密 resource。解密不能替代验签 —— 顺序永远是先验签再解密。
    let notify: WechatPayNotify = match serde_json::from_str(raw_body) {
        Ok(notify) => notify,
        Err(err) => return fail(format!("回调 JSON 解析失败: {err}")),
    };
    let data: WechatPayDecodeData = match wechat_pay.decrypt_paydata(
        notify.resource.ciphertext,
        notify.resource.nonce,
        notify.resource.associated_data.unwrap_or_default(),
    ) {
        Ok(data) => data,
        Err(err) => return fail(err),
    };

    // ④ 幂等落库 —— **SDK 不代劳这一步**。
    //    微信在收到成功应答前会重投（最多 15 次），必须按 out_trade_no /
    //    transaction_id 去重，且重复投递要返回**成功**，否则微信会一直重投。
    handle_payment(&data);

    // ⑤ 成功应答：200 或 204，且**不带 body**。
    HttpResponse::NoContent().finish()
}

/// 业务落库的占位实现 —— 真实项目请换成「幂等写库 + 异步发货」。
fn handle_payment(data: &WechatPayDecodeData) {
    tracing::info!(
        out_trade_no = %data.out_trade_no,
        transaction_id = %data.transaction_id,
        trade_state = %data.trade_state,
        "收到支付通知（请在此处按 out_trade_no 幂等落库）"
    );
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    // 启动时拉一次平台证书。之后应当按 `PlatformKeys::needs_refresh` 定时刷新
    // （官方要求至少每 12 小时一次），并在验签拿到 `UnknownPlatformSerial` 时立即重拉。
    let bootstrap = WechatPay::from_env();
    let keys = bootstrap
        .fetch_platform_keys()
        .await
        .expect("拉取平台证书失败：检查商户证书、证书序列号与 APIv3 密钥");
    tracing::info!(count = keys.len(), serials = ?keys.serials(), "已加载平台密钥");

    // `WechatPay` 不实现 Clone（它持有连接池），所以每个 worker 各建一份；
    // 证书索引是只读的，直接 clone 即可。
    HttpServer::new(move || {
        App::new()
            .app_data(Data::new(WechatPay::from_env()))
            .app_data(Data::new(keys.clone()))
            .service(pay_notify)
    })
    .bind(("0.0.0.0", 8080))?
    .run()
    .await
}
