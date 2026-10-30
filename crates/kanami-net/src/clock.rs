//! 端到端延迟测量。
//!
//! 两端的单调时钟起点不同，直接相减得到的数字毫无意义。
//! 这里用 Ping-Pong 交换四个时间戳估算时钟偏移（NTP 的简化版），
//! 然后才能把「发送端采集时刻」换算到本机时间轴上，算出真实延迟。
//!
//! 精度取决于链路对称性：局域网下往返时间对称，误差通常在 1ms 内。

#![forbid(unsafe_code)]

/// 一次往返测量得到的时钟偏移估计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetSample {
    /// 本机时钟 - 对端时钟（毫秒）。加上它可以把对端时间戳换算成本机时间。
    pub offset_ms: i64,
    /// 往返时间（毫秒）。越小说明这次测量越可信。
    pub rtt_ms: u32,
}

impl OffsetSample {
    /// 用四次时间戳计算偏移。
    ///
    /// - t1：本机发出请求的时刻
    /// - t2：对端收到请求的时刻（对端时钟）
    /// - t3：对端发出应答的时刻（对端时钟）
    /// - t4：本机收到应答的时刻
    ///
    /// 假设去程与回程耗时相同，则 offset = ((t2 - t1) + (t3 - t4)) / 2。
    pub fn from_ping_pong(t1: i64, t2: i64, t3: i64, t4: i64) -> Self {
        let offset = ((t2 - t1) + (t3 - t4)) / 2;
        let rtt = (t4 - t1) - (t3 - t2);
        Self { offset_ms: offset, rtt_ms: rtt.max(0) as u32 }
    }
}

/// 时钟同步器。多次采样后取往返最快的那个。
///
/// 只取一次样本容易被调度抖动带偏；往返时间最短的那次
/// 说明路径最干净，它的偏移估计也最可信。
#[derive(Debug, Default)]
pub struct ClockSync {
    samples: Vec<OffsetSample>,
    best: Option<OffsetSample>,
}

impl ClockSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// 加入一次测量结果。
    pub fn add(&mut self, sample: OffsetSample) {
        self.samples.push(sample);
        // 只保留最近 16 次，长时间运行不会无限增长
        if self.samples.len() > 16 {
            self.samples.remove(0);
        }
        self.best = self.samples.iter().min_by_key(|s| s.rtt_ms).copied();
    }

    /// 当前最优偏移。没有样本时返回 None。
    pub fn offset_ms(&self) -> Option<i64> {
        self.best.map(|s| s.offset_ms)
    }

    /// 当前估计的往返时间。
    pub fn rtt_ms(&self) -> Option<u32> {
        self.best.map(|s| s.rtt_ms)
    }

    /// 是否已同步（至少有一次样本）。
    pub fn is_synced(&self) -> bool {
        self.best.is_some()
    }

    /// 把对端时间戳换算成本机时间轴。
    pub fn to_local(&self, remote_ms: i64) -> Option<i64> {
        self.offset_ms().map(|o| remote_ms + o)
    }

    /// 计算端到端延迟：本机现在 - 对端采集时刻（已换算到本机时间轴）。
    ///
    /// 结果包含网络传输 + 抖动缓冲 + 解码的时间，
    /// 不包含最后写进声卡的那一小段（那由音频 API 的缓冲决定）。
    pub fn latency_ms(&self, now_local_ms: i64, remote_capture_ms: i64) -> Option<i64> {
        Some(now_local_ms - self.to_local(remote_capture_ms)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 理想对称链路下偏移准确() {
        // 对端时钟比本机快 1000ms，往返各 5ms
        let t1 = 100;
        let t2 = 1105; // 对端收到时，对端时钟 1105
        let t3 = 1105;
        let t4 = 110; // 本机收到
        let s = OffsetSample::from_ping_pong(t1, t2, t3, t4);
        assert_eq!(s.offset_ms, 1000, "偏移应当等于两端时钟差");
        assert_eq!(s.rtt_ms, 10);
    }

    #[test]
    fn 单次采样偏移换算正确() {
        let mut sync = ClockSync::new();
        sync.add(OffsetSample::from_ping_pong(100, 1105, 1105, 110));
        assert!(sync.is_synced());
        assert_eq!(sync.to_local(2000), Some(3000), "对端时间 + 偏移 = 本机时间");
    }

    #[test]
    fn 端到端延迟计算() {
        let mut sync = ClockSync::new();
        sync.add(OffsetSample::from_ping_pong(100, 1105, 1105, 110));

        // 对端在它自己的 5000ms 采集，本机现在是 6100ms
        // 换算：5000 + 1000 = 6000，延迟 = 6100 - 6000 = 100ms
        assert_eq!(sync.latency_ms(6100, 5000), Some(100));
    }

    #[test]
    fn 优先采用往返最快的样本() {
        let mut sync = ClockSync::new();
        // 第一个样本往返 50ms，抖动大
        sync.add(OffsetSample::from_ping_pong(0, 130, 130, 50));
        // 第二个样本往返 4ms，更可信
        sync.add(OffsetSample::from_ping_pong(100, 2100, 2100, 104));

        assert_eq!(sync.rtt_ms(), Some(4), "应当选往返最短的那次");
        assert_eq!(sync.offset_ms(), Some(1998));
    }

    #[test]
    fn 未同步时不返回延迟() {
        let sync = ClockSync::new();
        assert!(!sync.is_synced());
        assert_eq!(sync.latency_ms(1000, 500), None);
        assert_eq!(sync.to_local(500), None);
    }

    #[test]
    fn 样本数量有上限() {
        let mut sync = ClockSync::new();
        for i in 0..100i64 {
            sync.add(OffsetSample::from_ping_pong(i, i + 10, i + 10, i + 4));
        }
        assert!(sync.samples.len() <= 16, "长期运行不能无限堆积样本");
    }
}
