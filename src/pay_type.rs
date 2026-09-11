use std::fmt::{Display, Formatter};

/// 支付方式。
///
/// ⚠ crate 内部并未使用它：`Display` 输出的全大写字符串（`MICRO` / `JSAPI` / …）与微信
/// 文档里的 `trade_type` 取值一致，仅供调用方做分类与展示。
#[derive(Debug, PartialEq, Eq)]
pub enum PayType {
    /// 付款码支付
    Micro,
    /// 小程序 / 公众号支付
    Jsapi,
    /// 扫码支付（返回 `code_url`，由商户自行生成二维码）
    Native,
    /// APP 支付
    App,
    /// H5 支付（外部浏览器）
    H5,
    /// 二维码支付
    Qrcode,
}

impl Display for PayType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            PayType::Micro => write!(f, "MICRO"),
            PayType::Jsapi => write!(f, "JSAPI"),
            PayType::Native => write!(f, "NATIVE"),
            PayType::App => write!(f, "APP"),
            PayType::H5 => write!(f, "H5"),
            PayType::Qrcode => write!(f, "QRCODE"),
        }
    }
}
