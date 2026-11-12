//! Runtime falso para las pruebas del adaptador declarativo (P023).
//!
//! - `--version`: imprime `fake-runtime 1.2.3`.
//! - `--json-version`: imprime `{"version":"1.2.3","build":{"number":7}}`.
//! - `--hang`: no termina (sondeo que vence).
//! - `--serve --port N --dump F [--health-status S] [--model M] [...]`:
//!   escribe en `F` sus argumentos, su entorno y su carpeta de trabajo y sirve
//!   `GET /health` (`{"status":"ok"}` con el estado S, 200 por defecto) y
//!   `POST /shutdown` (responde y termina) en 127.0.0.1:N.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;

fn value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("fake-runtime 1.2.3");
        return;
    }
    if args.iter().any(|a| a == "--json-version") {
        println!(r#"{{"version":"1.2.3","build":{{"number":7}}}}"#);
        return;
    }
    if args.iter().any(|a| a == "--hang") {
        std::thread::sleep(std::time::Duration::from_secs(600));
        return;
    }
    let port: u16 = value(&args, "--port")
        .and_then(|p| p.parse().ok())
        .expect("--port");
    let status: u16 = value(&args, "--health-status")
        .and_then(|p| p.parse().ok())
        .unwrap_or(200);
    if let Some(dump) = value(&args, "--dump") {
        let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let cwd = std::env::current_dir().unwrap().display().to_string();
        let text =
            serde_json::json!({"args": args, "env": env, "cwd": cwd, "pid": std::process::id()});
        std::fs::write(dump, text.to_string()).unwrap();
    }
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let line = request.lines().next().unwrap_or_default().to_string();
        let (code, body, exit) = if line.starts_with("GET /health") {
            (status, r#"{"status":"ok"}"#, false)
        } else if line.starts_with("POST /shutdown") {
            (200, r#"{"stopping":true}"#, true)
        } else {
            (404, "{}", false)
        };
        let _ = write!(
            stream,
            "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.flush();
        if exit {
            std::process::exit(0);
        }
    }
}
