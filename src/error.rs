use crate::response::ErrorResponse;

/// 本 crate 的统一错误类型。
///
/// 用 [`PayError::kind`] 做三层归类（网络 / 业务 / 本地）来决定处置策略；
/// 微信侧的业务错误统一是 [`PayError::ApiError`]，原始错误码与字段级 `detail` 都在里面。
#[derive(Debug, thiserror::Error)]
pub enum PayError {
    /// 传输 / 连接层失败（含超时、TLS、DNS）。
    #[error("http error: {0}")]
    RequestError(#[from] reqwest::Error),
    /// 通用的微信业务错误文本。
    #[error("pay error: {0}")]
    WechatError(String),
    /// 响应体不是合法 JSON，或结构与目标类型不符。
    #[error("json error: {0}")]
    JsonError(#[from] serde_json::Error),
    /// 回调 / 应答解密失败（AES-256-GCM）。
    #[error("Decrypt error: {0}")]
    DecryptError(String),
    /// Base64 解码失败。
    #[error("Base64 decode error: {0}")]
    DecodeError(#[from] base64::DecodeError),
    /// 验签失败。
    #[error("verify error: {0}")]
    VerifyError(String),
    /// 在 H5 页面里没找到 `weixin://` 拉起链接。
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

/// `PayError` 的三层归类。
///
/// 用来决定处置策略 —— 重试、告警、还是把原因直接展示给用户：
///
/// | 归类 | 含义 | 建议处置 |
/// | --- | --- | --- |
/// | [`Network`](ErrorKind::Network) | 传输 / 连接层失败 | 可考虑重试。⚠ **但下单类接口不可无脑重试**（会重复下单），只对幂等的 GET 查单重试 |
/// | [`Api`](ErrorKind::Api) | 微信侧业务拒绝（HTTP 非 2xx） | 不要重试；按 `response.code` 分支。`ORDER_NOT_EXIST` 这类是**正常业务结果**，不是故障 |
/// | [`Local`](ErrorKind::Local) | 本地错误：签名、解密、Base64、JSON 解析、验签失败、回调超窗 | 不要重试，通常意味着配置或数据有问题，应当告警 |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// 传输 / 连接层失败（`reqwest::Error`，含超时）。
    Network,
    /// 微信侧的业务拒绝：HTTP 非 2xx，或 2xx 但 body 是错误信封。
    Api,
    /// 本地错误：签名、解密、Base64、JSON 解析、验签失败、回调超窗等。
    Local,
}

impl PayError {
    /// 三层归类，见 [`ErrorKind`]。
    ///
    /// 这里是**穷尽匹配**：`PayError` 新增变体时编译器会强制你在这里做出归类决定。
    pub fn kind(&self) -> ErrorKind {
        match self {
            PayError::RequestError(_) => ErrorKind::Network,
            PayError::ApiError { .. } => ErrorKind::Api,
            PayError::WechatError(_)
            | PayError::JsonError(_)
            | PayError::DecryptError(_)
            | PayError::DecodeError(_)
            | PayError::VerifyError(_)
            | PayError::WeixinNotFound
            | PayError::UnknownPlatformSerial(_)
            | PayError::StaleNotify(_) => ErrorKind::Local,
        }
    }
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
