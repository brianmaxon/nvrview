//! A small in-process mock of the Home Assistant REST API on a loopback
//! port: helper entities with a state, a bearer-token check, and a log of
//! the requests it served. The core talks to it through the real HTTP
//! client of `crates/ha-output`.

#![allow(dead_code)]

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Default)]
struct State {
    states: HashMap<String, String>,
    /// "METHOD path", plus " (bad token)" for a wrong bearer token.
    log: Vec<String>,
    token: String,
}

pub struct MockHa {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
}

fn lock(s: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    s.lock().unwrap_or_else(PoisonError::into_inner)
}

impl MockHa {
    /// Starts the mock; requests must carry `Authorization: Bearer <token>`.
    pub fn start(token: &str) -> MockHa {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            token: token.to_owned(),
            ..State::default()
        }));
        let st = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let st = st.clone();
                std::thread::spawn(move || serve(stream, &st));
            }
        });
        MockHa { addr, state }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn set(&self, entity: &str, state: &str) {
        lock(&self.state)
            .states
            .insert(entity.to_owned(), state.to_owned());
    }

    pub fn state_of(&self, entity: &str) -> Option<String> {
        lock(&self.state).states.get(entity).cloned()
    }

    pub fn log(&self) -> Vec<String> {
        lock(&self.state).log.clone()
    }

    /// How many times the entity was turned on.
    pub fn turn_ons(&self, entity: &str) -> usize {
        self.log()
            .iter()
            .filter(|l| l.as_str() == format!("POST /api/services/input_boolean/turn_on {entity}"))
            .count()
    }
}

fn serve(stream: TcpStream, st: &Mutex<State>) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let (mut auth, mut len) = (String::new(), 0usize);
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() {
            return;
        }
        let h = h.trim_end().to_owned();
        if h.is_empty() {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            auth = h["authorization:".len()..].trim().to_owned();
        }
    }
    let mut body = vec![0; len];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let entity = body["entity_id"].as_str().unwrap_or("").to_owned();
    let (status, reply) = {
        let mut s = lock(st);
        let authed = auth == format!("Bearer {}", s.token);
        let tag = if method == "POST" && !entity.is_empty() {
            format!("{method} {path} {entity}")
        } else {
            format!("{method} {path}")
        };
        s.log
            .push(format!("{tag}{}", if authed { "" } else { " (bad token)" }));
        if !authed {
            (401, json!({"message": "401: Unauthorized"}))
        } else if let Some(id) = path.strip_prefix("/api/states/") {
            match s.states.get(id) {
                Some(v) => (200, json!({"entity_id": id, "state": v})),
                None => (404, json!({"message": "Entity not found."})),
            }
        } else if let Some(svc) = path.strip_prefix("/api/services/") {
            match svc.rsplit_once('/') {
                Some((_, "turn_on")) if s.states.contains_key(&entity) => {
                    s.states.insert(entity.clone(), "on".into());
                    (200, json!([{"entity_id": entity, "state": "on"}]))
                }
                Some((_, "turn_off")) if s.states.contains_key(&entity) => {
                    s.states.insert(entity.clone(), "off".into());
                    (200, json!([{"entity_id": entity, "state": "off"}]))
                }
                _ => (404, json!({"message": "Service not found."})),
            }
        } else {
            (404, json!({}))
        }
    };
    let text = reply.to_string();
    let mut out = stream;
    let _ = write!(
        out,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
}
