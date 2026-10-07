//! Stub-Server für den curl-Pfad (nur Tests).
//!
//! Ein echter `curl`-Prozess gegen einen lokalen `TcpListener`: Nur so ist
//! sichtbar, was wirklich über die Leitung geht — die Aufteilung von stdin
//! zwischen Header und Body war genau der Fehler, den ein Mock der eigenen
//! Abstraktion nie gefunden hätte (#7).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, channel};

/// Ein empfangener Request.
pub(crate) struct Received {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<String>,
    pub(crate) body: String,
}

/// Startet einen Server, der die `responses` der Reihe nach ausliefert und
/// jeden empfangenen Request in den Kanal legt. Nach der letzten Antwort
/// endet der Thread; der Kanal meldet dann `Err` — „kein weiterer Request".
pub(crate) fn stub_server(responses: Vec<(u16, String)>) -> (String, Receiver<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let request = read_request(&mut stream);
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = sender.send(request);
        }
    });
    (format!("http://{address}"), receiver)
}

fn read_request(stream: &mut TcpStream) -> Received {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
        headers.push(line);
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).unwrap();
    Received {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}
