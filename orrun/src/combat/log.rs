//! 8-line always-hit combat log. Newest at the bottom.

use std::collections::VecDeque;

const CAP: usize = 8;

#[derive(Clone, Debug, Default)]
pub struct CombatLog {
    lines: VecDeque<String>,
}

impl CombatLog {
    pub fn new() -> Self {
        Self {
            lines: VecDeque::with_capacity(CAP),
        }
    }

    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push_back(line.into());
        while self.lines.len() > CAP {
            self.lines.pop_front();
        }
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_last_eight_newest_last() {
        let mut log = CombatLog::new();
        for i in 1..=10 {
            log.push(format!("line {i}"));
        }
        let got: Vec<_> = log.lines().collect();
        assert_eq!(got, ["line 3", "line 4", "line 5", "line 6", "line 7", "line 8", "line 9", "line 10"]);
    }
}
