#![allow(unexpected_cfgs)]
//! sepia-browser: a browser written from scratch.
//!
//! Own URL parser, own HTTP/1.1+TLS client, own HTML tokenizer and tree builder,
//! own CSS parser and cascade, own block/inline layout, own software rasteriser.
//! Own engine; no webview or browser driver.

mod cookies;
mod charset;
mod css;
mod gui;
mod dom;
mod fonts;
mod html;
mod images;
mod js;
mod svg;
mod layout;
mod net;
mod page;
mod paint;
mod persona;
mod protocols;
mod resolver;
mod style;
mod url;
mod woff2;

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
       --insecure  accept self-signed certificates (Gemini capsules)\n\
       --wait MS   keep the page's scripts running MS ms before reporting (as the window does)\n\
       --browser ID   present as this browser for this run (firefox-mac, chrome-win, ...)\n\
       --country CODE present as from this country for this run (US, RU, DE, ...)\n\
       --no-embed  accepted for compatibility; all pages use our own engine\n"
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
    click: Option<(f32, f32)>,
    /// --wait: keep the page's scripts running this long (ms) before reporting
    wait: u64,
    browser: Option<String>,
    country: Option<String>,
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
        click: None,
        wait: 0,
        browser: None,
        country: None,
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
            "--no-embed" => {},
            "--browser" => args.browser = Some(value()?),
            "--wait" => args.wait = value()?.parse().map_err(|_| "bad wait")?,
            "--country" => args.country = Some(value()?),
            "--click" => {
                let v = value()?;
                let (x, y) = v.split_once(',').ok_or("--click takes X,Y")?;
                args.click = Some((x.trim().parse().map_err(|_| "bad X")?, y.trim().parse().map_err(|_| "bad Y")?));
            }
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
        ("cache / profile", "no disk cache or cookie profile; a page keeps its static files (JS, CSS, fonts)
                   in memory while it is open, up to 64 MiB; explicit saves write files"),
        ("client hints", "none sent"),
        ("prefetch", "not implemented: no prefetch, preconnect or speculative loads"),
        ("http/2, quic", "not implemented: plain HTTP/1.1 over TLS"),
    ] {
        println!("  {:<16} {}", row.0, row.1);
    }
    println!();
    println!("sent on every request");
    let persona = persona::current();
    println!("  (as {} in {}, chosen in the settings)", persona.browser.label, persona.country.label);
    println!("  User-Agent: {}", persona.browser.user_agent);
    println!("  Accept: {}", persona.browser.accept);
    println!("  Accept-Language: {}", persona.accept_language());
    for header in [
        "Accept-Encoding: gzip, deflate, br",
        "Connection: keep-alive (reused per site for 30 s, never across sites)",
    ] {
        println!("  {header}");
    }
    println!();
    println!("state and explicit disk writes");
    println!("  {:<16} {}", "cookies", "partitioned by the site in the address bar (a tracker embedded in");
    println!("  {:<16} {}", "", "two sites gets two unrelated jars), at most a day; the burn button and quit");
    println!("  {:<16} {}", "", "erase them; sent only on page, subresource and same-origin fetch requests");
    println!("  {:<16} {}", "bookmarks", "saved to ~/.sepia/browser/bookmarks.tsv,");
    println!("  {:<16} {}", "", "and only when you press ⌘D; downloads go to ~/Downloads on ⌘S or ⌥-click");
    println!();
    println!("requests a page triggers");
    println!("  {:<16} {}", "images", "<img src> of http(s) pages, all of them, 6 at a time: same path, DoH and");
    println!("  {:<16} {}", "", "headers as the page, Referer as a browser sends it; the site's cookies; data: locally;");
    println!("  {:<16} {}", "", "every image is requested (1x1 pixels too); no page budget");
    println!("  {:<16} {}", "stylesheets", "<link rel=stylesheet> and @import, all of them, any origin, same");
    println!("  {:<16} {}", "", "path and headers as the page; no size or time cap; CSS background url()");
    println!("  {:<16} {}", "", "images are fetched through the page path, using the same chain and resolver");
    println!("  {:<16} {}", "icon", "one per page, for the tab: <link rel=icon> (or /favicon.ico), fetched like");
    println!("  {:<16} {}", "", "an image (same path, the site's cookies), 4 s; kept in memory");
    println!("  {:<16} {}", "fonts", "@font-face files (TTF, OTF, TTC, WOFF, WOFF2), all of them, same path and");
    println!("  {:<16} {}", "", "headers as the page; kept in memory only");
    println!("  {:<16} {}", "scripts", "<script src> fetched like stylesheets (any origin) and run in Boa;");
    println!("  {:<16} {}", "", "classic scripts: all (12 at a time), no size or time cap; ES modules after,");
    println!("  {:<16} {}", "", "their imports (and import maps) fetched the same way, all of them;");
    println!("  {:<16} {}", "", "nomodule fallbacks are skipped; fetch()");
    println!("  {:<16} {}", "", "and XMLHttpRequest reach any site; cross-origin answers are read only if the server");
    println!("  {:<16} {}", "", "allows it (CORS: Access-Control-Allow-Origin; no preflight), no-cors gets an opaque");
    println!("  {:<16} {}", "", "answer; Referer and Origin as in a browser; script headers except forbidden ones");
    println!("  {:<16} {}", "", "(User-Agent, Cookie, Origin, Sec-*…); sendBeacon posts; no WebSocket or workers;");
    println!("  {:<16} {}", "", "cookies and storage in memory; navigator, language, Intl locale and clock as");
    println!("  {:<16} {}", "", "chosen in the settings; screen 1280x800; page viewport follows window size");
    println!("  {:<16} {}", "", "scripts and stylesheets a page inserts are fetched like its own (any origin),");
    println!("  {:<16} {}", "", "in the background; fetch() and XMLHttpRequest run in the background too");
    println!("  {:<16} {}", "page engine", "all domains use our own HTTP, HTML, CSS, layout and rasteriser pipeline");
    println!("  {:<16} {}", "script navigation", "startup location changes follow the same chain/resolver, up to 8 hops within the load timeout");
    println!("  {:<16} {}", "timers", "real deadlines; positive delays rounded to 100 ms; intervals run until cleared");
    println!("  {:<16} {}", "abort signals", "events/reasons and pre-aborted fetch rejection; no interruption of in-flight native HTTP");
    println!("  {:<16} {}", "compatibility", "the web platform is incomplete; script-heavy sites may render partially");
    println!("  {:<16} {}", "media", "<video>/<audio> are not played; no external player or helper tool is started");
    println!();
    println!("network path");
    match resolver {
        Some(resolver) => println!("  dns              DoH via {} (bootstraps: {})",
                                   resolver.url, resolver.bootstraps.join(", ")),
        None => println!("  dns              system resolver (as any browser); --doh URL for DNS over HTTPS"),
    }
    if let (true, Some(proxy)) = (chain.hops.is_empty(), net::system_proxy_summary()) {
        println!("  transport        system proxy {proxy} (as any browser; SEPIA_NO_SYSTEM_PROXY=1 to skip)");
    } else if chain.hops.is_empty() {
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

/// --click X,Y: load the page, click the point (page coordinates) the way
/// the window does and print what the page's scripts answered.
fn click_check(url: &Url, chain: &Chain, resolver: Option<&resolver::Resolver>, args: &Args, fonts: &Fonts, x: f32, y: f32) {
    let opened = page::open(url, chain, resolver, args.width as f32, fonts, Duration::from_secs_f64(args.timeout), args.insecure, false, None);
    let (loaded, session, _) = match opened {
        Ok(v) => v,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    };
    println!("url         {}", loaded.url.to_string());
    println!("note        {}", loaded.note);
    if x < 0.0 {
        // list what looks clickable, with where it is
        let Some(session) = &session else { println!("no session"); return };
        fn walk(n: &dom::Node, page: &layout::Page, out: &mut Vec<String>) {
            let button = n.tag == "button" || n.attr("role") == Some("button") || (n.tag == "input" && matches!(n.attr("type"), Some("submit" | "button" | "checkbox")));
            if button {
                let id = n.attr("data-sepia-id").and_then(|v| v.parse::<usize>().ok());
                let rect = id.and_then(|id| page.boxes.iter().find(|(_, b)| *b == id)).map(|(r, _)| format!("{:.0},{:.0} {:.0}x{:.0}", r.x + 2.0, r.y + 2.0, r.w, r.h));
                let label: String = n.text_content().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
                out.push(format!("  <{}> {:<28} {}", n.tag, rect.unwrap_or_else(|| "NO BOX".into()), label));
            }
            for c in &n.children {
                walk(c, page, out);
            }
        }
        let mut out = Vec::new();
        let tree = session.tree();
        fn count(n: &dom::Node) -> usize { 1 + n.children.iter().map(count).sum::<usize>() }
        println!("dom         {} nodes, {} chars of text", count(&tree), tree.text_content().len());
        walk(&tree, &loaded.page, &mut out);
        for line in &out {
            println!("{line}");
        }
        // every clickable link rectangle: x,y w,h and where it goes (the centre clicks it)
        for hit in &loaded.page.links {
            let r = hit.rect;
            println!("  link {:.0},{:.0} {:.0}x{:.0} {}", r.x, r.y, r.w, r.h, hit.href);
        }
        return;
    }
    let Some((node, href)) = loaded.page.element_at(x, y) else {
        println!("nothing at {x},{y}");
        return;
    };
    let Some(mut session) = session else {
        println!("element     #{node} href={href:?}");
        println!("no session: only the link would be followed");
        return;
    };
    fn find(n: &dom::Node, id: usize) -> Option<&dom::Node> {
        if n.attr("data-sepia-id").and_then(|v| v.parse::<usize>().ok()) == Some(id) {
            return Some(n);
        }
        n.children.iter().find_map(|c| find(c, id))
    }
    let tree = session.tree();
    let describe = find(&tree, node).map(|n| {
        let attrs: Vec<String> = n.attrs.iter().filter(|(k, _)| k != "data-sepia-id").map(|(k, v)| format!("{k}={:?}", v.chars().take(60).collect::<String>())).collect();
        format!("<{} {}>", n.tag, attrs.join(" "))
    });
    let rect = loaded.page.boxes.iter().find(|(_, id)| *id == node).map(|(r, _)| format!("{:.0},{:.0} {:.0}x{:.0}", r.x, r.y, r.w, r.h));
    println!("element     #{node} {} at {} href={href:?}", describe.unwrap_or_default(), rect.unwrap_or_default());
    let before = session.log().len();
    let text_before = loaded.page.text.join(" ");
    let out = session.handle(js::PageEvent::Click { node, href }, fonts);
    println!("navigate    {:?}", out.navigate);
    println!("form        {:?}", out.form);
    println!("jump        {:?}", out.jump);
    println!("focus       {:?}", out.focus);
    match &out.page {
        Some(page) => {
            let after = page.text.join(" ");
            println!("page        relaid out: {} text nodes, text {}", page.text.len(), if after == text_before { "unchanged" } else { "changed" });
        }
        None => println!("page        unchanged"),
    }
    for line in &session.log()[before..] {
        println!("js          {line}");
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

    // who to present as: the saved choice, or --browser/--country for this run
    let mut chosen = persona::load();
    if let Some(id) = &args.browser {
        match persona::browser(id) {
            Some(b) => chosen.browser = b,
            None => {
                let ids: Vec<&str> = persona::BROWSERS.iter().map(|b| b.id).collect();
                eprintln!("error: unknown --browser {id:?}: one of {}", ids.join(", "));
                std::process::exit(2);
            }
        }
    }
    if let Some(code) = &args.country {
        match persona::country(code) {
            Some(c) => chosen.country = c,
            None => {
                let codes: Vec<&str> = persona::COUNTRIES.iter().map(|c| c.code).collect();
                eprintln!("error: unknown --country {code:?}: one of {}", codes.join(", "));
                std::process::exit(2);
            }
        }
    }
    persona::set(chosen);

    // like any browser: the system resolver, unless --doh asks for DNS over HTTPS
    let resolver_setting = match (&args.doh, args.no_doh) {
        (Some(url), false) => Some(resolver::Resolver::new(Some(url.as_str()))),
        _ => None,
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

    if args.gui || (!args.text && args.dump.is_none() && args.click.is_none()) {
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

    // pages run on a thread with a page-sized stack, as in the window
    let worker = js::spawn_page(move || run_cli(args, resolver_setting));
    if worker.join().is_err() {
        std::process::exit(1);
    }
}

/// --text / --dump / --click: load one page and report on it.
fn run_cli(args: Args, resolver_setting: Option<resolver::Resolver>) {
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
    if let Some((x, y)) = args.click {
        click_check(&url, &chain, resolver_setting.as_ref(), &args, &fonts, x, y);
        return;
    }
    // like the window: the page's scripts keep running (timers, fetches) for
    // --wait ms before the page is reported
    let opened = page::open(
        &url,
        &chain,
        resolver_setting.as_ref(),
        args.width as f32,
        &fonts,
        Duration::from_secs_f64(args.timeout),
        args.insecure,
        false,
        None,
    );
    let loaded = match opened {
        Ok((mut loaded, session, _)) => {
            if let (Some(mut session), true) = (session, args.wait > 0) {
                let until = std::time::Instant::now() + Duration::from_millis(args.wait);
                while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
                    let Some(delay) = session.next_timer_delay() else { break };
                    std::thread::sleep(delay.min(left));
                    if std::time::Instant::now() >= until {
                        break;
                    }
                    let outcome = session.handle(js::PageEvent::Tick, &fonts);
                    if let Some(page) = outcome.page {
                        loaded.page = page;
                    }
                    if let Some(title) = outcome.title {
                        loaded.title = title;
                    }
                }
                session.debug_eval();
                loaded.page = session.layout(&fonts);
            }
            loaded
        }
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
    // distinct links (href and element), and the clickable rectangles they make
    let distinct: std::collections::HashSet<(&str, Option<usize>)> = page.links.iter().map(|l| (l.href.as_str(), l.node)).collect();
    println!("links       {}", distinct.len());
    println!("link rects  {}", page.links.len());
    println!("text nodes  {}", page.text.len());
    println!("note        {}", loaded.note);
    match &loaded.icon {
        Some(icon) => println!("icon        {}x{}", icon.width, icon.height),
        None => println!("icon        none"),
    }

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
