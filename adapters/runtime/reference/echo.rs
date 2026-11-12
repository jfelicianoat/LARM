//! Proceso HTTP de referencia P025. Echo real, sin inferencia ni modelo IA.
#![forbid(unsafe_code)]
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn respond(stream: &mut TcpStream, code: u16, body: serde_json::Value) -> std::io::Result<()> {
    let data = serde_json::to_vec(&body)?;
    write!(stream, "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", data.len())?;
    stream.write_all(&data)?;
    stream.flush()
}

fn request(stream: &mut TcpStream) -> std::io::Result<bool> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(at) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            break at + 4;
        }
        if bytes.len() > 16 * 1024 {
            respond(stream, 431, json!({"error":"headers_too_large"}))?;
            return Ok(false);
        }
        let mut chunk = [0; 1024];
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(false);
        }
        bytes.extend_from_slice(&chunk[..n]);
    };
    let header = std::str::from_utf8(&bytes[..header_end]).map_err(std::io::Error::other)?;
    let mut lines = header.split("\r\n");
    let line: Vec<_> = lines.next().unwrap_or_default().split(' ').collect();
    if line.len() != 3 || line[2] != "HTTP/1.1" {
        respond(stream, 400, json!({"error":"invalid_request"}))?;
        return Ok(false);
    }
    let (method, path) = (line[0].to_string(), line[1].to_string());
    let mut length = None;
    for field in lines.filter(|line| !line.is_empty()) {
        let Some((name, value)) = field.split_once(':') else {
            respond(stream, 400, json!({"error":"invalid_header"}))?;
            return Ok(false);
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            respond(
                stream,
                400,
                json!({"error":"transfer_encoding_unsupported"}),
            )?;
            return Ok(false);
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                respond(stream, 400, json!({"error":"duplicate_content_length"}))?;
                return Ok(false);
            }
            let Ok(value) = value.trim().parse::<usize>() else {
                respond(stream, 400, json!({"error":"invalid_content_length"}))?;
                return Ok(false);
            };
            length = Some(value);
        }
    }
    let length = length.unwrap_or(0);
    if length > 1 << 20 {
        respond(stream, 413, json!({"error":"body_too_large"}))?;
        return Ok(false);
    }
    while bytes.len() - header_end < length {
        let mut chunk = [0; 8192];
        let remaining = (length - (bytes.len() - header_end)).min(chunk.len());
        let n = stream.read(&mut chunk[..remaining])?;
        if n == 0 {
            return Ok(false);
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => respond(stream, 200, json!({"status":"ok"}))?,
        ("POST", "/shutdown") => {
            respond(stream, 200, json!({"stopping":true}))?;
            return Ok(true);
        }
        ("POST", "/echo") => match serde_json::from_slice::<serde_json::Value>(
            &bytes[header_end..header_end + length],
        ) {
            Ok(value) => respond(stream, 200, json!({"echo":value}))?,
            Err(_) => respond(stream, 400, json!({"error":"invalid_json"}))?,
        },
        _ => respond(stream, 404, json!({"error":"not_found"}))?,
    }
    Ok(false)
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        if args.iter().any(|arg| arg == "--version-flood") {
            let _ = std::io::stdout().write_all(&vec![b'x'; 2 << 20]);
            return ExitCode::SUCCESS;
        }
        let delay = arg(&args, "--version-delay-ms")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            .min(60_000);
        std::thread::sleep(Duration::from_millis(delay));
        println!("reference-echo 1.0.0");
        return ExitCode::SUCCESS;
    }
    let Some(port) = arg(&args, "--port").and_then(|p| p.parse::<u16>().ok()) else {
        eprintln!("reference-echo: --port requerido");
        return ExitCode::from(2);
    };
    let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) else {
        eprintln!("reference-echo: no se pudo abrir el puerto loopback");
        return ExitCode::from(2);
    };
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else {
            eprintln!("reference-echo: fallo al aceptar conexión");
            return ExitCode::from(1);
        };
        if request(&mut stream).unwrap_or(false) {
            return ExitCode::SUCCESS;
        }
    }
    ExitCode::SUCCESS
}
