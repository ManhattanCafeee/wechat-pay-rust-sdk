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
    /// 微信侧返回的业务错误（HTTP 状态码非 2xx）。
    ///
    /// 保留微信原始的错误码、错误信息与 detail，便于定位到具体字段。
    /// 匹配方式：`PayError::ApiError { status, response }`，用 `response.code` 分支处理。
    #[error("wechat api error: http {status}, {response}")]
    ApiError {
        /// HTTP 状态码
        status: u16,
        /// 微信返回的错误体
        response: ErrorResponse,
    },
}

impl PayError {
    /// 由非 2xx 响应体构造业务错误。
    ///
    /// 响应体不符合微信错误结构时（例如网关返回的 HTML），
    /// 把原始文本放进 `message` 保留现场，避免丢失唯一线索。
    pub(crate) fn api_error(status: u16, body: &str) -> Self {
        let response =
            serde_json::from_str::<ErrorResponse>(body).unwrap_or_else(|_| ErrorResponse {
                code: None,
                message: Some(body.to_string()),
                detail: None,
            });
        PayError::ApiError { status, response }
    }
}
