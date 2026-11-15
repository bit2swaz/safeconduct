//! Fuzz mode: seeded faults on a replayed site, with a live model.
//!
//! Each fault is decided by hashing (seed, request, how many times this request was seen), never
//! by timing, so one seed gives the same faults in every run, whatever order requests arrive in.
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

pub struct Fuzz {
    pub seed: u64,
    /// Share of requests that get a fault, 0..=1.
    pub rate: f64,
    seen: Mutex<HashMap<String, u64>>,
}

/// What to do with one site request.
pub enum Fault {
    None,
    Delay(Duration),
    /// HTTP 503.
    Error,
    /// The connection fails.
    Drop,
    /// A modal overlay over the page (also a cookie wall, a newsletter box).
    Popup,
    /// Text injected at the top of the page; it also pushes the layout down.
    Banner,
}

impl Fault {
    fn name(&self) -> &str {
        match self {
            Fault::None => "none",
            Fault::Delay(_) => "delay",
            Fault::Error => "error",
            Fault::Drop => "drop",
            Fault::Popup => "popup",
            Fault::Banner => "banner",
        }
    }
}

/// FNV-1a. Not std's DefaultHasher: that may change between Rust versions, and a seed must
/// keep its fault schedule forever (runs are compared by it).
fn hash(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for p in parts {
        for b in p.iter().chain(&[0xff]) {
            h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
        }
    }
    h
}

impl Fuzz {
    pub fn new(seed: u64, rate: f64) -> Fuzz {
        Fuzz { seed, rate, seen: Mutex::default() }
    }

    /// The fault for this request. `document`: the response is an HTML page.
    /// `recorded`: false = not in the recording, which always gets a fault (503 or drop).
    pub fn fault(&self, key: &str, document: bool, recorded: bool) -> Fault {
        let n = {
            let mut seen = self.seen.lock().unwrap();
            let n = seen.entry(key.to_string()).or_default();
            *n += 1;
            *n
        };
        let h = hash(&[&self.seed.to_le_bytes(), key.as_bytes(), &n.to_le_bytes()]);
        let pick = (h >> 32) % 3;
        let delay = Duration::from_millis(200 + (h >> 40) % 1300);
        let fault = if !recorded {
            if pick == 0 { Fault::Drop } else { Fault::Error }
        } else if (h % 10_000) as f64 >= self.rate * 10_000.0 {
            Fault::None
        } else if document {
            // Not 503 or drop: a page that never loads ends the run instead of testing the agent.
            [Fault::Delay(delay), Fault::Popup, Fault::Banner].into_iter().nth(pick as usize).unwrap()
        } else {
            [Fault::Delay(delay), Fault::Error, Fault::Drop].into_iter().nth(pick as usize).unwrap()
        };
        if !matches!(fault, Fault::None) {
            let url = key.split('\n').next().unwrap_or("");
            let mut e = json!({ "event": "fault", "kind": fault.name(), "request": url, "nth": n });
            if let Fault::Delay(d) = fault {
                e["ms"] = json!(d.as_millis());
            }
            println!("{e}");
        }
        fault
    }
}

const POPUP: &str = r#"<div id="sc-fault-popup" style="position:fixed;inset:0;z-index:2147483647;background:rgba(0,0,0,.6);display:flex;align-items:center;justify-content:center"><div style="background:#fff;padding:24px;max-width:320px;font:16px sans-serif">We use cookies to improve your experience. <button onclick="this.closest('#sc-fault-popup').remove()">Accept</button></div></div>"#;
const BANNER: &str = r#"<div id="sc-fault-banner" style="padding:48px 16px;background:#fff3c4;font:18px sans-serif">Limited offer: free shipping on all orders today only.</div>"#;

/// Put a popup or banner at the top of an HTML page: after <body ...>, else after <!doctype ...>.
pub fn inject(html: &str, fault: &Fault) -> String {
    let snippet = match fault {
        Fault::Popup => POPUP,
        Fault::Banner => BANNER,
        _ => return html.to_string(),
    };
    let lower = html.to_ascii_lowercase();
    let at = match lower.find("<body") {
        Some(i) => lower[i..].find('>').map_or(0, |j| i + j + 1),
        None if lower.trim_start().starts_with("<!doctype") => lower.find('>').map_or(0, |j| j + 1),
        None => 0,
    };
    format!("{}{snippet}{}", &html[..at], &html[at..])
}

/// Whether a recorded response is an HTML page.
pub fn is_html(e: &Value) -> bool {
    e["headers"].as_array().into_iter().flatten().any(|h| {
        h["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case("content-type"))
            && h["value"].as_str().is_some_and(|v| v.contains("text/html"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_faults_whatever_the_order() {
        let keys = ["GET http://s/a\n", "GET http://s/b\n", "POST http://s/c\nx=1"];
        let run = |order: &[usize]| {
            let f = Fuzz::new(7, 0.5);
            let mut out = vec![String::new(); keys.len()];
            for &i in order {
                out[i] = f.fault(keys[i], false, true).name().to_string();
            }
            out
        };
        assert_eq!(run(&[0, 1, 2]), run(&[2, 0, 1]));
    }

    #[test]
    fn schedule_is_pinned() {
        // If this changes, every saved seed means something else: only change it on purpose.
        assert_eq!(hash(&[b"safeconduct"]), 0x122e1111051c0245); // FNV-1a of "safeconduct" + 0xff
        assert_ne!(hash(&[b"a", b"b"]), hash(&[b"ab"]));
    }

    #[test]
    fn unrecorded_requests_always_fault() {
        let f = Fuzz::new(1, 0.0);
        for i in 0..20 {
            assert!(matches!(f.fault(&format!("GET http://s/{i}\n"), false, false), Fault::Error | Fault::Drop));
        }
    }

    #[test]
    fn injects_after_body_or_doctype() {
        assert!(inject("<html><body class=x><p>hi", &Fault::Banner).starts_with("<html><body class=x><div id=\"sc-fault-banner\""));
        assert!(inject("<!doctype html>\n<title>t</title>", &Fault::Popup).starts_with("<!doctype html><div id=\"sc-fault-popup\""));
    }
}
