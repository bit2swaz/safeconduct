//! Network record and replay through the Fetch domain, enabled per site target (see setup()).
//! Not on the browser session: that would also catch the agent's own extension, and Stagehand v4
//! calls its model from there, so its API key and prompts would land in the recording.
//!
//! The recording is JSONL, one response per line:
//!   {"method","url","postData"?, "status","headers","body"(base64)}  or  {"method","url","postData"?, "error"}
use crate::cdp::Cdp;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::Mutex;

pub enum Net {
    Record(Mutex<std::fs::File>),
    /// Responses per request key, served in recorded order. The last one repeats.
    Replay(Mutex<HashMap<String, VecDeque<Value>>>),
}

// Chrome hands us the decoded body, so these would describe bytes we no longer have.
const DROP_HEADERS: [&str; 3] = ["content-encoding", "content-length", "transfer-encoding"];

fn key(method: &str, url: &str, post: Option<&str>) -> String {
    format!("{method} {url}\n{}", post.unwrap_or(""))
}

impl Net {
    pub fn record(path: &str) -> Net {
        Net::Record(Mutex::new(std::fs::File::create(path).expect("create recording")))
    }

    pub fn replay(path: &str) -> Net {
        let mut map: HashMap<String, VecDeque<Value>> = HashMap::new();
        for line in std::fs::read_to_string(path).expect("read recording").lines() {
            let e: Value = serde_json::from_str(line).unwrap();
            let k = key(e["method"].as_str().unwrap(), e["url"].as_str().unwrap(), e["postData"].as_str());
            map.entry(k).or_default().push_back(e);
        }
        Net::Replay(Mutex::new(map))
    }

    /// Record pauses after the response arrives; replay pauses before the request leaves.
    pub fn fetch_patterns(&self) -> Value {
        let stage = if matches!(self, Net::Record(_)) { "Response" } else { "Request" };
        json!([{ "urlPattern": "http://*", "requestStage": stage }, { "urlPattern": "https://*", "requestStage": stage }])
    }

    /// Handle one Fetch.requestPaused event from `session`.
    pub async fn paused(&self, cdp: &Cdp, session: Option<&str>, p: &Value) {
        let id = &p["requestId"];
        let req = &p["request"];
        let (method, url) = (req["method"].as_str().unwrap(), req["url"].as_str().unwrap());
        let post = req["postData"].as_str();
        match self {
            Net::Record(file) => {
                let mut e = json!({ "method": method, "url": url });
                if let Some(post) = post {
                    e["postData"] = json!(post);
                }
                if let Some(err) = p["responseErrorReason"].as_str() {
                    e["error"] = json!(err);
                } else {
                    let status = p["responseStatusCode"].as_u64().unwrap_or(200);
                    let headers: Vec<&Value> = p["responseHeaders"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|h| !DROP_HEADERS.contains(&h["name"].as_str().unwrap_or("").to_ascii_lowercase().as_str()))
                        .collect();
                    // Redirects have no body to read.
                    let body = match cdp.call("Fetch.getResponseBody", json!({ "requestId": id }), session).await {
                        Ok(b) if b["base64Encoded"] == true => b["body"].as_str().unwrap_or("").to_string(),
                        Ok(b) => B64.encode(b["body"].as_str().unwrap_or("")),
                        Err(_) => String::new(),
                    };
                    e["status"] = json!(status);
                    e["headers"] = json!(headers);
                    e["body"] = json!(body);
                }
                writeln!(file.lock().unwrap(), "{e}").unwrap();
                cdp.call("Fetch.continueRequest", json!({ "requestId": id }), session).await.ok();
            }
            Net::Replay(map) => {
                let hit = {
                    let mut map = map.lock().unwrap();
                    map.get_mut(&key(method, url, post)).map(|q| {
                        if q.len() > 1 { q.pop_front().unwrap() } else { q[0].clone() }
                    })
                };
                if hit.is_some() {
                    println!("{}", json!({ "event": "served", "method": method, "url": url }));
                }
                let (cmd, params) = match hit {
                    Some(e) if e.get("error").is_some() => {
                        ("Fetch.failRequest", json!({ "requestId": id, "errorReason": e["error"] }))
                    }
                    Some(e) => (
                        "Fetch.fulfillRequest",
                        json!({ "requestId": id, "responseCode": e["status"], "responseHeaders": e["headers"], "body": e["body"] }),
                    ),
                    None => {
                        // ponytail: v0 fails the request and reports it. Exact mode (B1b) stops the run;
                        // fuzz mode returns a seeded fault.
                        println!("{}", json!({ "event": "off-recording", "method": method, "url": url }));
                        ("Fetch.failRequest", json!({ "requestId": id, "errorReason": "BlockedByClient" }))
                    }
                };
                cdp.call(cmd, params, session).await.ok();
            }
        }
    }
}
