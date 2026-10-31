//! 用户手填地址的容错解析。
//!
//! 比 `str::parse::<SocketAddr>` 宽容，因为真实输入并不干净：
//!
//! - **中文输入法会把 `.` 和 `:` 打成全角的 `。` `：`。**
//!   实测在手机上填 `192.168.3.149:58893` 会变成
//!   `192。168。3。149：58893`，直接解析必然失败，
//!   而用户看着屏幕觉得「我明明填对了」。
//! - 从聊天工具复制常带首尾空格或换行。
//! - 有人会顺手粘上 `http://` 前缀。
//!
//! 这些都是用户无意的，不该报错让他们自己找原因。

#![forbid(unsafe_code)]

use std::net::SocketAddr;

/// 解析用户手填的设备地址。
pub fn parse_manual_addr(raw: &str) -> Result<SocketAddr, String> {
    // 全角标点 → 半角
    let mut s: String = raw
        .chars()
        .map(|c| match c {
            '。' | '．' => '.',
            '：' => ':',
            '　' => ' ',
            _ => c,
        })
        .collect();

    // 去掉可能粘上的协议前缀与路径
    for prefix in ["http://", "https://", "kanami://"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            break;
        }
    }
    if let Some(idx) = s.find('/') {
        s.truncate(idx);
    }
    let s = s.trim();

    if s.is_empty() {
        return Err("请填写设备地址".into());
    }

    // 只有 IP 没写端口时，补上默认发现端口
    if let Ok(ip) = s.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, kanami_net::discovery::DISCOVERY_PORT));
    }

    s.parse::<SocketAddr>().map_err(|_| "地址格式不对，应当形如 192.168.1.10:47822".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 中文输入法把点号和冒号打成全角，是最常见的「填了却连不上」。
    #[test]
    fn 全角标点也能解析() {
        let a = parse_manual_addr("192。168。3。149：58893").expect("应当能解析");
        assert_eq!(a.to_string(), "192.168.3.149:58893");
    }

    /// 首尾空格与换行（从聊天工具复制粘贴）。
    #[test]
    fn 首尾空白会被忽略() {
        let a = parse_manual_addr("  192.168.1.10:47822  ").expect("应当能解析");
        assert_eq!(a.to_string(), "192.168.1.10:47822");
    }

    /// 粘上协议前缀是常见手误。
    #[test]
    fn 忽略协议前缀() {
        let a = parse_manual_addr("http://192.168.1.10:47822/").expect("应当能解析");
        assert_eq!(a.to_string(), "192.168.1.10:47822");
    }

    /// 只填 IP 时补默认端口，而不是报错。
    #[test]
    fn 只填ip时补默认端口() {
        let a = parse_manual_addr("192.168.1.10").expect("应当能解析");
        assert_eq!(a.port(), kanami_net::discovery::DISCOVERY_PORT);
        assert_eq!(a.ip().to_string(), "192.168.1.10");
    }

    /// 空输入要有明确提示。
    #[test]
    fn 空输入报错() {
        assert!(parse_manual_addr("   ").is_err());
    }

    /// 真正的垃圾输入仍然要报错，不能静默接受。
    #[test]
    fn 非法输入报错() {
        assert!(parse_manual_addr("不是地址").is_err());
        assert!(parse_manual_addr("192.168.1.999:100").is_err());
    }
}
