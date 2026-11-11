//! Domain routing rules: chain, direct or block.

use crate::config::Rule;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Chain,
    Direct,
    Block,
}

impl Action {
    pub fn parse(value: &str) -> Action {
        match value {
            "direct" => Action::Direct,
            "block" => Action::Block,
            _ => Action::Chain,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Router {
    rules: Vec<(String, Action)>,
}

impl Router {
    pub fn new(rules: &[Rule]) -> Router {
        Router {
            rules: rules
                .iter()
                .map(|r| (r.domain.to_lowercase(), Action::parse(&r.action)))
                .collect(),
        }
    }

    pub fn action_for(&self, host: &str) -> Action {
        let host = host.trim_end_matches('.').to_lowercase();
        for (pattern, action) in &self.rules {
            if match_domain(pattern, &host) {
                return *action;
            }
        }
        Action::Chain
    }
}

pub fn match_domain(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim_end_matches('.').to_lowercase();
    if pattern.is_empty() || host.is_empty() {
        return false;
    }
    if pattern == "*" {
        return true;
    }
    if pattern.contains('*') || pattern.contains('?') {
        // "*.example.com" is a subdomain rule; anything with a further wildcard
        // ("*.tracker.*") is a plain glob and must not be read as a suffix.
        if let Some(suffix) = pattern.strip_prefix("*.") {
            if !suffix.contains('*') && !suffix.contains('?') {
                return host == suffix || host.ends_with(&format!(".{suffix}"));
            }
        }
        return wildcard_match(&pattern, host);
    }
    host == pattern || host.ends_with(&format!(".{pattern}"))
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(star_pos) = star {
            pi = star_pos + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Router {
        Router::new(&[
            Rule { domain: "*.bank.example".into(), action: "direct".into() },
            Rule { domain: "blocked.example".into(), action: "block".into() },
            Rule { domain: "*.tracker.*".into(), action: "block".into() },
        ])
    }

    #[test]
    fn bare_domain_covers_subdomains() {
        assert_eq!(router().action_for("api.bank.example"), Action::Direct);
        assert_eq!(router().action_for("bank.example"), Action::Direct);
        assert_eq!(router().action_for("sub.blocked.example"), Action::Block);
        assert_eq!(router().action_for("news.example.org"), Action::Chain);
    }

    #[test]
    fn suffix_confusion_is_rejected() {
        assert!(!match_domain("example.com", "notexample.com"));
        assert!(!match_domain("example.com", "example.com.evil.net"));
        assert!(!match_domain("*.example.com", "example.com.evil.net"));
    }

    #[test]
    fn wildcards() {
        assert!(match_domain("*.tracker.*", "a.tracker.net"));
        assert!(match_domain("ads-*.net", "ads-42.net"));
        assert!(!match_domain("ads-*.net", "ads.example.net"));
        assert!(match_domain("a?c.com", "abc.com"));
        assert!(match_domain("a?c.com", "aac.com")); // ? is exactly one char
        assert!(!match_domain("a?c.com", "abbc.com"));
    }
}
