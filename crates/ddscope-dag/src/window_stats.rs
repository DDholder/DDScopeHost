//! O(1) amortized Two-Stack Sliding Window Aggregation Queue (SWAG) with micro-bucket coalescing.

use ddscope_types::TimestampNs;

use super::config::WindowMetricKind;

#[derive(Copy, Clone, Debug)]
struct Agg {
    count: u64,
    mean: f64,
    m2: f64,
    min: f64,
    max: f64,
}

impl Agg {
    #[inline]
    fn single(val: f64) -> Self {
        Self {
            count: 1,
            mean: val,
            m2: 0.0,
            min: val,
            max: val,
        }
    }

    #[inline]
    fn push(&mut self, val: f64) {
        self.count += 1;
        let delta = val - self.mean;
        self.mean += delta / (self.count as f64);
        let delta2 = val - self.mean;
        self.m2 += delta * delta2;
        if val < self.min {
            self.min = val;
        }
        if val > self.max {
            self.max = val;
        }
    }

    #[inline]
    fn combine(a: Self, b: Self) -> Self {
        if a.count == 0 {
            return b;
        }
        if b.count == 0 {
            return a;
        }
        let n = a.count + b.count;
        let delta = b.mean - a.mean;
        let n_a = a.count as f64;
        let n_b = b.count as f64;
        let n_f = n as f64;
        Self {
            count: n,
            mean: a.mean + delta * (n_b / n_f),
            m2: a.m2 + b.m2 + delta * delta * (n_a * n_b / n_f),
            min: a.min.min(b.min),
            max: a.max.max(b.max),
        }
    }

    #[inline]
    fn metric(self, kind: WindowMetricKind) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        match kind {
            WindowMetricKind::Mean => self.mean,
            WindowMetricKind::Min => self.min,
            WindowMetricKind::Max => self.max,
            WindowMetricKind::PeakToPeak => (self.max - self.min).max(0.0),
            WindowMetricKind::StdDev => (self.m2 / (self.count as f64)).max(0.0).sqrt(),
            WindowMetricKind::Rms => {
                let var = (self.m2 / (self.count as f64)).max(0.0);
                (var + self.mean * self.mean).sqrt()
            }
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Bucket {
    bucket_id: u64,
    end_ts_ns: TimestampNs,
    agg: Agg,
}

#[derive(Copy, Clone, Debug)]
struct StackItem {
    bucket: Bucket,
    prefix_or_suffix_agg: Agg,
}

/// Bounded-memory sliding time-window statistic evaluator in `f64` using Chan's parallel variance monoid.
#[derive(Clone, Debug)]
pub struct SwagWindowTracker {
    window_ns: u64,
    bucket_span_ns: u64,
    front_stack: Vec<StackItem>,
    back_stack: Vec<StackItem>,
    current: Option<Bucket>,
}

impl SwagWindowTracker {
    /// Creates a new sliding window tracker for `window_ms` milliseconds.
    pub fn new(window_ms: f64) -> Self {
        let window_ns = (window_ms.max(1e-6) * 1_000_000.0).round().max(1.0) as u64;
        let bucket_span_ns = (window_ns / 4096).max(1);
        Self {
            window_ns,
            bucket_span_ns,
            front_stack: Vec::with_capacity(64),
            back_stack: Vec::with_capacity(64),
            current: None,
        }
    }

    /// Resets all accumulated window state.
    pub fn reset(&mut self) {
        self.front_stack.clear();
        self.back_stack.clear();
        self.current = None;
    }

    #[inline]
    fn push_bucket(&mut self, bucket: Bucket) {
        let agg = match self.back_stack.last() {
            Some(top) => Agg::combine(top.prefix_or_suffix_agg, bucket.agg),
            None => bucket.agg,
        };
        self.back_stack.push(StackItem {
            bucket,
            prefix_or_suffix_agg: agg,
        });
    }

    #[inline]
    fn rebalance_if_needed(&mut self) {
        if self.front_stack.is_empty() {
            while let Some(item) = self.back_stack.pop() {
                let agg = match self.front_stack.last() {
                    Some(top) => Agg::combine(item.bucket.agg, top.prefix_or_suffix_agg),
                    None => item.bucket.agg,
                };
                self.front_stack.push(StackItem {
                    bucket: item.bucket,
                    prefix_or_suffix_agg: agg,
                });
            }
        }
    }

    #[inline]
    fn evict_expired(&mut self, now_ts_ns: TimestampNs) {
        loop {
            self.rebalance_if_needed();
            let Some(top) = self.front_stack.last() else {
                break;
            };
            if now_ts_ns.saturating_sub(top.bucket.end_ts_ns) >= self.window_ns {
                self.front_stack.pop();
            } else {
                break;
            }
        }
    }

    /// Pushes `(ts_ns, val)` into the sliding window and returns the updated `metric` in `f64`.
    #[inline]
    pub fn push_and_eval(
        &mut self,
        ts_ns: TimestampNs,
        val: f64,
        metric: WindowMetricKind,
    ) -> f64 {
        let safe_val = if val.is_finite() { val } else { 0.0 };
        let bucket_id = ts_ns / self.bucket_span_ns;

        match &mut self.current {
            Some(cur) if cur.bucket_id == bucket_id => {
                cur.end_ts_ns = cur.end_ts_ns.max(ts_ns);
                cur.agg.push(safe_val);
            }
            Some(cur) => {
                let finished = *cur;
                *cur = Bucket {
                    bucket_id,
                    end_ts_ns: ts_ns,
                    agg: Agg::single(safe_val),
                };
                self.push_bucket(finished);
            }
            None => {
                self.current = Some(Bucket {
                    bucket_id,
                    end_ts_ns: ts_ns,
                    agg: Agg::single(safe_val),
                });
            }
        }

        self.evict_expired(ts_ns);

        let mut total: Option<Agg> = None;
        if let Some(f) = self.front_stack.last() {
            total = Some(f.prefix_or_suffix_agg);
        }
        if let Some(b) = self.back_stack.last() {
            total = Some(match total {
                Some(prev) => Agg::combine(prev, b.prefix_or_suffix_agg),
                None => b.prefix_or_suffix_agg,
            });
        }
        if let Some(cur) = self.current {
            total = Some(match total {
                Some(prev) => Agg::combine(prev, cur.agg),
                None => cur.agg,
            });
        }

        let res = total.map_or(0.0, |agg| agg.metric(metric));
        if res.is_finite() {
            res
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn swag_rms_mean_vpp_are_exact_and_drift_free() {
        let mut rms_tracker = SwagWindowTracker::new(20.0);
        let mut mean_tracker = SwagWindowTracker::new(20.0);
        let mut vpp_tracker = SwagWindowTracker::new(20.0);

        let fs = 10_000.0; // 10 kHz -> 200 samples per 20ms cycle
        let dt_ns = 100_000_u64;
        let amp = 100.0;
        let dc = 25.0;

        let mut last_rms = 0.0;
        let mut last_mean = 0.0;
        let mut last_vpp = 0.0;

        for k in 0..200_000_u64 {
            let t_sec = (k as f64) / fs;
            let val = dc + amp * (2.0 * PI * 50.0 * t_sec).sin();
            let ts = k * dt_ns;
            last_rms = rms_tracker.push_and_eval(ts, val, WindowMetricKind::Rms);
            last_mean = mean_tracker.push_and_eval(ts, val, WindowMetricKind::Mean);
            last_vpp = vpp_tracker.push_and_eval(ts, val, WindowMetricKind::PeakToPeak);
        }

        let expected_rms = (dc * dc + 0.5 * amp * amp).sqrt();
        assert!(
            (last_rms - expected_rms).abs() < 0.05,
            "expected rms {expected_rms}, got {last_rms}"
        );
        assert!(
            (last_mean - dc).abs() < 0.05,
            "expected mean {dc}, got {last_mean}"
        );
        assert!(
            (last_vpp - 2.0 * amp).abs() < 0.1,
            "expected vpp {}, got {last_vpp}",
            2.0 * amp
        );
    }
}
