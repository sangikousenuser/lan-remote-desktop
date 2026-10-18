//! Conservative hysteresis. Measurements are local estimates, not input-to-photon
//! latency or a claim about the winning transport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub fps: u32,
    pub bitrate: u32,
}
pub struct Controller {
    target: Target,
    maximum: Target,
    bad: u8,
    good: u8,
}
impl Controller {
    pub fn new(max_fps: u32, pixels: u64) -> Self {
        let maximum = Target {
            fps: max_fps.clamp(1, 60),
            bitrate: ((pixels * 6).clamp(8_000_000, 40_000_000)) as u32,
        };
        Self {
            target: maximum,
            maximum,
            bad: 0,
            good: 0,
        }
    }
    pub fn target(&self) -> Target {
        self.target
    }
    pub fn observe(&mut self, encode_ms: f64, rtt_ms: f64, queue_stalls: u64) -> Option<Target> {
        if !encode_ms.is_finite() || !rtt_ms.is_finite() {
            return None;
        }
        let overloaded =
            encode_ms > 800.0 / self.target.fps as f64 || rtt_ms > 80.0 || queue_stalls > 0;
        if overloaded {
            self.bad = self.bad.saturating_add(1);
            self.good = 0;
        } else {
            self.good = self.good.saturating_add(1);
            self.bad = 0;
        }
        let old = self.target;
        if self.bad >= 2 {
            self.target.fps = (self.target.fps / 2).max(15).min(self.maximum.fps);
            self.target.bitrate = (self.target.bitrate * 3 / 4).max(2_000_000);
            self.bad = 0;
        }
        if self.good >= 5 && rtt_ms < 30.0 && encode_ms < 500.0 / self.maximum.fps as f64 {
            self.target.fps = (self.target.fps * 2).min(self.maximum.fps);
            self.target.bitrate = (self.target.bitrate * 11 / 10).min(self.maximum.bitrate);
            self.good = 0;
        }
        (old != self.target).then_some(self.target)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sustained_load_and_recovery_have_hysteresis() {
        let mut c = Controller::new(60, 1920 * 1080);
        assert!(c.observe(30., 5., 0).is_none());
        assert_eq!(c.observe(30., 5., 0).unwrap().fps, 30);
        assert!(c.observe(2., 5., 0).is_none());
        for _ in 0..3 {
            assert!(c.observe(2., 5., 0).is_none());
        }
        assert_eq!(c.observe(2., 5., 0).unwrap().fps, 60);
        for _ in 0..30 {
            c.observe(300., 500., 10);
        }
        assert_eq!(c.target.fps, 15);
        assert!(c.target.bitrate >= 2_000_000);
    }
}
