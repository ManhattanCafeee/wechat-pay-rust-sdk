use crate::response::ErrorResponse;

#[derive(Debug, thiserror::Error)]
pub enum PayError {
    #[error("http error: {0}")]
    RequestError(#[from] reqwest::Error),
    #[error("pay error: {0}")]
    WechatError(String),
    #[error("json error: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("Decrypt error: {0}")]
    DecryptError(String),
    #[error("Base64 decode error: {0}")]
    DecodeError(#[from] base64::DecodeError),
    #[error("verify error: {0}")]
    VerifyError(String),
    #[error("weixin not found error")]
    WeixinNotFound,
    /// 平台上没有该 `Wechatpay-Serial` 对应的密钥。
    ///
    /// 通常意味着微信正在轮换平台证书（轮换期会同时下发新旧两张）——
    /// 应当**立即重新拉取**平台证书列表（`WechatPay::fetch_platform_keys`）后重试，
    /// 而不是拿别的密钥去试。
    #[error("unknown platform serial: {0}")]
    UnknownPlatformSerial(String),
    /// 回调/应答的时间戳超出允许窗口，判定为重放，已拒绝处理。
    #[error("stale notify rejected: {0}")]
    StaleNotify(String),
    /// 微信侧返回的业务错误（HTTP 状态码非 2xx）。
    ///
    /// 保留微信原始的错误码、错误信息与 detail，便于定位到具体字段。
    /// 匹配方式：`PayError::ApiError { status, response }`，用 `response.code` 分支处理。
    ///
    /// ⚠ 日志提示：`Display` 会带上 `detail`，而微信的 `detail` 含出错字段的路径与取值
    /// （例如 `/payer/openid`），`message` 也可能回显提交内容。日志外发前需自行脱敏。
    #[error("wechat api error: http {status}, {response}")]
    ApiError {
        /// HTTP 状态码
        status: u16,
        /// 微信返回的错误体
        response: ErrorResponse,
    },
}

/// 原始响应体保留进错误消息时的最大字符数。
/// 防止网关 / WAF 返回的整页 HTML 被复制进错误消息并刷爆日志。
const MAX_RAW_BODY_CHARS: usize = 4096;

impl PayError {
    /// 由非 2xx 响应体构造业务错误。
    ///
    /// 仅当响应体**确实是微信的错误结构**（至少含 `code` / `message` / `detail` 之一）
    /// 时才接受结构化解析；否则把原始文本截断后放进 `message`。
    ///
    /// 不能只判断「能否解析成 JSON 对象」：`ErrorResponse` 的字段全是 `Option` 且未加
    /// `deny_unknown_fields`，任何 JSON 对象都能解析成功。若不加这层判断，WAF / 反向
    /// 代理返回的 `{"status":403,"msg":"..."}` 会变成一个三个字段全为 `None` 的
    /// `ErrorResponse`，唯一的排查线索（原始文本）就被静默丢弃了。
    pub(crate) fn api_error(status: u16, body: &str) -> Self {
        let parsed = serde_json::from_str::<ErrorResponse>(body)
            .ok()
            .filter(|r| r.code.is_some() || r.message.is_some() || r.detail.is_some());

        let response = parsed.unwrap_or_else(|| ErrorResponse {
            code: None,
            message: Some(truncate_raw_body(body)),
            detail: None,
        });

        PayError::ApiError { status, response }
    }
}

/// 截断过长的响应体并明确标注截断，空响应体给出显式标记。
fn truncate_raw_body(body: &str) -> String {
    if body.is_empty() {
        return "<empty body>".to_string();
    }
    if body.chars().count() <= MAX_RAW_BODY_CHARS {
        return body.to_string();
    }
    let head: String = body.chars().take(MAX_RAW_BODY_CHARS).collect();
    format!("{head}… (truncated, {} bytes total)", body.len())
}
