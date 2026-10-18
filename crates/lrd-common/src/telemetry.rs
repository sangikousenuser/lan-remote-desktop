use std::collections::VecDeque;

/// Bounded local numeric samples, with no content, keys or peer identifiers.
#[derive(Debug)]
pub struct Latencies {
    samples_us: VecDeque<u64>,
    capacity: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Percentiles {
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub samples: usize,
}

impl Latencies {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples_us: VecDeque::new(),
            capacity: capacity.clamp(1, 60_000),
        }
    }
    pub fn record(&mut self, microseconds: u64) {
        if self.samples_us.len() == self.capacity {
            self.samples_us.pop_front();
        }
        self.samples_us.push_back(microseconds);
    }
    pub fn percentiles(&self) -> Option<Percentiles> {
        if self.samples_us.is_empty() {
            return None;
        }
        let mut values: Vec<_> = self.samples_us.iter().copied().collect();
        values.sort_unstable();
        let percentile = |p: usize| values[(values.len() * p).div_ceil(100) - 1];
        Some(Percentiles {
            p50_us: percentile(50),
            p95_us: percentile(95),
            p99_us: percentile(99),
            samples: values.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn percentiles_and_eviction() {
        let mut l = Latencies::new(100);
        assert_eq!(l.percentiles(), None);
        for n in 1..=100 {
            l.record(n);
        }
        assert_eq!(
            l.percentiles(),
            Some(Percentiles {
                p50_us: 50,
                p95_us: 95,
                p99_us: 99,
                samples: 100
            })
        );
        for _ in 0..100 {
            l.record(42);
        }
        assert_eq!(l.percentiles().unwrap().p99_us, 42);
    }
}
