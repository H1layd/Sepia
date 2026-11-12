//! Video through mpv and yt-dlp, kept on the browser's network path.
//!
//! Neither tool is allowed onto the network by itself. Sepia runs a small
//! HTTP CONNECT proxy on 127.0.0.1 and hands its address to both: every
//! connection they open is carried along the browser's own path
//! (`net::open_path`: the chain, or DoH plus a direct connection), so names
//! never reach the system resolver and Tor/I2P/proxy settings apply to video
//! as they do to pages. Only CONNECT to ports 80 and 443 is served.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use crate::net::{open_path, Chain};
use crate::resolver::Resolver;

static PROXY: OnceLock<Result<u16, String>> = OnceLock::new();

/// The local proxy's port, started on first use with this path.
pub fn proxy_port(chain: &Chain, resolver: Option<&Resolver>) -> Result<u16, String> {
    PROXY
        .get_or_init(|| {
            let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("media proxy: {e}"))?;
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            let (chain, resolver) = (chain.clone(), resolver.cloned());
            std::thread::spawn(move || {
                for client in listener.incoming().flatten() {
                    let (chain, resolver) = (chain.clone(), resolver.clone());
                    std::thread::spawn(move || {
                        let _ = serve(client, &chain, resolver.as_ref());
                    });
                }
            });
            Ok(port)
        })
        .clone()
}

/// One proxied connection: read `CONNECT host:port`, open it along the
/// path, then copy bytes both ways until either side closes.
fn serve(mut client: TcpStream, chain: &Chain, resolver: Option<&Resolver>) -> Result<(), String> {
    client.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if client.read(&mut byte).map_err(|e| e.to_string())? == 0 || head.len() > 8192 {
            return Ok(());
        }
        head.push(byte[0]);
    }
    let request = String::from_utf8_lossy(&head);
    let mut parts = request.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if !method.eq_ignore_ascii_case("CONNECT") {
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
        return Ok(());
    }
    let Some((host, port)) = target.rsplit_once(':').and_then(|(h, p)| p.parse::<u16>().ok().map(|p| (h.trim_matches(|c| c == '[' || c == ']'), p))) else {
        let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        return Ok(());
    };
    if !method.eq_ignore_ascii_case("CONNECT") || !matches!(port, 80 | 443) || host.is_empty() {
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
        return Ok(());
    }
    let upstream = match open_path(host, port, chain, resolver, Duration::from_secs(20)) {
        Ok(stream) => stream,
        Err(_) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
            return Ok(());
        }
    };
    client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").map_err(|e| e.to_string())?;
    client.set_read_timeout(None).ok();
    upstream.set_read_timeout(None).ok();
    let (mut c_read, mut u_write) = (client.try_clone().map_err(|e| e.to_string())?, upstream.try_clone().map_err(|e| e.to_string())?);
    let (mut u_read, mut c_write) = (upstream, client);
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut c_read, &mut u_write);
        let _ = u_write.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut u_read, &mut c_write);
    let _ = c_write.shutdown(std::net::Shutdown::Write);
    let _ = up.join();
    Ok(())
}

/// A tool from Homebrew or the PATH (a GUI app often starts without
/// Homebrew's directory in its PATH).
fn tool(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Is the player usable? (A Homebrew mpv built against an older ffmpeg
/// exits at once with a dynamic-library error.)
fn player() -> Result<PathBuf, String> {
    static CHECK: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    CHECK
        .get_or_init(|| {
            let mpv = tool("mpv").ok_or("mpv is not installed: brew install mpv")?;
            let out = Command::new(&mpv).arg("--version").stdin(Stdio::null()).output().map_err(|e| format!("mpv does not start: {e}"))?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                let first = err.lines().next().unwrap_or("").trim().to_string();
                return Err(format!("mpv does not start ({first}). Usually a Homebrew upgrade left it behind ffmpeg: brew upgrade mpv"));
            }
            tool("yt-dlp").ok_or("yt-dlp is not installed: brew install yt-dlp")?;
            Ok(mpv)
        })
        .clone()
}

/// Open `url` in mpv, all its traffic through the media proxy.
pub fn play(url: &str, chain: &Chain, resolver: Option<&Resolver>) -> Result<(), String> {
    let mpv = player()?;
    let port = proxy_port(chain, resolver)?;
    let proxy = format!("http://127.0.0.1:{port}");
    let ytdl = tool("yt-dlp").map(|p| p.display().to_string()).unwrap_or_else(|| "yt-dlp".into());
    Command::new(mpv)
        .arg("--force-window=immediate")
        .arg("--keep-open=no")
        .arg(format!("--http-proxy={proxy}"))
        .arg(format!("--script-opts=ytdl_hook-ytdl_path={ytdl}"))
        .arg(format!("--ytdl-raw-options=proxy={proxy}"))
        .arg("--ytdl-format=bestvideo[height<=?1080]+bestaudio/best")
        .arg("--title=Sepia · ${media-title}")
        .arg("--")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("mpv: {e}"))?;
    Ok(())
}

/// One search result.
#[derive(Debug, Clone)]
pub struct Video {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub duration: String,
}

/// Search YouTube with yt-dlp through the media proxy.
pub fn search(query: &str, chain: &Chain, resolver: Option<&Resolver>) -> Result<Vec<Video>, String> {
    let ytdl = tool("yt-dlp").ok_or("yt-dlp is not installed: brew install yt-dlp")?;
    let port = proxy_port(chain, resolver)?;
    let out = Command::new(ytdl)
        .arg("--proxy")
        .arg(format!("http://127.0.0.1:{port}"))
        .args(["--flat-playlist", "--no-warnings", "--ignore-config", "--print", "%(id)s\t%(title)s\t%(channel,uploader)s\t%(duration_string)s"])
        .arg(format!("ytsearch20:{query}"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("yt-dlp: {e}"))?;
    if !out.status.success() && out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("yt-dlp: {}", err.lines().last().unwrap_or("search failed")));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let id = f.next()?.trim().to_string();
            let valid = id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            valid.then(|| Video {
                id,
                title: f.next().unwrap_or("").to_string(),
                channel: f.next().map(|c| if c == "NA" { "" } else { c }).unwrap_or("").to_string(),
                duration: f.next().map(|d| if d == "NA" { "" } else { d }).unwrap_or("").to_string(),
            })
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Which YouTube addresses become what
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum Route {
    /// a video: play it (the canonical watch URL)
    Watch(String),
    Search(String),
    Home,
}

/// YouTube addresses are not loaded as pages (their app needs far more of
/// the web platform than Sepia has): videos go to mpv, search to yt-dlp.
pub fn route(url: &crate::url::Url) -> Option<Route> {
    let host = url.host.to_ascii_lowercase();
    let host = host.trim_start_matches("www.").trim_start_matches("m.").trim_start_matches("music.");
    let video = |id: &str| {
        let id: String = id.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        (id.len() == 11).then(|| Route::Watch(format!("https://www.youtube.com/watch?v={id}")))
    };
    let param = |name: &str| {
        url.query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == name).then(|| percent_decode(v))
        })
    };
    match host {
        "youtu.be" => video(url.path.trim_start_matches('/')),
        "youtube.com" | "youtube-nocookie.com" => {
            let path = url.path.as_str();
            if path == "/watch" {
                param("v").and_then(|v| video(&v)).or(Some(Route::Home))
            } else if let Some(rest) = path.strip_prefix("/shorts/").or_else(|| path.strip_prefix("/live/")).or_else(|| path.strip_prefix("/embed/")) {
                video(rest).or(Some(Route::Home))
            } else if path == "/results" {
                Some(param("search_query").filter(|q| !q.trim().is_empty()).map(Route::Search).unwrap_or(Route::Home))
            } else {
                Some(Route::Home)
            }
        }
        _ => None,
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

const PAGE_STYLE: &str = "<style>
body{margin:0;background:#0f0f10;color:#f1f1f1;font-family:sans-serif}
.wrap{max-width:960px;margin:0 auto;padding:72px 24px 120px}
h1{font-size:28px;margin:0 0 8px}
.lead{color:#aaaaaa;margin:0 0 28px;line-height:1.5}
form{display:flex;gap:8px;margin:0 0 28px}
input{flex:1;font-size:16px;padding:10px 14px;border:1px solid #3a3a3a;border-radius:20px;background:#121212;color:#f1f1f1}
button{font-size:15px;padding:10px 18px;border:0;border-radius:20px;background:#ff0033;color:#ffffff}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(280px,1fr));gap:20px}
.card{display:flex;flex-direction:column;gap:8px;text-decoration:none;color:#f1f1f1}
.card img{width:100%;height:auto;border-radius:12px;background:#222222}
.card b{font-size:15px;line-height:1.35}
.meta{color:#aaaaaa;font-size:13px}
.note{padding:14px 16px;border-radius:12px;background:#1f1f1f;color:#cccccc;line-height:1.5}
.error{background:#3a1414;color:#ffb4b4}
code{background:#000000;padding:2px 6px;border-radius:4px}
</style>";

fn form(query: &str) -> String {
    format!(
        "<form action=\"/results\" method=\"get\"><input name=\"search_query\" value=\"{}\" placeholder=\"Поиск на YouTube\"><button type=\"submit\">Найти</button></form>",
        esc(query)
    )
}

/// The page a YouTube address turns into (the work it implies is done here:
/// a video starts playing, a search runs). Returns (title, html).
pub fn page(route: &Route, chain: &Chain, resolver: Option<&Resolver>) -> (String, String) {
    // a script keeps the page interactive (the search form submits)
    let script = "<script>/* sepia */</script>";
    match route {
        Route::Home => (
            "YouTube".into(),
            format!(
                "<html><head><title>YouTube</title>{PAGE_STYLE}</head><body><div class=\"wrap\"><h1>YouTube</h1>\
                 <p class=\"lead\">Видео открывается в mpv, а поиск идёт через yt-dlp. Весь их трафик идёт через ту же цепочку, что и у браузера. Сам сайт YouTube не загружается: его приложению нужно гораздо больше веб-платформы, чем есть у Sepia.</p>\
                 {}<p class=\"note\">Совет: в адресной строке можно писать <code>yt что-нибудь</code>.</p></div>{script}</body></html>",
                form("")
            ),
        ),
        Route::Watch(url) => {
            let (title, body) = match play(url, chain, resolver) {
                Ok(()) => ("Видео в mpv".to_string(), format!("<h1>Видео открывается в mpv</h1><p class=\"lead\">{}</p><p class=\"note\">Окно плеера появится через несколько секунд: yt-dlp получает поток через цепочку браузера. Закрыть — <code>q</code>, пауза — пробел, перемотка — стрелки.</p>", esc(url))),
                Err(e) => ("Плеер не запустился".to_string(), format!("<h1>Плеер не запустился</h1><p class=\"lead\">{}</p><p class=\"note error\">{}</p>", esc(url), esc(&e))),
            };
            (title.clone(), format!("<html><head><title>{}</title>{PAGE_STYLE}</head><body><div class=\"wrap\">{body}{}</div>{script}</body></html>", esc(&title), form("")))
        }
        Route::Search(query) => {
            let results = match search(query, chain, resolver) {
                Ok(list) if list.is_empty() => "<p class=\"note\">Ничего не нашлось.</p>".to_string(),
                Ok(list) => {
                    let cards: String = list
                        .iter()
                        .map(|v| {
                            let meta = [v.channel.as_str(), v.duration.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
                            format!(
                                "<a class=\"card\" href=\"https://www.youtube.com/watch?v={id}\"><img src=\"https://i.ytimg.com/vi/{id}/mqdefault.jpg\" width=\"320\" height=\"180\" alt=\"\"><b>{}</b><span class=\"meta\">{}</span></a>",
                                esc(&v.title),
                                esc(&meta),
                                id = v.id
                            )
                        })
                        .collect();
                    format!("<div class=\"grid\">{cards}</div>")
                }
                Err(e) => format!("<p class=\"note error\">{}</p>", esc(&e)),
            };
            let title = format!("{query} — YouTube");
            (title.clone(), format!("<html><head><title>{}</title>{PAGE_STYLE}</head><body><div class=\"wrap\"><h1>{}</h1>{}{results}</div>{script}</body></html>", esc(&title), esc(query), form(query)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::url::Url;

    fn r(u: &str) -> Option<Route> {
        route(&Url::parse(u).unwrap())
    }

    #[test]
    fn youtube_addresses_route() {
        let w = Some(Route::Watch("https://www.youtube.com/watch?v=dQw4w9WgXcQ".into()));
        assert_eq!(r("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10"), w);
        assert_eq!(r("https://youtu.be/dQw4w9WgXcQ?si=x"), w);
        assert_eq!(r("https://m.youtube.com/shorts/dQw4w9WgXcQ"), w);
        assert_eq!(r("https://www.youtube.com/results?search_query=rust+lang"), Some(Route::Search("rust lang".into())));
        assert_eq!(r("https://www.youtube.com/"), Some(Route::Home));
        assert_eq!(r("https://example.com/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(r("https://www.youtube.com/watch?v=bad"), Some(Route::Home));
    }

    #[test]
    fn proxy_refuses_anything_but_connect() {
        let port = proxy_port(&Chain::default(), None).unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n").unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"CONNECT example.com:22 HTTP/1.1\r\n\r\n").unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 403"), "only 80 and 443: {reply}");
    }
}
