use std::collections::VecDeque;

/// The last `capacity` values of one metric, oldest first. Graphs read
/// the tail that fits their width.
#[derive(Debug, Clone)]
pub struct History {
    values: VecDeque<f32>,
    capacity: usize,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self {
            values: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, value: f32) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }

    /// The newest `count` values, oldest first. Fewer if the history
    /// hasn't filled up yet.
    pub fn tail(&self, count: usize) -> impl Iterator<Item = f32> + '_ {
        self.values
            .iter()
            .skip(self.values.len().saturating_sub(count))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_oldest_once_full() {
        let mut history = History::new(3);
        for value in [1.0, 2.0, 3.0, 4.0] {
            history.push(value);
        }
        assert_eq!(history.tail(10).collect::<Vec<_>>(), [2.0, 3.0, 4.0]);
    }

    #[test]
    fn tail_returns_newest_in_order() {
        let mut history = History::new(10);
        for value in [1.0, 2.0, 3.0] {
            history.push(value);
        }
        assert_eq!(history.tail(2).collect::<Vec<_>>(), [2.0, 3.0]);
    }
}
