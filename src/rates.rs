//! How often the sampling loop reads the system.

use std::time::Duration;

/// Process list refresh intervals that `+` and `-` step through.
const INTERVALS_MS: [u64; 6] = [250, 500, 1000, 2000, 3000, 5000];
/// Graphs never sample faster than this, which stays above sysinfo's
/// 200 ms minimum between CPU readings.
const MIN_GRAPH_INTERVAL_MS: u64 = 250;
const MAX_GRAPHS_PER_REFRESH: u32 = 8;

/// The process list refreshes every [`Rates::process_interval`]; graphs
/// and meters sample [`Rates::graphs_per_refresh`] times in that
/// interval, since reading every process costs far more than everything
/// else together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates {
    /// Index into [`INTERVALS_MS`].
    process_step: usize,
    /// What the user asked for with `[` and `]`. Short process intervals
    /// can't fit that many graph samples, so it is clamped where used
    /// and comes back once the interval is long enough again.
    wanted_graphs_per_refresh: u32,
}

impl Rates {
    pub const DEFAULT: Self = Self {
        process_step: 2,
        wanted_graphs_per_refresh: 2,
    };

    pub fn process_interval(self) -> Duration {
        Duration::from_millis(INTERVALS_MS[self.process_step])
    }

    fn max_graphs_per_refresh(self) -> u32 {
        let fits = INTERVALS_MS[self.process_step] / MIN_GRAPH_INTERVAL_MS;
        (fits as u32).clamp(1, MAX_GRAPHS_PER_REFRESH)
    }

    pub fn graphs_per_refresh(self) -> u32 {
        self.wanted_graphs_per_refresh
            .min(self.max_graphs_per_refresh())
    }

    pub fn graph_interval(self) -> Duration {
        self.process_interval() / self.graphs_per_refresh()
    }

    pub fn step_process_interval(self, delta: isize) -> Self {
        Self {
            process_step: self
                .process_step
                .saturating_add_signed(delta)
                .min(INTERVALS_MS.len() - 1),
            ..self
        }
    }

    /// Steps from the count in effect, not the wanted one, so each press
    /// changes what is on screen.
    pub fn step_graphs_per_refresh(self, delta: i32) -> Self {
        Self {
            wanted_graphs_per_refresh: self
                .graphs_per_refresh()
                .saturating_add_signed(delta)
                .clamp(1, self.max_graphs_per_refresh()),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intervals(rates: Rates) -> (u128, u128) {
        (
            rates.process_interval().as_millis(),
            rates.graph_interval().as_millis(),
        )
    }

    #[test]
    fn graphs_sample_several_times_per_process_refresh() {
        assert_eq!(intervals(Rates::DEFAULT), (1000, 500));
        let more = Rates::DEFAULT.step_graphs_per_refresh(1);
        assert_eq!(intervals(more), (1000, 333));
        let slower = Rates::DEFAULT.step_process_interval(1);
        assert_eq!(intervals(slower), (2000, 1000));
    }

    #[test]
    fn graph_interval_never_drops_below_the_minimum() {
        // At 1 s, four graph samples already take 250 ms each.
        let capped = Rates::DEFAULT
            .step_graphs_per_refresh(1)
            .step_graphs_per_refresh(1)
            .step_graphs_per_refresh(1);
        assert_eq!(intervals(capped), (1000, 250));
        let fastest = Rates::DEFAULT.step_process_interval(-2);
        assert_eq!(intervals(fastest), (250, 250));
    }

    #[test]
    fn clamped_choice_comes_back_with_a_longer_interval() {
        let round_trip = Rates::DEFAULT
            .step_process_interval(-2)
            .step_process_interval(2);
        assert_eq!(intervals(round_trip), (1000, 500));
    }
}
