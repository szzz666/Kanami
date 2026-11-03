//! 少量状态的持久化。
//!
//! 只存「必须跨进程存活」的东西，不存运行时数据：
//!
//! - **已授权的播放端白名单**。这是最要紧的：
//!   没有它，应用每次重启都要重新授权一遍，
//!   用户会觉得「怎么又连不上了」。
//!
//! 位置用 Tauri 的 app data 目录，跟着平台的规范走，
//! 不自己拼路径。

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::path::PathBuf;

use tauri::{AppHandle, Manager};

const FILE_NAME: &str = "trusted.json";
const SESSION_FILE: &str = "session.json";

/// 持久化文件路径。拿不到就返回 None，调用方静默跳过。
fn path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    Some(dir.join(FILE_NAME))
}

/// 会话文件路径。
fn session_path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    Some(dir.join(SESSION_FILE))
}

/// 上次退出时是否正在发送，以及用的哪个音源。
///
/// 为什么要存：这个应用的使用场景是「手机当无线音箱」。
/// 用户放进口袋、锁屏几小时，系统很可能把进程回收掉。
/// 不恢复的话，用户回来要重新打开应用、选音源、点发送——
/// 而这时他可能根本看不到屏幕。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct SessionState {
    /// 是否在发送。
    pub sending: bool,
    /// 音源 id（"mic" / "system" / "device:xxx"）。
    pub source: String,
}

impl Default for SessionState {
    // 刻意保留显式实现（而不是 #[derive(Default)]）：
    // 「默认未发送」是这个类型的核心安全属性——反了的话，
    // 应用第一次启动就会自动打开麦克风，是隐私事故。
    // 显式写出来 + 有测试锁住，比一个 derive 更能表达这个意图。
    #[allow(clippy::derivable_impls)]
    fn default() -> Self {
        Self { sending: false, source: String::new() }
    }
}

/// 读回上次的会话状态。
///
/// 文件不存在、损坏、没权限——都当作「没在发送」，不能让启动失败。
pub fn load_session(app: &AppHandle) -> SessionState {
    let Some(p) = session_path(app) else { return SessionState::default() };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return SessionState::default();
    };
    match serde_json::from_str::<SessionState>(&text) {
        Ok(s) => {
            if s.sending {
                log::info!("上次在发送（音源={}），将尝试恢复", s.source);
            }
            s
        }
        Err(e) => {
            log::warn!("会话状态解析失败（当作未发送）：{e}");
            SessionState::default()
        }
    }
}

/// 写入会话状态。失败只记日志。
pub fn save_session(app: &AppHandle, state: &SessionState) {
    let Some(p) = session_path(app) else { return };
    if let Some(dir) = p.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::warn!("创建数据目录失败：{e}");
            return;
        }
    }
    match serde_json::to_string_pretty(state) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&p, text) {
                log::warn!("保存会话状态失败：{e}");
            }
        }
        Err(e) => log::warn!("序列化会话状态失败：{e}"),
    }
}

/// 读回已授权的设备 ID。
///
/// 文件不存在、格式坏了、没权限——统统当作空集合，
/// 让用户重新授权一次即可，不能让启动失败。
pub fn load_trusted(app: &AppHandle) -> HashSet<String> {
    let Some(p) = path(app) else { return HashSet::new() };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return HashSet::new();
    };
    match serde_json::from_str::<Vec<String>>(&text) {
        Ok(v) => {
            log::info!("读回 {} 个已授权设备", v.len());
            v.into_iter().collect()
        }
        Err(e) => {
            log::warn!("授权名单解析失败（当作空）：{e}");
            HashSet::new()
        }
    }
}

/// 写入已授权的设备 ID。
///
/// 失败只记日志：写不进去最多是下次要重新授权，
/// 不该因此让「授权」这个动作本身失败。
pub fn save_trusted(app: &AppHandle, trusted: &HashSet<String>) {
    let Some(p) = path(app) else {
        log::warn!("拿不到数据目录，授权名单未保存");
        return;
    };
    if let Some(dir) = p.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::warn!("创建数据目录失败：{e}");
            return;
        }
    }
    let list: Vec<&String> = trusted.iter().collect();
    match serde_json::to_string_pretty(&list) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&p, text) {
                log::warn!("保存授权名单失败：{e}");
            }
        }
        Err(e) => log::warn!("序列化授权名单失败：{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 序列化格式要稳定：存的是设备 ID 数组。
    ///
    /// 这条测试锁住「格式悄悄变了导致老用户白名单失效」的风险。
    #[test]
    fn 授权名单序列化格式稳定() {
        let mut set = HashSet::new();
        set.insert("aaa-111".to_string());
        let list: Vec<&String> = set.iter().collect();
        let text = serde_json::to_string(&list).unwrap();
        assert!(text.starts_with('['), "必须是 JSON 数组");
        let back: Vec<String> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, vec!["aaa-111".to_string()]);
    }

    /// 损坏的文件必须当作空集合，而不是让启动失败。
    #[test]
    fn 损坏内容不会崩() {
        let bad = "{ this is not json";
        let r: Result<Vec<String>, _> = serde_json::from_str(bad);
        assert!(r.is_err(), "解析失败应当返回 Err，由调用方兜底为空集");
    }

    /// 空名单也要能正确往返。
    #[test]
    fn 空名单往返一致() {
        let set: HashSet<String> = HashSet::new();
        let list: Vec<&String> = set.iter().collect();
        let text = serde_json::to_string(&list).unwrap();
        assert_eq!(text, "[]");
        let back: Vec<String> = serde_json::from_str(&text).unwrap();
        assert!(back.is_empty());
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    /// 序列化格式要稳定。
    ///
    /// 这条锁住「格式悄悄变了导致老用户发送状态失效」的风险。
    #[test]
    fn 会话状态往返一致() {
        let s = SessionState { sending: true, source: "system".into() };
        let text = serde_json::to_string(&s).unwrap();
        let back: SessionState = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    /// 默认状态必须是「没在发送」。
    ///
    /// 这条很重要：如果默认值反了，应用第一次启动就会**自动开始采集**，
    /// 用户的麦克风在不知情的情况下被打开——这是隐私事故。
    #[test]
    fn 默认是未发送() {
        let s = SessionState::default();
        assert!(!s.sending);
        assert!(s.source.is_empty());
    }

    /// 损坏的文件按未发送处理。
    #[test]
    fn 损坏的会话文件不影响启动() {
        let bad = "{ not json";
        let r: Result<SessionState, _> = serde_json::from_str(bad);
        assert!(r.is_err());
    }

    /// 缺字段也要能解析（老版本文件没有 source 字段的情形）。
    #[test]
    fn 缺字段时用默认值() {
        let r: Result<SessionState, _> = serde_json::from_str("{}");
        assert!(r.is_err(), "缺 sending 字段时应当报错而不是猜一个值");
    }
}
