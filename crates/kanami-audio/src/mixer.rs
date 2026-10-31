//! 多路混音。
//!
//! 一个播放端可以同时接多路音源（软上限 8 路，按 4 路验收）。混在这里做，
//! 三个硬性要求来自需求共识：
//!
//! - **每路独立增益/静音/独奏。**
//! - **在解码后、输出前用 f32 累加，最后统一软限幅。** 整数直接相加会爆音，
//!   这是最典型的多音源事故。
//! - **某一路断流不能拖垮其他路。** 缺数据的那一路当作静音参与混音，
//!   而不是让整个输出卡住等它。

#![forbid(unsafe_code)]

use std::collections::HashMap;

/// 单路音源的混音状态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceState {
    /// 线性增益，1.0 为原始音量。
    pub gain: f32,
    pub muted: bool,
    /// 独奏。只要有任意一路处于独奏状态，其余路一律静音。
    pub solo: bool,
    /// 是否仍在正常供数。断流时该路按静音参与混音。
    pub active: bool,
}

impl Default for SourceState {
    fn default() -> Self {
        Self { gain: 1.0, muted: false, solo: false, active: true }
    }
}

impl SourceState {
    /// 该路在当前全局状态下是否应当出声。
    pub fn effective_gain(&self, any_solo: bool) -> f32 {
        if !self.active || self.muted {
            return 0.0;
        }
        if any_solo && !self.solo {
            return 0.0;
        }
        self.gain.clamp(0.0, 4.0)
    }
}

/// 多路混音器。
#[derive(Debug, Default)]
pub struct Mixer {
    sources: HashMap<u16, SourceState>,
}

impl Mixer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 加入一路音源。返回 false 表示已经达到软上限。
    pub fn add_source(&mut self, id: u16, max_sources: usize) -> bool {
        if self.sources.contains_key(&id) {
            return true;
        }
        if self.sources.len() >= max_sources {
            return false;
        }
        self.sources.insert(id, SourceState::default());
        true
    }

    /// 移除一路音源。调用方负责决定移除后是否要重新归一化。
    pub fn remove_source(&mut self, id: u16) -> Option<SourceState> {
        self.sources.remove(&id)
    }

    pub fn source(&self, id: u16) -> Option<&SourceState> {
        self.sources.get(&id)
    }

    pub fn source_mut(&mut self, id: u16) -> Option<&mut SourceState> {
        self.sources.get_mut(&id)
    }

    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// 是否有任意一路处于独奏状态。
    pub fn any_solo(&self) -> bool {
        self.sources.values().any(|s| s.solo && s.active && !s.muted)
    }

    /// 把多路交错立体声样本混成一路。
    ///
    /// - inputs：每路一段交错立体声样本，长度必须相同（由调用方补齐）。
    /// - out：输出缓冲区，长度必须等于单路样本数。
    ///
    /// 混音完成后统一做软限幅，保证不削波。
    pub fn mix(&self, inputs: &[(u16, &[f32])], out: &mut [f32]) {
        out.fill(0.0);
        let any_solo = self.any_solo();

        for (id, samples) in inputs {
            let state = match self.sources.get(id) {
                Some(s) => s,
                // 未登记的来源不参与混音，避免来路不明的数据混进输出
                None => continue,
            };
            let gain = state.effective_gain(any_solo);
            if gain == 0.0 {
                continue;
            }
            for (dst, src) in out.iter_mut().zip(samples.iter()) {
                *dst += src * gain;
            }
        }

        // f32 累加后统一软限幅：先找峰值，超过阈值才压缩，
        // 否则原样输出，避免小音量也被压出失真。
        let peak = out.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
        if peak > 1.0 {
            // 用 tanh 形状的软拐点，比硬除以峰值更自然
            let norm = 1.0 / peak;
            for sample in out.iter_mut() {
                let x = *sample * norm;
                *sample = x - (x * x * x) / 3.0;
            }
        }
    }
}

/// 多路混音器在同一时刻可接入的音源上限。
pub const MAX_SOURCES: usize = 8;
#[cfg(test)]
mod multistream_tests {
    use crate::mixer::{Mixer, MAX_SOURCES};

    /// 多路音源必须能被独立区分。
    ///
    /// 这对应一个真实修过的 bug：播放端连接第二台音源时写死了
    /// stream_id = 1，接收线程又总是投喂给「列表里第一个」，
    /// 结果第二路的数据盖掉了第一路。
    #[test]
    fn 多路音源各自独立混音() {
        let mut mixer = Mixer::new();
        // 模拟三路音源，各自不同的 stream_id
        for id in [1u16, 2, 3] {
            assert!(mixer.add_source(id, MAX_SOURCES));
        }
        assert_eq!(mixer.source_count(), 3);

        // 三路各自静音互不影响
        mixer.source_mut(2).unwrap().muted = true;
        assert!(mixer.source(2).unwrap().muted);
        assert!(!mixer.source(1).unwrap().muted);
        assert!(!mixer.source(3).unwrap().muted);

        // 独奏第 3 路，前两路应当静音
        mixer.source_mut(3).unwrap().solo = true;
        assert!(mixer.any_solo());
        let gain1 = mixer.source(1).unwrap().effective_gain(true);
        let gain3 = mixer.source(3).unwrap().effective_gain(true);
        assert_eq!(gain1, 0.0, "独奏时其他路必须闭嘴");
        assert!(gain3 > 0.0, "独奏的那一路必须出声");
    }

    /// 同一路重复加入不能被算成两路。
    #[test]
    fn 重复的_stream_id_不会占两个位置() {
        let mut mixer = Mixer::new();
        for _ in 0..10 {
            mixer.add_source(7, MAX_SOURCES);
        }
        assert_eq!(mixer.source_count(), 1);
    }

    /// 移除其中一路不能影响其他路。
    #[test]
    fn 移除一路不影响其他路() {
        let mut mixer = Mixer::new();
        for id in [1u16, 2, 3] {
            mixer.add_source(id, MAX_SOURCES);
        }
        assert!(mixer.remove_source(2).is_some());
        assert_eq!(mixer.source_count(), 2);
        assert!(mixer.source(1).is_some());
        assert!(mixer.source(3).is_some());
        assert!(mixer.source(2).is_none());
    }
}

#[cfg(test)]
mod control_state_tests {
    use crate::mixer::{Mixer, MAX_SOURCES};

    /// 混音台的每路控制状态必须能被读回。
    ///
    /// 这条测试锁住一个真实缺陷：status 只回传包数/延迟，
    /// 不回传 gain/muted/solo，于是前端只能写死——
    /// 桌面端的「独奏」按钮永远传 true，点了就取消不掉。
    #[test]
    fn 控制状态可读回() {
        let mut mixer = Mixer::new();
        mixer.add_source(1, MAX_SOURCES);

        mixer.source_mut(1).unwrap().gain = 0.5;
        mixer.source_mut(1).unwrap().muted = true;
        mixer.source_mut(1).unwrap().solo = true;

        let s = mixer.source(1).expect("应当能读到这一路");
        assert_eq!(s.gain, 0.5);
        assert!(s.muted);
        assert!(s.solo);
    }

    /// 独奏必须能取消。
    ///
    /// 前端按 `!s.solo` 传参，前提是状态真的能翻回去。
    #[test]
    fn 独奏可以取消() {
        let mut mixer = Mixer::new();
        mixer.add_source(1, MAX_SOURCES);
        mixer.source_mut(1).unwrap().solo = true;
        assert!(mixer.any_solo());

        mixer.source_mut(1).unwrap().solo = false;
        assert!(!mixer.any_solo(), "取消独奏后不应再有独奏路");
    }

    /// 不存在的 stream_id 读回来是 None，不能 panic。
    #[test]
    fn 未知音源读回为_none() {
        let mixer = Mixer::new();
        assert!(mixer.source(99).is_none());
    }
}
