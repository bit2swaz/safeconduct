//! A minimal CDP client: one WebSocket, replies matched by id, everything else is an event.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

pub struct Cdp {
    tx: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    next: AtomicU64,
}

impl Cdp {
    /// Connect to a browser endpoint. Events arrive on the returned receiver.
    pub async fn connect(url: &str) -> (Arc<Cdp>, mpsc::UnboundedReceiver<Value>) {
        let (ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect to chrome");
        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (events_tx, events) = mpsc::unbounded_channel();
        let cdp = Arc::new(Cdp { tx, pending: Mutex::default(), next: AtomicU64::new(1) });
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if sink.send(Message::text(m)).await.is_err() {
                    break;
                }
            }
        });
        let reader = cdp.clone();
        tokio::spawn(async move {
            while let Some(Ok(Message::Text(t))) = stream.next().await {
                let msg: Value = serde_json::from_str(&t).unwrap();
                match msg["id"].as_u64() {
                    Some(id) => {
                        if let Some(reply) = reader.pending.lock().unwrap().remove(&id) {
                            reply.send(msg).ok();
                        }
                    }
                    None => {
                        events_tx.send(msg).ok();
                    }
                }
            }
        });
        (cdp, events)
    }

    /// Send a command and wait for its reply. `session` None = the browser target.
    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value, Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.tx.send(msg.to_string()).map_err(|_| json!("connection closed"))?;
        let reply = rx.await.map_err(|_| json!("connection closed"))?;
        match reply.get("error") {
            Some(e) => Err(e.clone()),
            None => Ok(reply["result"].clone()),
        }
    }
}
