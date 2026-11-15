//! Record and replay of everything a run depends on from outside: site responses and model calls.
//!
//! The recording is JSONL, one response per line:
//!   {"method","url","postData"?, "status","headers","body"(base64)}  or  {..., "error"}
//! Model calls have "llm": true. Request headers are never stored: they hold the API keys.
//!
//! Replay is exact mode. Site responses are matched by request and served in recorded order.
//! Model calls are served strictly in recorded order; a request that differs from the recorded
//! one is reported as llm-drift. Anything not in the recording stops the run (off-recording).
use crate::cdp::Cdp;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Model APIs. Requests to these hosts are model calls, not site traffic.
pub const LLM_HOSTS: [&str; 3] = ["generativelanguage.googleapis.com", "api.openai.com", "api.anthropic.com"];

pub struct Net {
    record: Option<Mutex<std::fs::File>>,
    /// Site responses per request key. The last one repeats.
    site: Mutex<HashMap<String, VecDeque<Value>>>,
    /// Replay: per site response in recorded order ("seq"), whether it was delivered. See turn().
    delivered: Mutex<Vec<bool>>,
    /// Model calls in recorded order, and the index of the next one.
    llm: Mutex<(usize, Vec<Value>)>,
    /// Exact mode hit something not in the recording: main() ends the run.
    pub stop: Notify,
    /// Replay the site but let model calls through to the real API (fuzz mode builds on this).
    pub live_model: bool,
    /// Site requests paused and not yet answered, and when the last one was answered. See settle().
    in_flight: AtomicUsize,
    last_answer: Mutex<Instant>,
}

// Chrome and reqwest hand us the decoded body, so these would describe bytes we no longer have.
const DROP_HEADERS: [&str; 4] = ["content-encoding", "content-length", "transfer-encoding", "connection"];

fn key(method: &str, url: &str, post: Option<&str>) -> String {
    format!("{method} {url}\n{}", post.unwrap_or(""))
}

/// The URL without a `key=` query parameter, so an API key passed that way is never stored.
pub fn strip_key(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else { return url.to_string() };
    let kept: Vec<&str> = query.split('&').filter(|p| !p.starts_with("key=")).collect();
    if kept.is_empty() { base.to_string() } else { format!("{base}?{}", kept.join("&")) }
}

pub fn keep_header(name: &str) -> bool {
    !DROP_HEADERS.contains(&name.to_ascii_lowercase().as_str())
}

/// Quiet time the page gets after its last response before the agent's next command reaches it.
const SETTLE: Duration = Duration::from_millis(100);
/// A page that never goes quiet (polling, streaming) holds the agent at most this long.
const SETTLE_CAP: Duration = Duration::from_secs(3);
/// How long a replayed response waits for an earlier-recorded one the page has not asked for.
const ORDER_WAIT: Duration = Duration::from_secs(1);

impl Net {
    fn new() -> Net {
        Net {
            record: None,
            site: Mutex::default(),
            delivered: Mutex::default(),
            llm: Mutex::default(),
            stop: Notify::new(),
            live_model: false,
            in_flight: AtomicUsize::new(0),
            last_answer: Mutex::new(Instant::now()),
        }
    }

    /// Wait until the page's network is quiet. The proxy calls this before each agent command.
    /// Without it, page work (responses landing, scripts reacting) races the agent's commands,
    /// and the agent sees a different page state run to run. Example: Stagehand gets Chrome
    /// DOM node ids that depend on whether a list rendered before or after it called DOM.enable.
    pub async fn settle(&self) {
        let start = Instant::now();
        while start.elapsed() < SETTLE_CAP
            && (self.in_flight.load(Ordering::SeqCst) > 0 || self.last_answer.lock().unwrap().elapsed() < SETTLE)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// An agent command that can start page work (navigate, input, script) was just sent:
    /// its requests may not be visible yet, so the next command waits a full SETTLE.
    pub fn touched(&self) {
        *self.last_answer.lock().unwrap() = Instant::now();
    }

    pub fn record(path: &str) -> Net {
        let file = std::fs::File::create(path).expect("create recording");
        Net { record: Some(Mutex::new(file)), live_model: false, ..Net::new() }
    }

    pub fn replay(path: &str, live_model: bool) -> Net {
        let (mut site, mut llm): (HashMap<String, VecDeque<Value>>, Vec<Value>) = Default::default();
        let mut seq = 0;
        for line in std::fs::read_to_string(path).expect("read recording").lines() {
            let mut e: Value = serde_json::from_str(line).unwrap();
            if e["llm"] == true {
                llm.push(e);
            } else {
                let k = key(e["method"].as_str().unwrap(), e["url"].as_str().unwrap(), e["postData"].as_str());
                e["seq"] = json!(seq);
                seq += 1;
                site.entry(k).or_default().push_back(e);
            }
        }
        let delivered = Mutex::new(vec![false; seq]);
        Net { site: Mutex::new(site), delivered, llm: Mutex::new((0, llm)), live_model, ..Net::new() }
    }

    pub fn recording(&self) -> bool {
        self.record.is_some()
    }

    fn stage(&self) -> &str {
        if self.recording() { "Response" } else { "Request" }
    }

    /// Site targets: all http(s). Record pauses after the response arrives; replay before the request leaves.
    pub fn site_patterns(&self) -> Value {
        json!([{ "urlPattern": "http://*", "requestStage": self.stage() }, { "urlPattern": "https://*", "requestStage": self.stage() }])
    }

    /// The agent's own extension (Stagehand v4): only its model calls.
    pub fn llm_patterns(&self) -> Value {
        LLM_HOSTS.iter().map(|h| json!({ "urlPattern": format!("https://{h}/*"), "requestStage": self.stage() })).collect()
    }

    pub fn save(&self, e: &Value) {
        if let Some(f) = &self.record {
            writeln!(f.lock().unwrap(), "{e}").unwrap();
        }
    }

    fn off_recording(&self, what: Value) {
        println!("{}", json!({ "event": "off-recording", "request": what }));
        self.stop.notify_one();
    }

    /// Record mode: save one model call.
    pub fn save_llm(&self, method: &str, url: &str, post: &str, status: u16, headers: Value, body: &[u8]) {
        let url = strip_key(url);
        self.save(&json!({ "llm": true, "method": method, "url": url, "postData": post, "status": status, "headers": headers, "body": B64.encode(body) }));
    }

    /// Replay mode: the next recorded model call. None = off the recording (the run stops).
    pub fn next_llm(&self, method: &str, url: &str, post: &str) -> Option<Value> {
        let url = strip_key(url);
        let (i, e) = {
            let mut llm = self.llm.lock().unwrap();
            let i = llm.0;
            llm.0 += 1;
            (i, llm.1.get(i).cloned())
        };
        let Some(e) = e else {
            self.off_recording(json!({ "llm": true, "index": i, "method": method, "url": url }));
            return None;
        };
        let want = e["postData"].as_str().unwrap_or("");
        if e["url"] != url.as_str() || want != post {
            println!("{}", json!({ "event": "llm-drift", "index": i, "url": url, "diff": first_diff(want, post) }));
        }
        println!("{}", json!({ "event": "served", "llm": true, "index": i, "url": url }));
        Some(e)
    }

    /// Replay mode: the recorded response for a site request, once it is its turn (see turn()).
    /// None = off the recording (the run stops). Call delivered() once the response is handed over.
    pub async fn next_site(&self, method: &str, url: &str, post: Option<&str>) -> Option<Value> {
        let hit = {
            let mut site = self.site.lock().unwrap();
            site.get_mut(&key(method, url, post)).map(|q| if q.len() > 1 { q.pop_front().unwrap() } else { q[0].clone() })
        };
        match &hit {
            Some(e) => {
                self.turn(e["seq"].as_u64().unwrap_or(0) as usize).await;
                println!("{}", json!({ "event": "served", "method": method, "url": url }));
            }
            None => self.off_recording(json!({ "method": method, "url": url })),
        }
        hit
    }

    /// Answer site requests in recorded order. Requests the page sends at the same moment
    /// (a fetch and a worker script) reach us in either order under load; answered as they
    /// come, the page would see its responses land in a different order run to run.
    /// ponytail: waits at most ORDER_WAIT for an earlier response the page never asks for
    /// (then gives up on it), so a page that skips a request is slower, never stuck.
    async fn turn(&self, seq: usize) {
        let start = Instant::now();
        while !self.delivered.lock().unwrap()[..seq].iter().all(|d| *d) {
            if start.elapsed() > ORDER_WAIT {
                self.delivered.lock().unwrap()[..seq].iter_mut().for_each(|d| *d = true);
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    pub fn delivered(&self, e: &Value) {
        if let Some(seq) = e["seq"].as_u64() {
            self.delivered.lock().unwrap()[seq as usize] = true;
        }
    }

    /// Handle one Fetch.requestPaused event from `session`. `llm`: it is a model call.
    pub async fn paused(&self, cdp: &Cdp, session: Option<&str>, p: &Value, llm: bool) {
        if llm {
            return self.answer(cdp, session, p, true).await;
        }
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        self.answer(cdp, session, p, false).await;
        self.touched();
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    async fn answer(&self, cdp: &Cdp, session: Option<&str>, p: &Value, llm: bool) {
        let id = &p["requestId"];
        let req = &p["request"];
        let (method, url) = (req["method"].as_str().unwrap(), req["url"].as_str().unwrap());
        let post = req["postData"].as_str();
        if self.recording() {
            let status = p["responseStatusCode"].as_u64().unwrap_or(200);
            let headers: Vec<&Value> = p["responseHeaders"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|h| keep_header(h["name"].as_str().unwrap_or("")))
                .collect();
            // Redirects have no body to read.
            let body = match cdp.call("Fetch.getResponseBody", json!({ "requestId": id }), session).await {
                Ok(b) if b["base64Encoded"] == true => B64.decode(b["body"].as_str().unwrap_or("")).unwrap_or_default(),
                Ok(b) => b["body"].as_str().unwrap_or("").as_bytes().to_vec(),
                Err(_) => Vec::new(),
            };
            let mut e = json!({ "method": method, "url": url, "status": status, "headers": headers, "body": B64.encode(&body) });
            if let Some(post) = post {
                e["postData"] = json!(post);
            }
            if let Some(err) = p["responseErrorReason"].as_str() {
                e = json!({ "method": method, "url": url, "postData": post, "error": err });
            }
            if llm {
                self.save_llm(method, url, post.unwrap_or(""), status as u16, json!(headers), &body);
            } else {
                self.save(&e);
            }
            // Deliver it the way replay will: fulfillRequest with the stored headers and body.
            // continueRequest builds the page differently, and Chrome's lazily assigned DOM node
            // ids (which Stagehand puts in its prompts) then differ between record and replay.
            let (cmd, params) = match e.get("error") {
                Some(err) => ("Fetch.failRequest", json!({ "requestId": id, "errorReason": err })),
                None => ("Fetch.fulfillRequest", json!({ "requestId": id, "responseCode": status, "responseHeaders": e["headers"], "body": e["body"] })),
            };
            cdp.call(cmd, params, session).await.ok();
            return;
        }
        let hit = if llm { self.next_llm(method, url, post.unwrap_or("")) } else { self.next_site(method, url, post).await };
        let (cmd, params) = match &hit {
            Some(e) if e.get("error").is_some() => ("Fetch.failRequest", json!({ "requestId": id, "errorReason": e["error"] })),
            Some(e) => (
                "Fetch.fulfillRequest",
                json!({ "requestId": id, "responseCode": e["status"], "responseHeaders": e["headers"], "body": e["body"] }),
            ),
            None => ("Fetch.failRequest", json!({ "requestId": id, "errorReason": "BlockedByClient" })),
        };
        cdp.call(cmd, params, session).await.ok();
        if let Some(e) = &hit {
            self.delivered(e); // only now: the next one in line must not overtake this on the wire
        }
    }
}

/// Where two strings first differ, with a little context, to explain an llm-drift event.
fn first_diff(a: &str, b: &str) -> Value {
    let i = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let at = |s: &str| {
        let (mut lo, mut hi) = (i.saturating_sub(30), (i + 30).min(s.len()));
        while !s.is_char_boundary(lo) {
            lo -= 1;
        }
        while hi < s.len() && !s.is_char_boundary(hi) {
            hi += 1;
        }
        s.get(lo..hi).unwrap_or("").to_string()
    };
    json!({ "at": i, "recorded": at(a), "now": at(b) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_api_key_from_query() {
        assert_eq!(strip_key("https://h/p?key=SECRET"), "https://h/p");
        assert_eq!(strip_key("https://h/p?alt=sse&key=SECRET&x=1"), "https://h/p?alt=sse&x=1");
        assert_eq!(strip_key("https://h/p"), "https://h/p");
    }

    #[test]
    fn first_diff_points_at_the_change() {
        assert_eq!(first_diff("/tmp/browser-use-abc/x", "/tmp/browser-use-xyz/x")["at"], 17);
    }
}
