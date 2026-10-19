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
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feedback {
    pub epoch: u64,
    pub interval_ms: u32,
    pub decoded: u32,
    pub presented: u32,
    pub decode_ms: f64,
    pub render_ms: f64,
    pub dropped: u32,
    pub errors: u32,
    pub visible: bool,
}
impl Feedback {
    pub fn valid(&self) -> bool {
        self.epoch != 0
            && (100..=10000).contains(&self.interval_ms)
            && self.decoded <= 2400
            && self.presented <= 2400
            && self.dropped <= 10000
            && self.errors <= 10000
            && [self.decode_ms, self.render_ms]
                .iter()
                .all(|v| v.is_finite() && (0.0..=10000.0).contains(v))
    }
}
/// Sample only a bounded grid in memory. Frame contents are never logged.
pub fn changed_fraction(previous: &[u8], next: &[u8]) -> f64 {
    if previous.len() != next.len() || next.is_empty() {
        return 1.;
    }
    let pixels = next.len() / 4;
    let stride = pixels.div_ceil(4096).max(1);
    let mut changed = 0u32;
    let mut sampled = 0u32;
    for pixel in (0..pixels).step_by(stride) {
        let at = pixel * 4;
        sampled += 1;
        if (0..3).any(|channel| previous[at + channel].abs_diff(next[at + channel]) > 12) {
            changed += 1;
        }
    }
    changed as f64 / sampled.max(1) as f64
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
        self.observe_pipeline(encode_ms, rtt_ms, queue_stalls, None, f64::NAN, 0.)
    }
    pub fn observe_pipeline(
        &mut self,
        encode_ms: f64,
        rtt_ms: f64,
        queue_stalls: u64,
        feedback: Option<Feedback>,
        change: f64,
        input_hz: f64,
    ) -> Option<Target> {
        if !encode_ms.is_finite() || !rtt_ms.is_finite() {
            return None;
        }
        let remote_overloaded = feedback
            .filter(|f| f.valid() && f.visible)
            .is_some_and(|f| {
                let losses = f.dropped.saturating_add(f.errors);
                let loss_fraction =
                    losses as f64 / (f.decoded.saturating_add(losses)).max(1) as f64;
                f.decode_ms.max(f.render_ms) > 1200. / self.target.fps as f64
                    || (f.decoded >= 8 && f.presented < f.decoded / 2)
                    || loss_fraction > 0.05
            });
        let overloaded = encode_ms > 800.0 / self.target.fps as f64
            || rtt_ms > 80.0
            || queue_stalls > 0
            || remote_overloaded;
        if overloaded {
            self.bad = self.bad.saturating_add(1);
            self.good = 0;
        } else {
            self.good = self.good.saturating_add(1);
            self.bad = 0;
        }
        let old = self.target;
        let active = change.is_finite() && change >= 0.02 || input_hz.is_finite() && input_hz >= 5.;
        if self.good >= 2 && active && rtt_ms < 30. && encode_ms < 500. / self.maximum.fps as f64 {
            self.target.fps = (self.target.fps * 2).min(self.maximum.fps);
        }
        if self.bad >= 2 {
            self.target.fps = (self.target.fps / 2).max(15).min(self.maximum.fps);
            self.target.bitrate = (self.target.bitrate * 3 / 4).max(2_000_000);
            self.bad = 0;
        }
        if self.good >= 5 && rtt_ms < 30.0 && encode_ms < 500.0 / self.maximum.fps as f64 {
            let preferred = if change.is_finite() && change < 0.02 && input_hz < 5. {
                self.maximum.fps.min(30)
            } else {
                self.maximum.fps
            };
            self.target.fps = (self.target.fps * 2).min(preferred);
            self.target.bitrate = (self.target.bitrate * 11 / 10).min(self.maximum.bitrate);
            self.good = 0;
        }
        (old != self.target).then_some(self.target)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn feedback() -> Feedback {
        Feedback {
            epoch: 1,
            interval_ms: 500,
            decoded: 30,
            presented: 30,
            decode_ms: 2.,
            render_ms: 2.,
            dropped: 0,
            errors: 0,
            visible: true,
        }
    }
    #[test]
    fn receiver_overload_and_loss_reduce_quality_without_host_overload() {
        let mut c = Controller::new(60, 1920 * 1080);
        let mut f = feedback();
        f.decode_ms = 30.;
        assert!(c.observe_pipeline(2., 5., 0, Some(f), 1., 0.).is_none());
        assert_eq!(
            c.observe_pipeline(2., 5., 0, Some(f), 1., 0.).unwrap().fps,
            30
        );
        f.decode_ms = 2.;
        f.dropped = 10;
        c.observe_pipeline(2., 5., 0, Some(f), 1., 0.);
        assert_eq!(
            c.observe_pipeline(2., 5., 0, Some(f), 1., 0.).unwrap().fps,
            15
        );
        f.visible = false;
        let mut c = Controller::new(60, 1920 * 1080);
        for _ in 0..10 {
            c.observe_pipeline(2., 5., 0, Some(f), 1., 0.);
        }
        assert_eq!(c.target().fps, 60);
    }
    #[test]
    fn text_preserves_bitrate_and_activity_restores_frame_rate() {
        let mut c = Controller::new(60, 1920 * 1080);
        let bitrate = c.target().bitrate;
        for _ in 0..5 {
            c.observe_pipeline(2., 5., 0, None, 0., 0.);
        }
        assert_eq!(c.target().fps, 30);
        assert_eq!(c.target().bitrate, bitrate);
        c.observe_pipeline(2., 5., 0, None, 0., 15.);
        assert_eq!(
            c.observe_pipeline(2., 5., 0, None, 0., 15.).unwrap().fps,
            60
        );
        let pixels = vec![0; 1920 * 1080 * 4];
        let other = vec![255; pixels.len()];
        assert_eq!(changed_fraction(&pixels, &pixels), 0.);
        assert_eq!(changed_fraction(&pixels, &other), 1.);
        let mut f = feedback();
        f.decode_ms = f64::NAN;
        assert!(!f.valid());
        f = feedback();
        f.interval_ms = 0;
        assert!(!f.valid());
    }
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
