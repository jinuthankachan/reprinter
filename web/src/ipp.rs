//! Minimal IPP client for reading state from the local CUPS scheduler.
//!
//! Speaks IPP/1.1 (RFC 8010) over HTTP on the scheduler's Unix domain socket.
//! Over that socket CUPS accepts `Authorization: PeerCred <user>` and checks
//! it against the socket's peer credentials, so a service user in the
//! `lpadmin` group sees all job details without a password.
//!
//! Only what the web interface reads is implemented: building requests with
//! simple attribute types, and decoding responses into groups of attributes.
//! Changes go through the CUPS command-line tools instead (see `cups.rs`).

use std::{path::PathBuf, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

pub const GET_JOBS: u16 = 0x000A;
pub const CUPS_GET_DEFAULT: u16 = 0x4001;
pub const CUPS_GET_PRINTERS: u16 = 0x4002;

pub const STATUS_NOT_FOUND: u16 = 0x0406;

pub const GROUP_OPERATION: u8 = 0x01;
pub const GROUP_JOB: u8 = 0x02;
const GROUP_END: u8 = 0x03;
pub const GROUP_PRINTER: u8 = 0x04;

const TAG_INTEGER: u8 = 0x21;
const TAG_BOOLEAN: u8 = 0x22;
const TAG_ENUM: u8 = 0x23;
const TAG_TEXT_WITH_LANGUAGE: u8 = 0x35;
const TAG_NAME_WITH_LANGUAGE: u8 = 0x36;
const TAG_NAME: u8 = 0x42;
const TAG_KEYWORD: u8 = 0x44;
const TAG_URI: u8 = 0x45;
const TAG_CHARSET: u8 = 0x47;
const TAG_LANGUAGE: u8 = 0x48;

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i32),
    Bool(bool),
    Str(String),
    /// Types the web interface does not use (dates, ranges, collections, ...).
    Other,
}

/// An IPP request under construction. Attributes go into the operation group.
pub struct Request {
    buf: Vec<u8>,
}

impl Request {
    pub fn new(operation: u16) -> Self {
        let mut buf = Vec::with_capacity(256);
        buf.extend_from_slice(&[1, 1]); // IPP/1.1
        buf.extend_from_slice(&operation.to_be_bytes());
        buf.extend_from_slice(&1u32.to_be_bytes()); // request-id
        buf.push(GROUP_OPERATION);
        Self { buf }
            .attr(TAG_CHARSET, "attributes-charset", b"utf-8")
            .attr(TAG_LANGUAGE, "attributes-natural-language", b"en")
    }

    fn attr(mut self, tag: u8, name: &str, value: &[u8]) -> Self {
        self.buf.push(tag);
        self.buf
            .extend_from_slice(&(name.len() as u16).to_be_bytes());
        self.buf.extend_from_slice(name.as_bytes());
        self.buf
            .extend_from_slice(&(value.len() as u16).to_be_bytes());
        self.buf.extend_from_slice(value);
        self
    }

    pub fn uri(self, name: &str, value: &str) -> Self {
        self.attr(TAG_URI, name, value.as_bytes())
    }

    pub fn name(self, name: &str, value: &str) -> Self {
        self.attr(TAG_NAME, name, value.as_bytes())
    }

    pub fn keyword(self, name: &str, value: &str) -> Self {
        self.attr(TAG_KEYWORD, name, value.as_bytes())
    }

    pub fn boolean(self, name: &str, value: bool) -> Self {
        self.attr(TAG_BOOLEAN, name, &[value as u8])
    }

    /// A 1setOf keyword: the first value carries the name, the rest an empty one.
    pub fn keywords(mut self, name: &str, values: &[&str]) -> Self {
        for (i, v) in values.iter().enumerate() {
            self = self.attr(TAG_KEYWORD, if i == 0 { name } else { "" }, v.as_bytes());
        }
        self
    }

    fn finish(mut self) -> Vec<u8> {
        self.buf.push(GROUP_END);
        self.buf
    }
}

#[derive(Debug, Default)]
pub struct Group {
    pub tag: u8,
    pub attrs: Vec<(String, Vec<Value>)>,
}

impl Group {
    pub fn values(&self, name: &str) -> &[Value] {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn str(&self, name: &str) -> Option<&str> {
        match self.values(name).first() {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }

    pub fn string(&self, name: &str) -> String {
        self.str(name).unwrap_or_default().to_string()
    }

    pub fn strings(&self, name: &str) -> Vec<String> {
        self.values(name)
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn int(&self, name: &str) -> Option<i32> {
        match self.values(name).first() {
            Some(Value::Int(i)) => Some(*i),
            _ => None,
        }
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.values(name).first() {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub groups: Vec<Group>,
}

impl Response {
    pub fn ok(&self) -> bool {
        self.status < 0x0100
    }

    /// The scheduler's explanation for a failed request.
    pub fn message(&self) -> String {
        self.groups
            .iter()
            .find(|g| g.tag == GROUP_OPERATION)
            .and_then(|g| g.str("status-message"))
            .map(str::to_string)
            .unwrap_or_else(|| format!("IPP status 0x{:04x}", self.status))
    }

    pub fn groups(&self, tag: u8) -> impl Iterator<Item = &Group> {
        self.groups.iter().filter(move |g| g.tag == tag)
    }
}

fn read_u16(data: &[u8], pos: &mut usize) -> Result<u16, String> {
    let bytes = data.get(*pos..*pos + 2).ok_or("truncated IPP response")?;
    *pos += 2;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn take<'a>(data: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], String> {
    let bytes = data.get(*pos..*pos + len).ok_or("truncated IPP response")?;
    *pos += len;
    Ok(bytes)
}

fn decode(tag: u8, value: &[u8]) -> Value {
    match tag {
        TAG_INTEGER | TAG_ENUM if value.len() == 4 => {
            Value::Int(i32::from_be_bytes([value[0], value[1], value[2], value[3]]))
        }
        TAG_BOOLEAN if value.len() == 1 => Value::Bool(value[0] != 0),
        TAG_TEXT_WITH_LANGUAGE | TAG_NAME_WITH_LANGUAGE => {
            // u16 language length, language, u16 text length, text
            let mut pos = 0;
            let text = read_u16(value, &mut pos)
                .and_then(|n| take(value, &mut pos, n as usize).map(|_| ()))
                .and_then(|_| read_u16(value, &mut pos))
                .and_then(|n| take(value, &mut pos, n as usize));
            match text {
                Ok(t) => Value::Str(String::from_utf8_lossy(t).into_owned()),
                Err(_) => Value::Other,
            }
        }
        // All character-string types: text, name, keyword, uri, charset, ...
        0x40..=0x5F => Value::Str(String::from_utf8_lossy(value).into_owned()),
        _ => Value::Other,
    }
}

pub fn parse(data: &[u8]) -> Result<Response, String> {
    if data.len() < 9 {
        return Err("truncated IPP response".into());
    }
    let status = u16::from_be_bytes([data[2], data[3]]);
    let mut pos = 8;
    let mut groups = Vec::new();
    let mut current: Option<Group> = None;
    loop {
        let tag = *data.get(pos).ok_or("truncated IPP response")?;
        pos += 1;
        if tag <= 0x0F {
            // Delimiter: ends the previous group, starts a new one or ends the message.
            if let Some(g) = current.take() {
                groups.push(g);
            }
            if tag == GROUP_END {
                break;
            }
            current = Some(Group {
                tag,
                attrs: Vec::new(),
            });
            continue;
        }
        let name_len = read_u16(data, &mut pos)? as usize;
        let name = take(data, &mut pos, name_len)?;
        let value_len = read_u16(data, &mut pos)? as usize;
        let value = decode(tag, take(data, &mut pos, value_len)?);
        let group = current.get_or_insert_with(Group::default);
        if name_len == 0 {
            // Additional value of the previous attribute (or a collection member).
            if let Some((_, values)) = group.attrs.last_mut() {
                values.push(value);
            }
        } else {
            group
                .attrs
                .push((String::from_utf8_lossy(name).into_owned(), vec![value]));
        }
    }
    Ok(Response { status, groups })
}

/// Splits an HTTP/1.1 response into status code and (de-chunked) body.
fn parse_http(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let end = find(raw, b"\r\n\r\n").ok_or("malformed HTTP response from CUPS")?;
    let head = String::from_utf8_lossy(&raw[..end]);
    let body = &raw[end + 4..];
    let mut lines = head.split("\r\n");
    let code = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or("malformed HTTP status line from CUPS")?;
    let (mut chunked, mut length) = (false, None);
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if k == "content-length" {
                length = v.parse::<usize>().ok();
            }
        }
    }
    let body = if chunked {
        dechunk(body)?
    } else if let Some(n) = length {
        body[..n.min(body.len())].to_vec()
    } else {
        body.to_vec()
    };
    Ok((code, body))
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = find(data, b"\r\n").ok_or("malformed chunked response from CUPS")?;
        let size = String::from_utf8_lossy(&data[..eol]);
        let size = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "malformed chunk size from CUPS")?;
        data = &data[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        let chunk = data.get(..size).ok_or("truncated chunk from CUPS")?;
        out.extend_from_slice(chunk);
        data = &data[size..];
        data = data.strip_prefix(b"\r\n").unwrap_or(data);
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub struct Client {
    socket: PathBuf,
    user: String,
}

impl Client {
    pub fn new(socket: PathBuf, user: String) -> Self {
        Self { socket, user }
    }

    /// Sends a request to `path` ("/", "/printers/NAME", ...). The response
    /// may carry an IPP error status; callers decide what that means.
    pub async fn call(&self, path: &str, request: Request) -> Result<Response, String> {
        let body = request.name("requesting-user-name", &self.user).finish();
        let (mut code, mut data) = self.post(path, &body, true).await?;
        if code == 401 {
            // PeerCred refused (e.g. disabled in cupsd.conf): read anonymously.
            (code, data) = self.post(path, &body, false).await?;
        }
        if code != 200 {
            return Err(format!("CUPS answered HTTP {code}"));
        }
        parse(&data)
    }

    async fn post(
        &self,
        path: &str,
        body: &[u8],
        peer_cred: bool,
    ) -> Result<(u16, Vec<u8>), String> {
        let exchange = async {
            let mut stream = UnixStream::connect(&self.socket).await.map_err(|e| {
                format!(
                    "cannot reach CUPS at {}: {e} (is cupsd running?)",
                    self.socket.display()
                )
            })?;
            let mut head = format!(
                "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/ipp\r\n\
                 Content-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            if peer_cred {
                head.push_str(&format!("Authorization: PeerCred {}\r\n", self.user));
            }
            head.push_str("\r\n");
            let io = |e: std::io::Error| format!("talking to CUPS: {e}");
            stream.write_all(head.as_bytes()).await.map_err(io)?;
            stream.write_all(body).await.map_err(io)?;
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).await.map_err(io)?;
            parse_http(&raw)
        };
        tokio::time::timeout(TIMEOUT, exchange)
            .await
            .map_err(|_| "CUPS did not answer in time".to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes a response the way the scheduler does, using the request builder
    /// for the attribute encoding.
    fn response(status: u16, groups: &[(u8, Request)]) -> Vec<u8> {
        let mut out = vec![1, 1];
        out.extend_from_slice(&status.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        for (tag, req) in groups {
            out.push(*tag);
            // Skip the builder's own header (8 bytes), operation tag and the
            // charset/language attributes it always adds.
            let skip = 8 + 1 + (1 + 2 + 18 + 2 + 5) + (1 + 2 + 27 + 2 + 2);
            out.extend_from_slice(&req.buf[skip..]);
        }
        out.push(GROUP_END);
        out
    }

    #[test]
    fn request_encoding() {
        let body = Request::new(GET_JOBS)
            .keyword("which-jobs", "completed")
            .finish();
        assert_eq!(&body[..8], &[1, 1, 0x00, 0x0A, 0, 0, 0, 1]);
        assert_eq!(body[8], GROUP_OPERATION);
        assert_eq!(body[9], TAG_CHARSET);
        assert_eq!(*body.last().unwrap(), GROUP_END);
        // The builder's output parses back to the same attributes.
        let parsed = parse(&body).unwrap();
        let op = &parsed.groups[0];
        assert_eq!(op.str("attributes-charset"), Some("utf-8"));
        assert_eq!(op.str("which-jobs"), Some("completed"));
    }

    #[test]
    fn parses_groups_multi_values_and_types() {
        let printer1 = Request::new(0)
            .name("printer-name", "LBP1210")
            .boolean("printer-is-shared", true)
            .keywords("printer-state-reasons", &["none", "paused"]);
        let mut printer2 = Request::new(0).name("printer-name", "Other");
        printer2 = printer2.attr(TAG_ENUM, "printer-state", &5i32.to_be_bytes());
        // nameWithLanguage: "en" + "Büro"
        let mut nwl = vec![0, 2, b'e', b'n'];
        let text = "Büro".as_bytes();
        nwl.extend_from_slice(&(text.len() as u16).to_be_bytes());
        nwl.extend_from_slice(text);
        printer2 = printer2.attr(TAG_NAME_WITH_LANGUAGE, "printer-location", &nwl);
        printer2 = printer2.attr(0x31, "printer-current-time", &[0; 11]); // dateTime

        let data = response(
            0,
            &[
                (GROUP_OPERATION, Request::new(0)),
                (GROUP_PRINTER, printer1),
                (GROUP_PRINTER, printer2),
            ],
        );
        let r = parse(&data).unwrap();
        assert!(r.ok());
        let printers: Vec<_> = r.groups(GROUP_PRINTER).collect();
        assert_eq!(printers.len(), 2);
        assert_eq!(printers[0].str("printer-name"), Some("LBP1210"));
        assert_eq!(printers[0].bool("printer-is-shared"), Some(true));
        assert_eq!(
            printers[0].strings("printer-state-reasons"),
            vec!["none", "paused"]
        );
        assert_eq!(printers[1].int("printer-state"), Some(5));
        assert_eq!(printers[1].str("printer-location"), Some("Büro"));
        assert_eq!(printers[1].values("printer-current-time"), &[Value::Other]);
    }

    #[test]
    fn error_status_and_message() {
        let op = Request::new(0).attr(0x41, "status-message", b"No destinations added.");
        let r = parse(&response(STATUS_NOT_FOUND, &[(GROUP_OPERATION, op)])).unwrap();
        assert!(!r.ok());
        assert_eq!(r.message(), "No destinations added.");
    }

    #[test]
    fn truncated_input_is_an_error() {
        let mut data = response(
            0,
            &[(GROUP_PRINTER, Request::new(0).name("printer-name", "X"))],
        );
        data.truncate(data.len() - 4);
        assert!(parse(&data).is_err());
    }

    #[test]
    fn http_content_length_and_chunked() {
        let (code, body) =
            parse_http(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcdef").unwrap();
        assert_eq!((code, body.as_slice()), (200, b"abc".as_slice()));
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n";
        let (code, body) = parse_http(raw).unwrap();
        assert_eq!((code, body.as_slice()), (200, b"abcde".as_slice()));
        let (code, _) = parse_http(b"HTTP/1.1 401 Unauthorized\r\n\r\n").unwrap();
        assert_eq!(code, 401);
    }
}
