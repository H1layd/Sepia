//! sepia-browser: a browser written from scratch.
//!
//! Own URL parser, own HTTP/1.1+TLS client, own HTML tokenizer and tree builder,
//! own CSS parser and cascade, own block/inline layout, own software rasteriser.
//! No engine, no webview, no driver.

mod cookies;
mod css;
mod gui;
mod dom;
mod fonts;
mod html;
mod images;
mod js;
mod media;
mod svg;
mod layout;
mod net;
mod page;
mod paint;
mod protocols;
mod resolver;
mod style;
mod url;

use std::path::PathBuf;
use std::time::Duration;

use css::Color;
use layout::Fonts;
use net::Chain;
use url::Url;

fn usage() -> &'static str {
    "sepia-browser — a browser written from scratch\n\n\
     usage: sepia-browser --url URL [--dump out.png] [--width W] [--height H]\n\
                         [--socks host:port[,host:port]] [--text] [--scroll PX]\n\n\
     options:\n\
       --url       page to load\n\
       --dump      render to a PNG file\n\
       --text      print the laid-out text instead of rendering\n\
       --width     viewport width (default 1200)\n\
       --height    viewport height (default 900)\n\
       --socks     SOCKS5 chain to route the request through\n\
       --scroll    vertical offset for the screenshot\n\
       --gui       open the browser window (default when no --dump/--text)\n\
       --doh URL   DoH endpoint for direct connections (default cloudflare)\n\
       --no-doh    use the system resolver (leaks names to the ISP)\n\
       --audit     print what this browser does and does not expose\n\
       --verbose   log DoH resolutions to stderr\n\
       --via tor|i2p  use the local Tor SOCKS or I2P HTTP proxy\n\
       --insecure  accept self-signed certificates (Gemini capsules)\n"
}

fn build_chain(socks: Option<&str>, via: Option<&str>) -> Result<Chain, String> {
    if let Some(preset) = via {
        return match preset.to_lowercase().as_str() {
            "tor" => Chain::parse("socks5://127.0.0.1:9050"),
            "i2p" => Chain::parse("http://127.0.0.1:4444"),
            "none" | "clear" => Ok(Chain::default()),
            other => Err(format!("unknown --via {other:?}: use tor, i2p or none")),
        };
    }
    match socks.map(Chain::parse) {
        Some(Ok(chain)) => Ok(chain),
        Some(Err(err)) => Err(err),
        None => Ok(Chain::default()),
    }
}

struct Args {
    url: String,
    dump: Option<PathBuf>,
    width: usize,
    height: usize,
    socks: Option<String>,
    text: bool,
    scroll: f32,
    timeout: f64,
    gui: bool,
    doh: Option<String>,
    no_doh: bool,
    audit: bool,
    via: Option<String>,
    insecure: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        url: String::new(),
        dump: None,
        width: 1200,
        height: 900,
        socks: None,
        text: false,
        scroll: 0.0,
        timeout: 20.0,
        gui: false,
        doh: None,
        no_doh: false,
        audit: false,
        via: None,
        insecure: false,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = || iter.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--help" | "-h" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            "--url" => args.url = value()?,
            "--dump" => args.dump = Some(PathBuf::from(value()?)),
            "--width" => args.width = value()?.parse().map_err(|_| "bad width")?,
            "--height" => args.height = value()?.parse().map_err(|_| "bad height")?,
            "--socks" => args.socks = Some(value()?),
            "--scroll" => args.scroll = value()?.parse().map_err(|_| "bad scroll")?,
            "--timeout" => args.timeout = value()?.parse().map_err(|_| "bad timeout")?,
            "--text" => args.text = true,
            "--gui" => args.gui = true,
            "--doh" => args.doh = Some(value()?),
            "--no-doh" => args.no_doh = true,
            "--audit" => args.audit = true,
            "--via" => args.via = Some(value()?),
            "--insecure" => args.insecure = true,
            "--verbose" | "-v" => std::env::set_var("SEPIA_VERBOSE", "1"),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.url.is_empty() && !args.gui && !args.audit {
        return Err("--url is required (or use --gui)".into());
    }
    Ok(args)
}

fn print_audit(chain: &Chain, resolver: Option<&resolver::Resolver>, timeout: f64) {
    println!("sepia-browser — anonymity surface");
    println!();
    println!("absent by construction");
    for row in [
        ("canvas / webgl", "not implemented: getContext() returns null, no audio, no devices"),
        ("webrtc", "not implemented: no UDP, no ICE, no local address exposure"),
        ("cache / profile", "no disk state: nothing written unless you --dump a PNG"),
        ("client hints", "none sent"),
        ("favicons / prefetch", "not implemented"),
        ("http/2, quic", "not implemented: plain HTTP/1.1 over TLS"),
    ] {
        println!("  {:<16} {}", row.0, row.1);
    }
    println!();
    println!("sent on every request");
    for header in [
        "User-Agent: sepia-browser/0.1",
        "Accept: text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8",
        "Accept-Encoding: gzip, deflate",
        "Accept-Language: en-US,en;q=0.9",
        "Connection: keep-alive (reused per site for 30 s, never across sites)",
    ] {
        println!("  {header}");
    }
    println!();
    println!("kept in memory, never on disk");
    println!("  {:<16} {}", "cookies", "partitioned by the site in the address bar (a tracker embedded in");
    println!("  {:<16} {}", "", "two sites gets two unrelated jars), at most a day; the burn button and quit");
    println!("  {:<16} {}", "", "erase them; sent only on page, subresource and same-origin fetch requests");
    println!();
    println!("requests a page triggers");
    println!("  {:<16} {}", "images", "<img src> of http(s) pages, up to 64, 6 at a time: same path, DoH and");
    println!("  {:<16} {}", "", "headers as the page (no Referer; the site's own cookies); data: URIs locally;");
    println!("  {:<16} {}", "", "1x1 (tracking) images are never requested; 8 s budget per page");
    println!("  {:<16} {}", "stylesheets", "<link rel=stylesheet> and @import, up to 24 (+8 imports), any origin, same");
    println!("  {:<16} {}", "", "path and headers as the page; 8 s budget; url() images in CSS are never");
    println!("  {:<16} {}", "", "requested");
    println!("  {:<16} {}", "fonts", "@font-face files (TTF, OTF, WOFF; not WOFF2), up to 16, same path and");
    println!("  {:<16} {}", "", "headers as the page, 6 s budget; kept in memory only");
    println!("  {:<16} {}", "scripts", "<script src> fetched like stylesheets (any origin) and run in Boa; fetch()");
    println!("  {:<16} {}", "", "and XMLHttpRequest reach only the page's own site (its subdomains, same");
    println!("  {:<16} {}", "", "scheme), any method, same path and headers plus Content-Type; script headers");
    println!("  {:<16} {}", "", "are dropped; no WebSocket, beacons or workers; cookies and storage in memory;");
    println!("  {:<16} {}", "", "navigator/screen fixed (sepia-browser/0.1, en-US, 1280x800); 100 ms timers");
    println!("  {:<16} {}", "youtube", "addresses open in mpv, search runs in yt-dlp; both only through a proxy on");
    println!("  {:<16} {}", "", "127.0.0.1 that carries each CONNECT (ports 80/443) along the same path");
    println!();
    println!("network path");
    match resolver {
        Some(resolver) => println!("  dns              DoH via {} (bootstraps: {})",
                                   resolver.url, resolver.bootstraps.join(", ")),
        None => println!("  dns              SYSTEM RESOLVER — names leak to the ISP"),
    }
    if chain.hops.is_empty() {
        println!("  transport        direct (no chain): your address is the exit");
    } else {
        let hops: Vec<String> = chain.hops.iter().map(|h| format!("{}:{}", h.host, h.port)).collect();
        println!("  transport        chain {}", hops.join(" -> "));
        println!("  dns at hops      remote: names are resolved by the first hop");
    }
    println!();
    println!("live check (through the configured path)");
    let trace = Url::parse("https://cloudflare.com/cdn-cgi/trace").expect("url");
    match net::fetch_with(&trace, chain, resolver, Duration::from_secs_f64(timeout)) {
        Ok(response) => {
            let body = String::from_utf8_lossy(&response.body);
            for line in body.lines() {
                if line.starts_with("ip=") || line.starts_with("loc=") || line.starts_with("colo=") {
                    println!("  {}", line);
                }
            }
            println!("  http             {}", response.status);
        }
        Err(err) => println!("  failed            {err}"),
    }
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("error: {err}\n\n{}", usage());
            std::process::exit(2);
        }
    };

    let resolver_setting = if args.no_doh {
        None
    } else {
        Some(resolver::Resolver::new(args.doh.as_deref()))
    };

    if args.audit {
        let chain = match build_chain(args.socks.as_deref(), args.via.as_deref()) {
            Ok(chain) => chain,
            Err(err) => {
                eprintln!("error: {err}");
                std::process::exit(2);
            }
        };
        print_audit(&chain, resolver_setting.as_ref(), args.timeout);
        return;
    }

    if args.gui || (!args.text && args.dump.is_none()) {
        let initial = if args.url.is_empty() { None } else { Some(args.url.clone()) };
        let chain = match build_chain(args.socks.as_deref(), args.via.as_deref()) {
            Ok(chain) => chain,
            Err(err) => {
                eprintln!("error: {err}");
                std::process::exit(2);
            }
        };
        if let Err(err) = gui::run(initial, chain, resolver_setting) {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
        return;
    }

    let url = match Url::parse(&args.url) {
        Some(url) => url,
        None => {
            eprintln!("error: cannot parse URL {:?}", args.url);
            std::process::exit(2);
        }
    };
    let chain = match build_chain(args.socks.as_deref(), args.via.as_deref()) {
        Ok(chain) => chain,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(2);
        }
    };

    let started = std::time::Instant::now();
    let fonts = Fonts::load();
    let loaded = match page::load(
        &url,
        &chain,
        resolver_setting.as_ref(),
        args.width as f32,
        &fonts,
        Duration::from_secs_f64(args.timeout),
        args.insecure,
    ) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    };
    let fetched_ms = started.elapsed().as_millis();
    let page = loaded.page;
    let title = loaded.title.clone();

    println!("url         {}", loaded.url.to_string());
    println!("scheme      {}", loaded.url.scheme);
    println!("status      {}", loaded.status);
    println!("fetched     {} ms", fetched_ms);
    println!("title       {}", if title.is_empty() { "(none)" } else { &title });
    println!("styles      {} rules", loaded.rules);
    println!("page height {:.0}px", page.height);
    println!("links       {}", page.links.len());
    println!("text nodes  {}", page.text.len());
    println!("note        {}", loaded.note);

    if std::env::var("SEPIA_VERBOSE").or_else(|_| std::env::var("VEIL_VERBOSE")).is_ok() {
        for draw in &page.draws {
            if let layout::Draw::Text { x, baseline, text, size, .. } = draw {
                eprintln!("text x={x:.1} y={baseline:.1} size={size:.0} {text:?}");
            }
        }
    }

    if args.text {
        let mut line = String::new();
        for piece in &page.text {
            line.push_str(piece);
            if line.len() > 100 {
                println!("  {line}");
                line.clear();
            }
        }
        if !line.is_empty() {
            println!("  {line}");
        }
    }

    if let Some(path) = &args.dump {
        let height = args.height.max(1);
        let mut canvas = paint::Canvas::new(args.width, height, Color::WHITE);
        canvas.paint(&fonts, &page, args.scroll);
        if let Err(err) = canvas.save_png(path) {
            eprintln!("error: cannot write {}: {err}", path.display());
            std::process::exit(1);
        }
        println!("png         {} ({}x{})", path.display(), args.width, height);
    }
}
