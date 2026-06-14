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

/// The operator's `hide` list, or the plugin's defaults when they set none —
/// how Glances ships sensible defaults (hiding loop/docker/boot &c.) that an
/// explicit `hide` in the config replaces.
pub fn hide_or_default(user_hide: Vec<String>, defaults: &[&str]) -> Vec<String> {
    if user_hide.is_empty() {
        defaults.iter().map(|s| (*s).to_string()).collect()
    } else {
        user_hide
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

    #[test]
    fn hide_or_default_uses_defaults_only_when_user_sets_none() {
        assert_eq!(hide_or_default(vec![], &["loop.*"]), ["loop.*"]);
        assert_eq!(hide_or_default(vec!["^sr".into()], &["loop.*"]), ["^sr"]);
    }
}
