//! Who the browser says it is: a browser (User-Agent, `navigator`) and a
//! country (language, `Accept-Language`, Intl locale, time zone).
//!
//! The choice is one process-wide value, read when a request is built and
//! when a page's scripts start, so a change applies to the next load. It is
//! remembered in `~/.sepia/browser/settings.tsv` (two lines, nothing else).

use std::sync::RwLock;

/// A browser to present as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Browser {
    pub id: &'static str,
    /// Shown in the settings.
    pub label: &'static str,
    pub user_agent: &'static str,
    /// The `Accept` header of a top-level page load.
    pub accept: &'static str,
    /// `navigator.platform`
    pub platform: &'static str,
    /// `navigator.vendor`
    pub vendor: &'static str,
    /// `navigator.productSub`
    pub product_sub: &'static str,
    /// `navigator.appVersion`
    pub app_version: &'static str,
    /// `navigator.oscpu` (Firefox only)
    pub oscpu: Option<&'static str>,
    /// `navigator.userAgentData` (Chromium only): (brand, major version, platform)
    pub client_hints: Option<(&'static str, &'static str, &'static str)>,
}

const FIREFOX_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
// Chrome's, minus image/avif: images use the same header and AVIF does not decode here
const CHROME_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7";
const SAFARI_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

pub const BROWSERS: &[Browser] = &[
    Browser {
        id: "firefox-mac",
        label: "Firefox · macOS",
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:140.0) Gecko/20100101 Firefox/140.0",
        accept: FIREFOX_ACCEPT,
        platform: "MacIntel",
        vendor: "",
        product_sub: "20100101",
        app_version: "5.0 (Macintosh)",
        oscpu: Some("Intel Mac OS X 10.15"),
        client_hints: None,
    },
    Browser {
        id: "firefox-win",
        label: "Firefox · Windows",
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0",
        accept: FIREFOX_ACCEPT,
        platform: "Win32",
        vendor: "",
        product_sub: "20100101",
        app_version: "5.0 (Windows)",
        oscpu: Some("Windows NT 10.0; Win64; x64"),
        client_hints: None,
    },
    Browser {
        id: "chrome-mac",
        label: "Chrome · macOS",
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
        accept: CHROME_ACCEPT,
        platform: "MacIntel",
        vendor: "Google Inc.",
        product_sub: "20030107",
        app_version: "5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
        oscpu: None,
        client_hints: Some(("Google Chrome", "141", "macOS")),
    },
    Browser {
        id: "chrome-win",
        label: "Chrome · Windows",
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
        accept: CHROME_ACCEPT,
        platform: "Win32",
        vendor: "Google Inc.",
        product_sub: "20030107",
        app_version: "5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
        oscpu: None,
        client_hints: Some(("Google Chrome", "141", "Windows")),
    },
    Browser {
        id: "edge-win",
        label: "Edge · Windows",
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0",
        accept: CHROME_ACCEPT,
        platform: "Win32",
        vendor: "Google Inc.",
        product_sub: "20030107",
        app_version: "5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0",
        oscpu: None,
        client_hints: Some(("Microsoft Edge", "141", "Windows")),
    },
    Browser {
        id: "safari-mac",
        label: "Safari · macOS",
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.6 Safari/605.1.15",
        accept: SAFARI_ACCEPT,
        platform: "MacIntel",
        vendor: "Apple Computer, Inc.",
        product_sub: "20030107",
        app_version: "5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.6 Safari/605.1.15",
        oscpu: None,
        client_hints: None,
    },
    Browser {
        id: "sepia",
        label: "Sepia (как есть)",
        user_agent: "sepia-browser/0.1",
        accept: "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8",
        platform: "",
        vendor: "",
        product_sub: "20100101",
        app_version: "5.0",
        oscpu: None,
        client_hints: None,
    },
];

/// Daylight saving time rules a country's clock follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dst {
    None,
    /// EU: last Sunday of March 01:00 UTC to last Sunday of October 01:00 UTC
    Europe,
    /// US/Canada: second Sunday of March 02:00 local to first Sunday of November 02:00 local
    NorthAmerica,
    /// Australia (south-east): first Sunday of October 02:00 to first Sunday of April 03:00 local
    Australia,
}

/// A country to present as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Country {
    /// ISO 3166 code, the saved value.
    pub code: &'static str,
    /// Shown in the settings.
    pub label: &'static str,
    /// BCP 47 tag: `navigator.language` and the default Intl locale.
    pub language: &'static str,
    /// IANA time zone: `Intl.DateTimeFormat().resolvedOptions().timeZone`.
    pub time_zone: &'static str,
    /// Standard (winter) offset from UTC in minutes.
    pub offset_minutes: i32,
    pub dst: Dst,
}

const fn c(code: &'static str, label: &'static str, language: &'static str, time_zone: &'static str, offset_minutes: i32, dst: Dst) -> Country {
    Country { code, label, language, time_zone, offset_minutes, dst }
}

pub const COUNTRIES: &[Country] = &[
    c("US", "США", "en-US", "America/New_York", -300, Dst::NorthAmerica),
    c("GB", "Великобритания", "en-GB", "Europe/London", 0, Dst::Europe),
    c("DE", "Германия", "de-DE", "Europe/Berlin", 60, Dst::Europe),
    c("FR", "Франция", "fr-FR", "Europe/Paris", 60, Dst::Europe),
    c("NL", "Нидерланды", "nl-NL", "Europe/Amsterdam", 60, Dst::Europe),
    c("CH", "Швейцария", "de-CH", "Europe/Zurich", 60, Dst::Europe),
    c("ES", "Испания", "es-ES", "Europe/Madrid", 60, Dst::Europe),
    c("IT", "Италия", "it-IT", "Europe/Rome", 60, Dst::Europe),
    c("PL", "Польша", "pl-PL", "Europe/Warsaw", 60, Dst::Europe),
    c("SE", "Швеция", "sv-SE", "Europe/Stockholm", 60, Dst::Europe),
    c("FI", "Финляндия", "fi-FI", "Europe/Helsinki", 120, Dst::Europe),
    c("UA", "Украина", "uk-UA", "Europe/Kyiv", 120, Dst::Europe),
    c("RU", "Россия", "ru-RU", "Europe/Moscow", 180, Dst::None),
    c("KZ", "Казахстан", "ru-KZ", "Asia/Almaty", 300, Dst::None),
    c("TR", "Турция", "tr-TR", "Europe/Istanbul", 180, Dst::None),
    c("AE", "ОАЭ", "ar-AE", "Asia/Dubai", 240, Dst::None),
    c("IN", "Индия", "en-IN", "Asia/Kolkata", 330, Dst::None),
    c("SG", "Сингапур", "en-SG", "Asia/Singapore", 480, Dst::None),
    c("CN", "Китай", "zh-CN", "Asia/Shanghai", 480, Dst::None),
    c("JP", "Япония", "ja-JP", "Asia/Tokyo", 540, Dst::None),
    c("KR", "Южная Корея", "ko-KR", "Asia/Seoul", 540, Dst::None),
    c("AU", "Австралия", "en-AU", "Australia/Sydney", 600, Dst::Australia),
    c("CA", "Канада", "en-CA", "America/Toronto", -300, Dst::NorthAmerica),
    c("BR", "Бразилия", "pt-BR", "America/Sao_Paulo", -180, Dst::None),
];

/// The browser and country presented to sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Persona {
    pub browser: &'static Browser,
    pub country: &'static Country,
}

impl Default for Persona {
    fn default() -> Persona {
        // tests must not depend on the machine's locale
        let system = if cfg!(test) { None } else { system_country() };
        Persona { browser: &BROWSERS[0], country: system.unwrap_or(&COUNTRIES[0]) }
    }
}

/// The country of the system's own locale (macOS AppleLocale, else LC_ALL /
/// LANG), as a browser takes its language from the system, not from the exit IP.
fn system_country() -> Option<&'static Country> {
    static FOUND: std::sync::OnceLock<Option<&'static Country>> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| {
        let locale = std::env::var("LC_ALL").ok().filter(|v| !v.is_empty() && v != "C" && v != "POSIX")
            .or_else(|| {
                cfg!(target_os = "macos").then(|| {
                    std::process::Command::new("/usr/bin/defaults").args(["read", "-g", "AppleLocale"]).output().ok()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                }).flatten().filter(|v| !v.is_empty())
            })
            .or_else(|| std::env::var("LANG").ok().filter(|v| !v.is_empty() && v != "C"))?;
        // ru_RU, ru-RU, ru_RU.UTF-8, en_US@rg=… → the region, else the language's usual country
        let main = locale.split(['.', '@']).next().unwrap_or("");
        let mut parts = main.split(['_', '-']);
        let lang = parts.next().unwrap_or("").to_ascii_lowercase();
        let region = parts.last().filter(|r| r.len() == 2).map(|r| r.to_ascii_uppercase());
        region.and_then(|r| country(&r))
            .or_else(|| COUNTRIES.iter().find(|c| c.language.to_ascii_lowercase().starts_with(&format!("{lang}-"))))
    })
}

static CURRENT: RwLock<Option<Persona>> = RwLock::new(None);

#[cfg(test)]
thread_local! {
    /// A test's own persona, so parallel tests do not share the global one.
    pub static TEST_PERSONA: std::cell::Cell<Option<Persona>> = const { std::cell::Cell::new(None) };
}

/// The persona in use now.
pub fn current() -> Persona {
    #[cfg(test)]
    if let Some(p) = TEST_PERSONA.with(|t| t.get()) {
        return p;
    }
    CURRENT.read().ok().and_then(|p| *p).unwrap_or_default()
}

pub fn browser(id: &str) -> Option<&'static Browser> {
    BROWSERS.iter().find(|b| b.id.eq_ignore_ascii_case(id))
}

pub fn country(code: &str) -> Option<&'static Country> {
    COUNTRIES.iter().find(|c| c.code.eq_ignore_ascii_case(code))
}

/// Use `persona` from now on (the next requests and page loads).
pub fn set(persona: Persona) {
    if let Ok(mut p) = CURRENT.write() {
        *p = Some(persona);
    }
}

/// Choose and remember: the settings call this.
pub fn choose(browser_id: &str, country_code: &str) -> Result<Persona, String> {
    let b = browser(browser_id).ok_or_else(|| format!("unknown browser {browser_id:?}"))?;
    let c = country(country_code).ok_or_else(|| format!("unknown country {country_code:?}"))?;
    let persona = Persona { browser: b, country: c };
    set(persona);
    save(persona)?;
    Ok(persona)
}

fn settings_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(std::path::PathBuf::from(home).join(".sepia").join("browser").join("settings.tsv"))
}

/// Read the remembered choice (missing or unknown values keep the default).
pub fn load() -> Persona {
    let mut persona = Persona::default();
    if let Some(text) = settings_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        for line in text.lines() {
            match line.split_once('\t') {
                Some(("browser", id)) => {
                    if let Some(b) = browser(id.trim()) {
                        persona.browser = b;
                    }
                }
                Some(("country", code)) => {
                    if let Some(c) = country(code.trim()) {
                        persona.country = c;
                    }
                }
                _ => {}
            }
        }
    }
    persona
}

fn save(persona: Persona) -> Result<(), String> {
    let path = settings_path().ok_or("no home directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, format!("browser\t{}\ncountry\t{}\n", persona.browser.id, persona.country.code)).map_err(|e| e.to_string())
}

impl Persona {
    /// `Accept-Language`: the country's language first, English after.
    pub fn accept_language(&self) -> String {
        let tag = self.country.language;
        let primary = tag.split('-').next().unwrap_or(tag);
        if primary == "en" {
            if tag == "en-US" { "en-US,en;q=0.9".to_string() } else { format!("{tag},en;q=0.9,en-US;q=0.8") }
        } else {
            format!("{tag},{primary};q=0.9,en-US;q=0.8,en;q=0.7")
        }
    }

    /// `navigator.languages`
    pub fn languages(&self) -> Vec<String> {
        let tag = self.country.language;
        let primary = tag.split('-').next().unwrap_or(tag);
        let mut out = vec![tag.to_string()];
        for t in [primary, "en-US", "en"] {
            if !out.iter().any(|x| x == t) {
                out.push(t.to_string());
            }
        }
        out
    }

    /// The country's offset from UTC (seconds) at a moment, daylight saving included.
    pub fn offset_seconds(&self, unix: i64) -> i32 {
        let std = self.country.offset_minutes * 60;
        let year = year_of(unix);
        let dst = match self.country.dst {
            Dst::None => false,
            Dst::Europe => {
                let start = sunday_last(year, 3) + 3600;
                let end = sunday_last(year, 10) + 3600;
                (start..end).contains(&unix)
            }
            Dst::NorthAmerica => {
                let start = sunday_nth(year, 3, 2) + 2 * 3600 - std as i64;
                let end = sunday_nth(year, 11, 1) + 2 * 3600 - (std as i64 + 3600);
                (start..end).contains(&unix)
            }
            Dst::Australia => {
                // southern summer: on from October, off in April
                let off = sunday_nth(year, 4, 1) + 3 * 3600 - (std as i64 + 3600);
                let on = sunday_nth(year, 10, 1) + 2 * 3600 - std as i64;
                unix < off || unix >= on
            }
        };
        std + if dst { 3600 } else { 0 }
    }
}

// ---- calendar arithmetic (proleptic Gregorian, UTC) ----

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn year_of(unix: i64) -> i64 {
    let days = unix.div_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    yoe + era * 400 + if m <= 2 { 1 } else { 0 }
}

/// Weekday of a day count since 1970-01-01 (a Thursday): 0 = Sunday.
fn weekday(days: i64) -> i64 {
    (days + 4).rem_euclid(7)
}

/// Midnight UTC of the `n`th Sunday of a month.
fn sunday_nth(year: i64, month: i64, n: i64) -> i64 {
    let first = days_from_civil(year, month, 1);
    let to_sunday = (7 - weekday(first)) % 7;
    (first + to_sunday + 7 * (n - 1)) * 86400
}

/// Midnight UTC of the last Sunday of a month.
fn sunday_last(year: i64, month: i64) -> i64 {
    let next = if month == 12 { days_from_civil(year + 1, 1, 1) } else { days_from_civil(year, month + 1, 1) };
    let last = next - 1;
    (last - weekday(last)) * 86400
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i64, m: i64, d: i64, h: i64) -> i64 {
        days_from_civil(y, m, d) * 86400 + h * 3600
    }

    #[test]
    fn clocks_follow_daylight_saving() {
        let p = |code| Persona { browser: &BROWSERS[0], country: country(code).unwrap() };
        // 2026: EU switches on 29 March and 25 October; the US on 8 March and 1 November
        assert_eq!(p("DE").offset_seconds(at(2026, 1, 15, 12)), 3600);
        assert_eq!(p("DE").offset_seconds(at(2026, 3, 29, 0)), 3600);
        assert_eq!(p("DE").offset_seconds(at(2026, 3, 29, 2)), 7200);
        assert_eq!(p("DE").offset_seconds(at(2026, 10, 25, 2)), 3600);
        assert_eq!(p("US").offset_seconds(at(2026, 3, 8, 6)), -5 * 3600);
        assert_eq!(p("US").offset_seconds(at(2026, 3, 8, 8)), -4 * 3600);
        assert_eq!(p("US").offset_seconds(at(2026, 11, 1, 7)), -5 * 3600);
        assert_eq!(p("RU").offset_seconds(at(2026, 7, 1, 0)), 3 * 3600);
        assert_eq!(p("AU").offset_seconds(at(2026, 1, 10, 0)), 11 * 3600);
        assert_eq!(p("AU").offset_seconds(at(2026, 6, 10, 0)), 10 * 3600);
        assert_eq!(p("IN").offset_seconds(at(2026, 6, 10, 0)), 330 * 60);
    }

    #[test]
    fn headers_follow_the_persona() {
        let p = Persona { browser: browser("chrome-win").unwrap(), country: country("DE").unwrap() };
        TEST_PERSONA.with(|t| t.set(Some(p)));
        let url = crate::url::Url::parse("https://example.test/").unwrap();
        let headers = crate::net::request_headers(&url);
        let get = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone()).unwrap();
        assert!(get("User-Agent").contains("Chrome/141"));
        assert!(get("Accept").contains("image/webp") && !get("Accept").contains("avif"));
        assert_eq!(get("Accept-Language"), "de-DE,de;q=0.9,en-US;q=0.8,en;q=0.7");
        TEST_PERSONA.with(|t| t.set(None));
    }

    #[test]
    fn languages_and_lookup() {
        let ru = Persona { browser: browser("chrome-win").unwrap(), country: country("ru").unwrap() };
        assert_eq!(ru.accept_language(), "ru-RU,ru;q=0.9,en-US;q=0.8,en;q=0.7");
        assert_eq!(ru.languages(), ["ru-RU", "ru", "en-US", "en"]);
        let us = Persona::default();
        assert_eq!(us.accept_language(), "en-US,en;q=0.9");
        assert_eq!(us.languages(), ["en-US", "en"]);
        assert!(browser("nope").is_none());
        assert!(BROWSERS.iter().all(|b| !b.user_agent.is_empty()));
    }
}
