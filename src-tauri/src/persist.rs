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

/// 持久化文件路径。拿不到就返回 None，调用方静默跳过。
fn path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    Some(dir.join(FILE_NAME))
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
