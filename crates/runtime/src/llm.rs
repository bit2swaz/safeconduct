//! Proxy for traffic that leaves the agent's own process, not the browser:
//! - model calls: point the agent's base URL here (GOOGLE_GEMINI_BASE_URL, OPENAI_BASE_URL, ...),
//! - site requests: set HTTP_PROXY here. Browser Use's agent writes Python that fetches the
//!   site directly; those requests go through the same record/replay store as the browser's.
//!
//! Record forwards to the real server and saves the response; replay answers from the recording
//! only. With --live-model, model calls are forwarded and not saved. Agents that call their model
//! from inside the browser (Stagehand v4) are handled in setup() instead.
use crate::net::{Answer, FAULT_503, Net, keep_header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// The real API for a request path, by each provider's path shape.
fn upstream(path: &str) -> &'static str {
    if path.starts_with("/v1/messages") {
        "https://api.anthropic.com"
    } else if path.starts_with("/v1/chat/completions") || path.starts_with("/v1/responses") {
        "https://api.openai.com"
    } else {
        "https://generativelanguage.googleapis.com"
    }
}

/// `cdp`: our CDP endpoint (127.0.0.1:port). With HTTP_PROXY set, agents send their CDP traffic
/// here too (Browser Use does); it is passed through untouched.
pub async fn serve(listener: TcpListener, net: Arc<Net>, cdp: String) {
    // The agent sees redirects as recorded, and follows them itself. No env proxy: the runtime is
    // the agent's HTTP_PROXY, and a runtime started from the agent's process (the clients do that)
    // inherits it, so it would send each request back to itself.
    // ponytail: no upstream proxy support; add a --proxy flag when someone needs one.
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).no_proxy().build().unwrap();
    let own = Arc::new(listener.local_addr().unwrap().to_string());
    let cdp = Arc::new(cdp);
    loop {
        let (sock, _) = listener.accept().await.unwrap();
        let (net, http, own, cdp) = (net.clone(), http.clone(), own.clone(), cdp.clone());
        tokio::spawn(async move {
            if let Err(e) = handle(sock, &net, &http, &own, &cdp).await {
                eprintln!("llm proxy: {e}");
            }
        });
    }
}

type Error = Box<dyn std::error::Error + Send + Sync>;

/// One request per connection: read it, answer it, close.
async fn handle(sock: TcpStream, net: &Net, http: &reqwest::Client, own: &str, cdp: &str) -> Result<(), Error> {
    let mut r = BufReader::new(sock);
    let mut line = String::new();
    r.read_line(&mut line).await?;
    let mut parts = line.split(' ');
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
    let (mut headers, mut len, mut chunked) = (Vec::new(), 0, false);
    loop {
        let mut h = String::new();
        r.read_line(&mut h).await?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (name, value) = h.split_once(':').ok_or("bad header")?;
        if name.eq_ignore_ascii_case("content-length") {
            len = value.trim().parse()?;
        }
        chunked |= name.eq_ignore_ascii_case("transfer-encoding") && value.to_ascii_lowercase().contains("chunked");
        headers.push((name.to_string(), value.trim().to_string()));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).await?;
    if chunked {
        body = read_chunked(&mut r).await?; // a streamed request body: no content-length
    }
    let post = String::from_utf8_lossy(&body).into_owned();
    // As an HTTP proxy we get absolute URLs. Addressed to us = a model call; anything else = the site.
    let (site, path) = match path.strip_prefix("http://") {
        Some(rest) => {
            let (authority, p) = rest.split_once('/').unwrap_or((rest, ""));
            let ours = authority == own || authority == own.replace("127.0.0.1", "localhost");
            (!ours, if ours { format!("/{p}") } else { path.clone() })
        }
        None => (false, path),
    };
    let mut sock = r.into_inner();
    let to_cdp = path == cdp || path.starts_with(&format!("http://{cdp}/"));
    if method == "CONNECT" && to_cdp {
        // The agent's CDP WebSocket, tunnelled through us: pass it through to the real endpoint.
        let mut up = TcpStream::connect(cdp).await?;
        sock.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
        tokio::io::copy_bidirectional(&mut sock, &mut up).await.ok();
        return Ok(());
    }
    if method == "CONNECT" {
        // ponytail: https from the agent's process would need a MITM CA to record. Refuse loudly.
        println!("{}", json!({ "event": "unsupported", "what": "https via HTTP_PROXY (CONNECT)", "target": path }));
        sock.write_all(b"HTTP/1.1 501 Not Implemented\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await?;
        return Ok(());
    }
    let post_opt = (!post.is_empty()).then_some(post.as_str());
    let (status, resp_headers, resp_body) = if to_cdp {
        forward(http, &method, &path, &headers, body).await? // /json/version etc.: not site traffic
    } else if site {
        site_call(net, http, &method, &path, &headers, body, post_opt).await?
    } else {
        model_call(net, http, &method, &format!("{}{path}", upstream(&path)), &headers, body, &post).await?
    };

    let mut out = format!("HTTP/1.1 {status} OK\r\n");
    for (n, v) in resp_headers {
        out += &format!("{n}: {v}\r\n");
    }
    out += &format!("content-length: {}\r\nconnection: close\r\n\r\n", resp_body.len());
    sock.write_all(out.as_bytes()).await?;
    sock.write_all(&resp_body).await?;
    Ok(())
}

/// An HTTP/1.1 chunked body: hex size lines, each followed by that many bytes, until size 0.
async fn read_chunked(r: &mut BufReader<TcpStream>) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    loop {
        let mut size = String::new();
        r.read_line(&mut size).await?;
        let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or(""), 16)?;
        if size == 0 {
            loop {
                // trailers, then the empty line that ends the body
                let mut t = String::new();
                if r.read_line(&mut t).await? == 0 || t.trim().is_empty() {
                    return Ok(body);
                }
            }
        }
        let start = body.len();
        body.resize(start + size + 2, 0); // the chunk and its CRLF
        r.read_exact(&mut body[start..]).await?;
        body.truncate(start + size);
    }
}

type Response = (u16, Vec<(String, String)>, Vec<u8>);

/// Forward a request to the real server, keeping the agent's headers except hop-by-hop ones.
async fn forward(http: &reqwest::Client, method: &str, url: &str, headers: &[(String, String)], body: Vec<u8>) -> Result<Response, Error> {
    let mut req = http.request(method.parse()?, url).body(body);
    for (name, value) in headers {
        if !["host", "content-length", "transfer-encoding", "accept-encoding", "connection", "proxy-connection"].contains(&name.to_ascii_lowercase().as_str()) {
            req = req.header(name, value);
        }
    }
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .filter(|(n, _)| keep_header(n.as_str()))
        .map(|(n, v)| (n.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    Ok((status, headers, resp.bytes().await?.to_vec()))
}

fn stored(headers: &[(String, String)]) -> serde_json::Value {
    headers.iter().map(|(n, v)| json!({ "name": n, "value": v })).collect()
}

fn from_entry(e: &serde_json::Value) -> Result<Response, Error> {
    let headers = e["headers"].as_array().into_iter().flatten();
    let headers = headers.map(|h| (h["name"].as_str().unwrap_or("").to_string(), h["value"].as_str().unwrap_or("").to_string()));
    Ok((e["status"].as_u64().unwrap_or(502) as u16, headers.collect(), B64.decode(e["body"].as_str().unwrap_or(""))?))
}

/// A site request from the agent's process: the same store, keys and events as the browser's.
async fn site_call(
    net: &Net, http: &reqwest::Client, method: &str, url: &str, headers: &[(String, String)], body: Vec<u8>, post: Option<&str>,
) -> Result<Response, Error> {
    net.request(method, url, post);
    if net.recording() {
        let resp = forward(http, method, url, headers, body).await?;
        let mut e = json!({ "method": method, "url": url, "status": resp.0, "headers": stored(&resp.1), "body": B64.encode(&resp.2) });
        if let Some(post) = post {
            e["postData"] = json!(post);
        }
        net.save(&e);
        return Ok(resp);
    }
    let answer = net.answer_site(method, url, post, None).await;
    if let Answer::Serve(e) | Answer::Error(Some(e)) | Answer::Drop(Some(e)) = &answer {
        net.delivered(e);
    }
    match answer {
        Answer::Serve(e) => from_entry(&e),
        Answer::Error(_) => Ok((503, vec![], FAULT_503.as_bytes().to_vec())),
        Answer::Drop(_) => Err("fuzz: connection dropped".into()), // closes the socket, no reply
        Answer::Miss => Ok((599, vec![], b"off-recording: safeconduct exact mode has no recorded response".to_vec())),
    }
}

async fn model_call(
    net: &Net, http: &reqwest::Client, method: &str, url: &str, headers: &[(String, String)], body: Vec<u8>, post: &str,
) -> Result<Response, Error> {
    if net.recording() || net.live_model {
        let resp = forward(http, method, url, headers, body).await?;
        if net.recording() {
            net.save_llm(method, url, post, resp.0, stored(&resp.1), &resp.2);
        }
        return Ok(resp);
    }
    match net.next_llm(method, url, post) {
        Some(e) => from_entry(&e),
        None => Ok((599, vec![], b"off-recording: safeconduct exact mode has no more recorded model calls".to_vec())),
    }
}
