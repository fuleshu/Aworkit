use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use serde_json::Value;

pub struct FixtureRequest {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

pub struct FixtureResponse {
    status: u16,
    body: Vec<u8>,
}

impl FixtureResponse {
    pub fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).expect("fixture response JSON"),
        }
    }

    #[allow(dead_code)]
    pub fn raw(body: Vec<u8>) -> Self {
        Self { status: 200, body }
    }
}

pub fn start_fixture(
    requests: usize,
    handler: impl Fn(usize, FixtureRequest) -> FixtureResponse + Send + Sync + 'static,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider fixture");
    let address = listener.local_addr().expect("provider fixture address");
    let handler = Arc::new(handler);
    let handle = thread::spawn(move || {
        for index in 0..requests {
            let (mut stream, _) = listener.accept().expect("fixture connection");
            let request = read_request(&mut stream);
            let response = handler(index, request);
            let reason = if response.status < 300 {
                "OK"
            } else {
                "Response"
            };
            let header = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.status,
                reason,
                response.body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&response.body);
        }
    });
    (format!("http://{address}"), handle)
}

fn read_request(stream: &mut TcpStream) -> FixtureRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("fixture read timeout");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).expect("fixture request");
        assert_ne!(read, 0, "request ended before headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break offset + 4;
        }
    };
    let header_text = String::from_utf8(bytes[..header_end].to_vec()).expect("request headers");
    let mut lines = header_text.split("\r\n");
    let mut request_line = lines.next().expect("request line").split_whitespace();
    let method = request_line.next().expect("request method").to_owned();
    let path = request_line.next().expect("request path").to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let content_length = headers
        .get("content-length")
        .map_or(0, |value| value.parse::<usize>().expect("content length"));
    while bytes.len().saturating_sub(header_end) < content_length {
        let read = stream.read(&mut buffer).expect("fixture request body");
        assert_ne!(read, 0, "request ended before body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    FixtureRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}
