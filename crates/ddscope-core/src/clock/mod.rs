//! Three-tier hybrid clock synchronization and second-order software PLL contracts.

use serde::{Deserialize, Serialize};

/// Clock reconstruction strategy configured per data source.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum ClockMode {
    /// Tier 1: Hardware counter embedded in protocol frames, anchored to `host_mono_ns`.
    Tier1HardwareCounter { tick_hz: f64, counter_bits: u8 },
    /// Tier 2: Second-order software PLL reconstructing uniform sample spacing across bursty packets.
    Tier2SoftwarePll { nominal_sample_rate_hz: Option<f64> },
    /// Tier 3: Direct monotonic host arrival timestamps for asynchronous events.
    #[default]
    Tier3HostArrival,
}
