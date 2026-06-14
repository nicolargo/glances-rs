//! Shared `show`/`hide` regex filter on a collection plugin's primary key
//! (ARCHITECTURE.md §8.1): network (interface name), fs (mount point) and
//! diskio (disk name) all filter their items the same way.
//!
//! `hide` wins over `show`; an empty `show` list means "everything". Patterns
//! are compiled once at construction — they were already validated at config
//! load (`Config::validate`), so a plugin only ever sees compilable ones.

use regex_lite::Regex;

pub struct KeyFilter {
    show: Vec<Regex>,
    hide: Vec<Regex>,
}

impl KeyFilter {
    pub fn new(show: &[String], hide: &[String]) -> Self {
        let compile = |patterns: &[String]| {
            patterns
                .iter()
                .map(|p| Regex::new(p).expect("regex validated by Config::validate"))
                .collect()
        };
        Self {
            show: compile(show),
            hide: compile(hide),
        }
    }

    /// Whether an item with this primary key should appear in the output.
    pub fn shown(&self, key: &str) -> bool {
        let shown = self.show.is_empty() || self.show.iter().any(|re| re.is_match(key));
        shown && !self.hide.iter().any(|re| re.is_match(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_lists_show_everything() {
        let all = KeyFilter::new(&[], &[]);
        assert!(all.shown("lo"));
        assert!(all.shown("eth0"));
    }

    #[test]
    fn hide_removes_matching_keys() {
        let no_lo = KeyFilter::new(&[], &["^lo$".into()]);
        assert!(!no_lo.shown("lo"));
        assert!(no_lo.shown("eth0"));
    }

    #[test]
    fn hide_wins_over_show() {
        let eth_only = KeyFilter::new(&["^eth".into()], &["^eth1$".into()]);
        assert!(eth_only.shown("eth0"));
        assert!(!eth_only.shown("eth1"));
        assert!(!eth_only.shown("wlan0"));
        assert!(!eth_only.shown("lo"));
    }
}
