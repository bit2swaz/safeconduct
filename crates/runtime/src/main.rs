//! safeconduct replay browser v0.
//!
//! Launches headless Chromium and exposes one CDP endpoint. Agents connect to it as to any
//! Chrome (connectOverCDP, cdp_url). Beside the agent's connection the runtime keeps its own:
//! - it records or replays the network through Fetch,
//! - it seeds RNG and fixes the clock in every page and worker before their scripts run.
//!
//! CDP state is per session, so the agent never sees our commands, replies or events.
mod cdp;
mod net;

use cdp::Cdp;
use futures_util::{SinkExt, StreamExt};
use net::Net;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const USAGE: &str = "usage: safeconduct record|replay <file.jsonl> [--port 9222] [--seed 42]";
/// 2023-11-14T22:13:20Z. Every run starts its clock here.
const T0: u64 = 1_700_000_000_000;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str, default: u64| -> u64 {
        args.iter().position(|a| a == name).map_or(default, |i| args[i + 1].parse().expect(USAGE))
    };
    let (port, seed) = (flag("--port", 9222) as u16, flag("--seed", 42));
    let net = Arc::new(match (args.first().map(String::as_str), args.get(1)) {
        (Some("record"), Some(path)) => Net::record(path),
        (Some("replay"), Some(path)) => Net::replay(path),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    });

    let (chrome, ws_url, profile) = launch();
    let chrome_port: u16 = ws_url.split(':').nth(2).unwrap().split('/').next().unwrap().parse().unwrap();
    let (cdp, mut events) = Cdp::connect(&ws_url).await;

    let start = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let init = format!("{}({seed}, {T0}, {start});", include_str!("init.js"));
    let fetch = json!({ "patterns": net.fetch_patterns() });
    let setup_cmds = Arc::new((init, fetch));
    // Pause every new target until it is set up. The agent may ask for the same pause;
    // Chrome then waits for both of us to call runIfWaitingForDebugger.
    let auto_attach = json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true });
    cdp.call("Target.setAutoAttach", auto_attach.clone(), None).await.unwrap();

    let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("bind port");
    println!("{}", json!({ "event": "ready", "endpoint": format!("http://127.0.0.1:{port}") }));

    let events_loop = async {
        while let Some(ev) = events.recv().await {
            let (cdp, net, cmds, auto_attach) = (cdp.clone(), net.clone(), setup_cmds.clone(), auto_attach.clone());
            match ev["method"].as_str() {
                Some("Fetch.requestPaused") => {
                    tokio::spawn(async move { net.paused(&cdp, ev["sessionId"].as_str(), &ev["params"]).await });
                }
                Some("Target.attachedToTarget") => {
                    tokio::spawn(async move { setup(&cdp, &ev["params"], &cmds.0, &cmds.1, auto_attach).await });
                }
                _ => {}
            }
        }
    };
    let accept_loop = async {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            tokio::spawn(handle(sock, chrome_port, port));
        }
    };
    // Exit on Ctrl-C or SIGTERM, so Chrome is always killed below.
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    tokio::select! {
        _ = events_loop => eprintln!("chrome closed the connection"),
        _ = accept_loop => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    drop(chrome);
    std::fs::remove_dir_all(profile).ok();
}

/// Prepare one new target, then let it run: Fetch and the init script are in place
/// before runIfWaitingForDebugger. Extension workers are the agent's (Stagehand v4), not the site.
async fn setup(cdp: &Cdp, p: &Value, init: &str, fetch: &Value, auto_attach: Value) {
    let s = p["sessionId"].as_str();
    let site = !p["targetInfo"]["url"].as_str().unwrap_or("").starts_with("chrome-extension://");
    match p["targetInfo"]["type"].as_str().unwrap_or("") {
        // Pages always: Stagehand opens its tabs on an extension page, then goes to the site.
        // init.js itself skips extension documents.
        "page" | "iframe" => {
            cdp.call("Fetch.enable", fetch.clone(), s).await.ok();
            cdp.call("Page.enable", json!({}), s).await.ok(); // without it, the script below never runs
            let script = json!({ "source": init, "runImmediately": true });
            cdp.call("Page.addScriptToEvaluateOnNewDocument", script, s).await.ok();
            cdp.call("Target.setAutoAttach", auto_attach, s).await.ok(); // its workers and OOPIFs
        }
        "worker" | "shared_worker" | "service_worker" if site => {
            cdp.call("Fetch.enable", fetch.clone(), s).await.ok();
            cdp.call("Runtime.evaluate", json!({ "expression": init }), s).await.ok();
        }
        _ => {}
    }
    cdp.call("Runtime.runIfWaitingForDebugger", json!({}), s).await.ok();
}

struct KillOnDrop(std::process::Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

/// Start full Chromium, headless. Not chrome-headless-shell: Stagehand v4 runs inside an
/// extension and loads it over CDP (Extensions.loadUnpacked), which the shell lacks.
fn launch() -> (KillOnDrop, String, std::path::PathBuf) {
    let exe = std::env::var("SAFECONDUCT_CHROME").unwrap_or_else(|_| find_chromium());
    let profile = std::env::temp_dir().join(format!("safeconduct-{}", std::process::id()));
    let mut child = Command::new(&exe)
        .args([
            "--headless=new",
            "--enable-unsafe-extension-debugging", // the Extensions CDP domain, for Stagehand v4
            "--disable-component-extensions-with-background-pages", // no built-in service workers
            "--remote-debugging-port=0",
            "about:blank",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .stdout(Stdio::null()) // else Chrome keeps our stdout pipe open after we exit
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("start {exe}: {e}"));
    let stderr = child.stderr.take().unwrap();
    let chrome = KillOnDrop(child);
    // Port 0: Chrome picks a free port and prints the endpoint on stderr.
    let mut lines = BufReader::new(stderr).lines().map_while(Result::ok);
    let ws_url = lines
        .find_map(|l| l.strip_prefix("DevTools listening on ").map(String::from))
        .expect("chrome printed no DevTools endpoint");
    // Keep reading: a closed stderr pipe makes Chrome's next log line fail.
    std::thread::spawn(move || lines.for_each(drop));
    (chrome, ws_url, profile)
}

/// Newest Playwright-installed Chromium. Install one with `npx playwright install chromium`.
fn find_chromium() -> String {
    let cache = std::path::Path::new(&std::env::var("HOME").unwrap()).join(".cache/ms-playwright");
    let mut dirs: Vec<_> = std::fs::read_dir(&cache)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("chromium-"))
        .collect();
    dirs.sort_by_key(|p| p.file_name().unwrap().to_string_lossy().rsplit('-').next().unwrap().parse::<u32>().unwrap_or(0));
    let exe = dirs.last().expect("no Chromium: set SAFECONDUCT_CHROME").join("chrome-linux64/chrome");
    exe.to_string_lossy().into_owned()
}

/// One TCP connection from the agent: a WebSocket upgrade, or plain HTTP like /json/version.
async fn handle(sock: TcpStream, chrome_port: u16, port: u16) {
    let mut head = vec![0; 8192];
    let n = loop {
        let n = sock.peek(&mut head).await.unwrap_or(0);
        if n == 0 || n == head.len() || head[..n].windows(4).any(|w| w == b"\r\n\r\n") {
            break n;
        }
    };
    if String::from_utf8_lossy(&head[..n]).to_ascii_lowercase().contains("upgrade: websocket") {
        websocket(sock, chrome_port).await;
    } else {
        http(sock, chrome_port, port).await.ok();
    }
}

/// Forward an HTTP request to Chrome and rewrite Chrome's port to ours in the reply,
/// so webSocketDebuggerUrl points back at us.
async fn http(mut sock: TcpStream, chrome_port: u16, port: u16) -> std::io::Result<()> {
    let mut reader = tokio::io::BufReader::new(&mut sock);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let (method, path) = { let mut w = line.split(' '); (w.next().unwrap_or("GET").to_string(), w.next().unwrap_or("/").to_string()) };
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).await? == 0 || h == "\r\n" {
            break;
        }
    }
    // Chrome drops HTTP/1.0 and ignores "Connection: close": speak 1.1, read by Content-Length.
    let mut up = tokio::io::BufReader::new(TcpStream::connect(("127.0.0.1", chrome_port)).await?);
    up.get_mut().write_all(format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{chrome_port}\r\n\r\n").as_bytes()).await?;
    let (mut status, mut len) = (String::new(), 0);
    up.read_line(&mut status).await?;
    loop {
        let mut h = String::new();
        up.read_line(&mut h).await?;
        if h == "\r\n" || h.is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; len];
    up.read_exact(&mut body).await?;
    let body = String::from_utf8_lossy(&body).replace(&format!("127.0.0.1:{chrome_port}"), &format!("127.0.0.1:{port}"));
    let resp = format!("{status}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    sock.write_all(resp.as_bytes()).await
}

/// Pipe one agent WebSocket to the same path on Chrome, both ways, unchanged.
#[allow(clippy::result_large_err)] // the handshake callback's error type is tungstenite's
async fn websocket(sock: TcpStream, chrome_port: u16) {
    let mut path = String::new();
    let Ok(agent) = tokio_tungstenite::accept_hdr_async(sock, |req: &tokio_tungstenite::tungstenite::handshake::server::Request, resp| {
        path = req.uri().path().to_string();
        Ok(resp)
    })
    .await
    else {
        return;
    };
    let Ok((chrome, _)) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{chrome_port}{path}")).await else {
        return;
    };
    let (mut agent_tx, mut agent_rx) = agent.split();
    let (mut chrome_tx, mut chrome_rx) = chrome.split();
    let up = async {
        while let Some(Ok(m)) = agent_rx.next().await {
            if m.is_close() || chrome_tx.send(m).await.is_err() {
                break;
            }
        }
    };
    let down = async {
        while let Some(Ok(m)) = chrome_rx.next().await {
            if agent_tx.send(m).await.is_err() {
                break;
            }
        }
    };
    tokio::select! { _ = up => {}, _ = down => {} }
}
