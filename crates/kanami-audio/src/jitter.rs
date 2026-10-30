//! 抖动缓冲。
//!
//! 把 UDP 上乱序、重复、迟到的包整理成连续的帧流交给解码器。
//!
//! 这里的核心取舍来自需求共识：
//!
//! - **低延迟模式固定缓冲，不增长。** 宁可丢音也要守住 20ms 的延迟承诺，
//!   所以该模式下上限等于下限，自适应逻辑直接失效。
//! - **标准/高音质模式在两个水位之间自适应。** 网络抖动变大时抬高水位保连续，
//!   平稳后再慢慢降回来，避免延迟只涨不跌。
//! - 延迟以**帧**为单位计算，因为一帧就是一帧 Opus，不需要更细的粒度。

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use kanami_proto::ModeParams;

/// 一帧已收到的音频数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub seq: u32,
    /// 采集时刻（毫秒），用于端到端延迟测量。
    pub timestamp_ms: u32,
    pub payload: Vec<u8>,
}

/// 抖动缓冲的统计指标，直接喂给 UI 的「当前延迟」显示。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitterStats {
    /// 当前缓冲水位（帧）。
    pub depth_frames: u32,
    /// 当前目标水位（帧）。
    pub target_frames: u32,
    /// 累计丢失的包数。
    pub lost: u64,
    /// 累计晚到被丢弃的包数。
    pub late: u64,
    /// 累计重复包数。
    pub duplicate: u64,
    /// 累计欠载次数（该出帧却没有足够水位）。
    pub underruns: u64,
}

/// 按序号重排、按目标水位输出的缓冲区。
#[derive(Debug)]
pub struct JitterBuffer {
    /// 已收到但还没出队的帧，按序号排序。
    pending: BTreeMap<u32, Frame>,
    /// 下一个要输出的序号。
    next_seq: u32,
    /// 是否已经输出过至少一帧。在此之前处于预滚阶段。
    playing: bool,
    /// 当前目标水位（帧）。
    target_frames: u32,
    min_frames: u32,
    max_frames: u32,
    /// 每帧时长（毫秒），用于把水位换算成时间。
    frame_ms: u32,
    stats: JitterStats,
    /// 连续欠载计数，用于决定是否抬高水位。
    recent_underruns: u32,
    /// 连续充足计数，用于决定是否降低水位。
    healthy: u32,
}

impl JitterBuffer {
    /// 按模式参数构造。水位由 jitter_min_ms / jitter_max_ms 换算成帧数。
    pub fn new(params: ModeParams) -> Self {
        let frame_ms = params.frame_ms.max(1);
        let to_frames = |ms: u32| (ms / frame_ms).max(1);
        let min_frames = to_frames(params.jitter_min_ms);
        let max_frames = to_frames(params.jitter_max_ms).max(min_frames);
        Self {
            pending: BTreeMap::new(),
            next_seq: 0,
            playing: false,
            target_frames: min_frames,
            min_frames,
            max_frames,
            frame_ms,
            stats: JitterStats::default(),
            recent_underruns: 0,
            healthy: 0,
        }
    }

    /// 当前目标水位换算成毫秒。
    pub fn target_latency_ms(&self) -> u32 {
        self.target_frames * self.frame_ms
    }

    /// 缓冲是否自适应。低延迟模式下为 false。
    pub fn is_adaptive(&self) -> bool {
        self.max_frames > self.min_frames
    }

    /// 是否已经开始出帧。
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn stats(&self) -> JitterStats {
        JitterStats {
            depth_frames: self.pending.len() as u32,
            target_frames: self.target_frames,
            ..self.stats
        }
    }

    /// 投入一个刚收到的帧。
    ///
    /// 返回 false 表示该帧被丢弃（重复，或已经晚到没有播放机会）。
    pub fn push(&mut self, frame: Frame) -> bool {
        if self.playing {
            // 已经播过的序号：重复到达，或绕了一圈才到
            if Self::seq_before(frame.seq, self.next_seq) {
                self.stats.late += 1;
                return false;
            }
            if self.pending.contains_key(&frame.seq) {
                self.stats.duplicate += 1;
                return false;
            }
        } else {
            // 预滚阶段：基准序号取「最早的那个」，靠回绕安全的比较，
            // 不能直接用 BTreeMap 的最小键，否则序号跨过 u32 上界时会取错。
            if self.pending.is_empty() || Self::seq_before(frame.seq, self.next_seq) {
                self.next_seq = frame.seq;
            }
            if self.pending.contains_key(&frame.seq) {
                self.stats.duplicate += 1;
                return false;
            }
        }

        self.pending.insert(frame.seq, frame);
        true
    }

    /// 取出下一帧用于播放。
    ///
    /// 返回 None 时调用方应当补静音。预滚阶段的 None 不算欠载，
    /// 开始出帧之后的 None 才计入欠载统计——这直接驱动水位自适应。
    pub fn pop(&mut self) -> Option<Frame> {
        if self.pending.is_empty() {
            self.note_starvation();
            return None;
        }
        if (self.pending.len() as u32) < self.target_frames {
            self.note_starvation();
            return None;
        }

        match self.pending.remove(&self.next_seq) {
            Some(frame) => {
                self.playing = true;
                self.next_seq = self.next_seq.wrapping_add(1);
                self.healthy = self.healthy.saturating_add(1);
                self.recent_underruns = 0;
                Some(frame)
            }
            None => {
                // 该序号永远到不了了，跳过去继续
                self.stats.lost += 1;
                self.next_seq = self.next_seq.wrapping_add(1);
                self.note_starvation();
                None
            }
        }
    }

    /// 记一次「该出帧却没出成」。预滚阶段不计。
    fn note_starvation(&mut self) {
        if !self.playing {
            return;
        }
        self.stats.underruns += 1;
        self.recent_underruns = self.recent_underruns.saturating_add(1);
        self.healthy = 0;
    }

    /// 根据网络状况调整水位。每轮出帧后调用一次即可。
    ///
    /// - 连续欠载时抬高水位（网络抖动变大）
    /// - 持续充足时缓慢降低水位（把延迟还回去）
    /// - 不自适应（低延迟模式）时不做事
    pub fn adapt(&mut self) {
        if !self.is_adaptive() {
            return;
        }

        const UNDERRUN_THRESHOLD: u32 = 2;
        const HEALTHY_THRESHOLD: u32 = 250;

        if self.recent_underruns >= UNDERRUN_THRESHOLD {
            if self.target_frames < self.max_frames {
                self.target_frames += 1;
            }
            self.recent_underruns = 0;
        } else if self.healthy >= HEALTHY_THRESHOLD {
            if self.target_frames > self.min_frames {
                self.target_frames -= 1;
            }
            self.healthy = 0;
        }
    }

    /// 序号比较，处理 u32 回绕。
    fn seq_before(a: u32, b: u32) -> bool {
        a.wrapping_sub(b) > u32::MAX / 2
    }
}
