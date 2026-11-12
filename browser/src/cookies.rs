//! Cookies, the way Tor Browser keeps them: in memory only, gone when the
//! browser quits or the burn button is pressed, and partitioned by the site
//! you are on (like Firefox's Total Cookie Protection): a tracker embedded in
//! two sites gets two unrelated jars, so it cannot follow you across them.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::url::Url;

#[derive(Debug, Clone)]
struct Cookie {
    /// the top-level site this cookie was set under
    partition: String,
    name: String,
    value: String,
    /// host it belongs to; `host_only` = exactly that host, else it and its subdomains
    domain: String,
    host_only: bool,
    path: String,
    secure: bool,
    http_only: bool,
    expires: Option<Instant>,
}

static JAR: OnceLock<Mutex<Vec<Cookie>>> = OnceLock::new();
const MAX_COOKIES: usize = 3000;

fn jar() -> std::sync::MutexGuard<'static, Vec<Cookie>> {
    JAR.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The registrable part of a host, roughly (eTLD+1 with the common
/// two-level suffixes): what a partition is keyed by.
pub fn site_of(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() <= 2 {
        return host;
    }
    let last_two = format!("{}.{}", labels[labels.len() - 2], labels[labels.len() - 1]);
    const TWO_LEVEL: &[&str] = &["co.uk", "org.uk", "ac.uk", "gov.uk", "com.au", "net.au", "org.au", "co.jp", "ne.jp", "or.jp", "com.br", "com.cn", "com.tr", "co.nz", "co.za", "co.in", "co.kr", "com.mx", "com.ar", "com.ua", "com.ru", "msk.ru", "spb.ru", "github.io", "gitlab.io", "pages.dev", "vercel.app", "netlify.app", "herokuapp.com", "blogspot.com", "appspot.com"];
    if TWO_LEVEL.contains(&last_two.as_str()) {
        return format!("{}.{}", labels[labels.len() - 3], last_two);
    }
    last_two
}

fn domain_matches(cookie: &Cookie, host: &str) -> bool {
    if cookie.host_only {
        return cookie.domain == host;
    }
    host == cookie.domain || host.ends_with(&format!(".{}", cookie.domain))
}

fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    request_path == cookie_path
        || (request_path.starts_with(cookie_path) && (cookie_path.ends_with('/') || request_path[cookie_path.len()..].starts_with('/')))
}

fn live(cookie: &Cookie) -> bool {
    cookie.expires.is_none_or(|t| t > Instant::now())
}

/// The `Cookie:` header for a request to `url` from a page on `site`.
pub fn header_for(site: &str, url: &Url) -> Option<String> {
    cookies_for(site, url, true)
}

/// What `document.cookie` shows (no HttpOnly cookies).
pub fn script_view(site: &str, url: &Url) -> String {
    cookies_for(site, url, false).unwrap_or_default()
}

fn cookies_for(site: &str, url: &Url, http: bool) -> Option<String> {
    let partition = site_of(site);
    let host = url.host.to_ascii_lowercase();
    let path = url.path.split('#').next().unwrap_or("/");
    let secure = url.scheme == "https";
    let mut jar = jar();
    jar.retain(live);
    let mut matching: Vec<&Cookie> = jar
        .iter()
        .filter(|c| c.partition == partition && domain_matches(c, &host) && path_matches(&c.path, path) && (!c.secure || secure) && (http || !c.http_only))
        .collect();
    // longer paths first, as RFC 6265 asks
    matching.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
    let line: Vec<String> = matching.iter().map(|c| format!("{}={}", c.name, c.value)).collect();
    (!line.is_empty()).then(|| line.join("; "))
}

/// Take a `Set-Cookie` value (or a `document.cookie` assignment when
/// `from_script`) for a response from `url` while on `site`.
pub fn store(site: &str, url: &Url, header: &str) {
    store_inner(site, url, header, false);
}

pub fn store_from_script(site: &str, url: &Url, assignment: &str) {
    store_inner(site, url, assignment, true);
}

fn store_inner(site: &str, url: &Url, header: &str, from_script: bool) {
    let mut parts = header.split(';');
    let Some((name, value)) = parts.next().and_then(|nv| nv.split_once('=')) else { return };
    let (name, value) = (name.trim().to_string(), value.trim().to_string());
    if name.is_empty() || name.len() + value.len() > 4096 {
        return;
    }
    let host = url.host.to_ascii_lowercase();
    let mut cookie = Cookie {
        partition: site_of(site),
        name,
        value,
        domain: host.clone(),
        host_only: true,
        path: default_path(&url.path),
        secure: false,
        http_only: false,
        expires: None,
    };
    let mut delete = false;
    for attribute in parts {
        let (key, val) = attribute.split_once('=').map(|(k, v)| (k.trim(), v.trim())).unwrap_or((attribute.trim(), ""));
        match key.to_ascii_lowercase().as_str() {
            "domain" => {
                let d = val.trim_start_matches('.').to_ascii_lowercase();
                // a cookie may only be set for the host itself or a parent of it, never a bare suffix
                if !d.is_empty() && (host == d || host.ends_with(&format!(".{d}"))) && d.contains('.') && site_of(&d) == site_of(&host) {
                    cookie.domain = d;
                    cookie.host_only = false;
                } else {
                    return;
                }
            }
            "path" if val.starts_with('/') => cookie.path = val.to_string(),
            "secure" => cookie.secure = true,
            "httponly" => {
                if from_script {
                    return;
                }
                cookie.http_only = true;
            }
            "max-age" => match val.parse::<i64>() {
                Ok(s) if s <= 0 => delete = true,
                // kept no longer than the run anyway; at most a day in memory
                Ok(s) => cookie.expires = Some(Instant::now() + Duration::from_secs((s as u64).min(86_400))),
                Err(_) => {}
            },
            "expires" if expired_date(val) => delete = true,
            _ => {}
        }
    }
    if cookie.secure && url.scheme != "https" {
        return;
    }
    let mut jar = jar();
    jar.retain(|c| !(c.partition == cookie.partition && c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path));
    if !delete {
        if jar.len() >= MAX_COOKIES {
            jar.remove(0);
        }
        jar.push(cookie);
    }
}

fn default_path(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or("/");
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => path[..i].to_string(),
    }
}

/// `Expires=` in the past (sites delete cookies this way). Only the year is
/// looked at: anything before 2000 counts as past.
fn expired_date(value: &str) -> bool {
    value.split(|c: char| !c.is_ascii_digit()).filter_map(|t| t.parse::<u32>().ok()).any(|n| (1970..2000).contains(&n))
}

/// Forget every cookie (the burn button).
pub fn forget() {
    jar().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn first_party_cookies_round_trip() {
        let site = "shop-a.test";
        store(site, &u("https://www.shop-a.test/login"), "sid=abc; Path=/; HttpOnly; Secure");
        store(site, &u("https://www.shop-a.test/login"), "theme=dark; Domain=shop-a.test");
        let header = header_for(site, &u("https://www.shop-a.test/account")).unwrap();
        assert!(header.contains("sid=abc") && header.contains("theme=dark"), "{header}");
        assert!(header_for(site, &u("http://www.shop-a.test/")).is_none_or(|h| !h.contains("sid")), "secure only over https");
        assert_eq!(script_view(site, &u("https://www.shop-a.test/")), "theme=dark", "HttpOnly is hidden from scripts");
        assert!(header_for(site, &u("https://api.shop-a.test/")).unwrap().contains("theme=dark"), "domain cookie reaches subdomains");
    }

    #[test]
    fn partitions_keep_trackers_apart() {
        store("news-b.test", &u("https://tracker.test/pixel"), "id=1");
        assert!(header_for("news-b.test", &u("https://tracker.test/x")).is_some());
        assert!(header_for("blog-c.test", &u("https://tracker.test/x")).is_none(), "another site sees another jar");
    }

    #[test]
    fn deletion_and_bad_domains() {
        let site = "del-d.test";
        store(site, &u("https://del-d.test/"), "a=1");
        store(site, &u("https://del-d.test/"), "a=1; Max-Age=0");
        assert!(header_for(site, &u("https://del-d.test/")).is_none());
        store(site, &u("https://del-d.test/"), "b=2; Domain=test");
        store(site, &u("https://del-d.test/"), "c=3; Domain=other.test");
        assert!(header_for(site, &u("https://del-d.test/")).is_none(), "no cookies for a suffix or a foreign domain");
        assert_eq!(site_of("a.b.example.co.uk"), "example.co.uk");
    }
}
