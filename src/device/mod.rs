mod crosspoint;
mod stock;

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use indicatif::ProgressBar;
use serde::Serialize;

use crate::{Result, config};

pub use crosspoint::CrossPoint;
pub use stock::Stock;

#[derive(Serialize, Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    #[serde(rename = "dir")]
    pub is_dir: bool,
}

#[derive(Clone, Copy)]
pub struct Capabilities {
    pub download: bool,
    pub rename: bool,
    pub move_file: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Firmware {
    Stock,
    CrossPoint,
}

impl Firmware {
    pub fn as_str(self) -> &'static str {
        match self {
            Firmware::Stock => "stock",
            Firmware::CrossPoint => "crosspoint",
        }
    }

    pub fn parse(s: &str) -> Option<Firmware> {
        [Firmware::Stock, Firmware::CrossPoint]
            .into_iter()
            .find(|f| f.as_str() == s)
    }
}

pub trait Device {
    fn firmware(&self) -> Firmware;
    fn caps(&self) -> Capabilities;
    fn host(&self) -> &str;
    /// Extra status lines; stock firmware has no status endpoint.
    fn status(&self) -> Result<Vec<(&'static str, String)>>;
    fn list(&self, dir: &str) -> Result<Vec<Entry>>;
    fn mkdir(&self, parent: &str, name: &str) -> Result<()>;
    /// Callers delete an existing file first: CrossPoint refuses to overwrite.
    fn upload(&self, dir: &str, name: &str, data: &[u8], bar: &ProgressBar) -> Result<()>;
    /// Deletes in order, so a folder must come after everything inside it.
    fn delete(&self, paths: &[String]) -> Result<()>;
    fn download(&self, _path: &str, _out: &mut dyn Write, _bar: &ProgressBar) -> Result<u64> {
        Err(unsupported("pull"))
    }
    fn rename(&self, _path: &str, _new_name: &str) -> Result<()> {
        Err(unsupported("mv"))
    }
    fn move_to(&self, _path: &str, _dest_dir: &str) -> Result<()> {
        Err(unsupported("mv"))
    }
}

pub fn unsupported(cmd: &str) -> Box<dyn std::error::Error> {
    format!("`x4-cli {cmd}` is not supported on stock firmware. Flash CrossPoint to get it.").into()
}

/// `fresh` skips the per-host firmware cache.
pub fn connect(host: &str, trace: bool, fresh: bool) -> Result<Box<dyn Device>> {
    let http = Http::new(host, trace, Duration::from_secs(3));
    let cached = if fresh {
        None
    } else {
        config::cached_firmware(host)
    };
    let fw = match cached {
        Some(fw) => fw,
        None => {
            let fw = detect(&http)?;
            config::cache_firmware(host, fw);
            fw
        }
    };
    Ok(match fw {
        Firmware::Stock => Box::new(Stock { http }),
        Firmware::CrossPoint => Box::new(CrossPoint { http }),
    })
}

pub fn detect(http: &Http) -> Result<Firmware> {
    let (code, body) = http.get("/api/status", &[])?;
    if code == 200 && serde_json::from_str::<serde_json::Value>(&body).is_ok_and(|v| v.is_object())
    {
        return Ok(Firmware::CrossPoint);
    }
    let (code, _) = http.get("/list", &[("dir", "/")])?;
    if code == 200 {
        return Ok(Firmware::Stock);
    }
    Err(format!(
        "{} answers HTTP but not like an X4 (HTTP {code} from /list). Check the address.",
        http.host
    )
    .into())
}

/// Creates `dir` and any missing parents, one level at a time.
pub fn mkdir_p(dev: &dyn Device, dir: &str) -> Result<()> {
    let mut parent = String::from("/");
    for seg in dir.split('/').filter(|s| !s.is_empty()) {
        let exists = dev
            .list(&parent)?
            .iter()
            .any(|e| e.is_dir && e.name.eq_ignore_ascii_case(seg));
        if !exists {
            dev.mkdir(&parent, seg)?;
        }
        parent = join(&parent, seg);
    }
    Ok(())
}

pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

pub fn join(dir: &str, name: &str) -> String {
    normalize(&format!("{dir}/{name}"))
}

/// Resolves a device path given on the command line against the default folder.
pub fn resolve(base: &str, arg: &str) -> String {
    if arg.starts_with('/') {
        normalize(arg)
    } else {
        join(base, arg)
    }
}

pub fn split(path: &str) -> (String, String) {
    let path = normalize(path);
    let i = path.rfind('/').unwrap_or(0);
    let parent = if i == 0 {
        "/".to_string()
    } else {
        path[..i].to_string()
    };
    (parent, path[i + 1..].to_string())
}

pub struct Http {
    agent: ureq::Agent,
    pub host: String,
    pub trace: bool,
}

type Reply = (u16, String);

impl Http {
    pub fn new(host: &str, trace: bool, connect_timeout: Duration) -> Http {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(connect_timeout))
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            // CrossPoint in AP mode 302s unknown paths to its HTML page; a redirect is never a valid answer.
            .max_redirects(0)
            // The ESP32 server closes after each reply; never reuse a pooled socket.
            .max_idle_connections(0)
            .build()
            .into();
        Http {
            agent,
            host: host.to_string(),
            trace,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.host)
    }

    fn trace(&self, line: impl FnOnce() -> String) {
        if self.trace {
            eprintln!("{}", line());
        }
    }

    fn finish(
        &self,
        res: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<Reply> {
        let mut resp = res.map_err(|e| self.net_err(e))?;
        let code = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| self.net_err(e))?;
        self.trace(|| format!("< {code} {}", body.chars().take(4000).collect::<String>()));
        Ok((code, body))
    }

    pub fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Reply> {
        self.trace(|| format!("> GET {path} {query:?}"));
        let mut req = self.agent.get(self.url(path));
        for (k, v) in query {
            req = req.query(k, v);
        }
        self.finish(req.call())
    }

    pub fn form(&self, method: &str, path: &str, form: &[(&str, &str)]) -> Result<Reply> {
        self.trace(|| format!("> {method} {path} {form:?}"));
        let url = self.url(path);
        let req = match method {
            "POST" => self.agent.post(url),
            "DELETE" => self.agent.delete(url).force_send_body(),
            _ => unreachable!("form method {method}"),
        };
        self.finish(req.send_form(form.iter().copied()))
    }

    /// Sends one multipart field. The ESP32 server needs Content-Length, so the body is built in memory.
    pub fn multipart(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        field: &str,
        filename: Option<&str>,
        data: &[u8],
    ) -> Result<Reply> {
        self.trace(|| {
            format!(
                "> {method} {path} {query:?} multipart {field}={filename:?} ({} bytes)",
                data.len()
            )
        });
        let url = self.url(path);
        let mut req = match method {
            "POST" => self.agent.post(url),
            "PUT" => self.agent.put(url),
            _ => unreachable!("multipart method {method}"),
        };
        for (k, v) in query {
            req = req.query(k, v);
        }
        let boundary = format!(
            "x4-{:x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let disposition = match filename {
            Some(f) => format!(
                "form-data; name=\"{field}\"; filename=\"{}\"",
                f.replace('"', "%22")
            ),
            None => format!("form-data; name=\"{field}\""),
        };
        let mut body =
            format!("--{boundary}\r\nContent-Disposition: {disposition}\r\n").into_bytes();
        if filename.is_some() {
            body.extend_from_slice(b"Content-Type: application/octet-stream\r\n");
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        // ponytail: assumes >= 32 KB/s; raise it if slow SD cards time out.
        let timeout = Duration::from_secs(30 + data.len() as u64 / 32_768);
        let req = req
            .config()
            .timeout_global(Some(timeout))
            .build()
            .content_type(format!("multipart/form-data; boundary={boundary}"));
        self.finish(req.send(&body[..]))
    }

    /// Plain HTTP/1.1 on a TcpStream, because ureq only has whole-body timeouts. An X4 that drops
    /// off Wi-Fi mid-download leaves the socket open with no data; this fails after a 30 s stall.
    pub fn download(
        &self,
        path: &str,
        query: &[(&str, &str)],
        out: &mut dyn Write,
        bar: &ProgressBar,
    ) -> Result<u64> {
        let stall = |e: std::io::Error| match e.kind() {
            ErrorKind::WouldBlock | ErrorKind::TimedOut => self.lost("no data for 30 s"),
            _ => self.lost(e),
        };
        let query: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        let target = format!("{path}?{}", query.join("&"));
        self.trace(|| format!("> GET {target}"));
        let addr = if self.host.contains(':') {
            self.host.clone()
        } else {
            format!("{}:80", self.host)
        };
        let sock = addr
            .to_socket_addrs()
            .map_err(|e| self.unreachable(e))?
            .next()
            .ok_or("no address")?;
        let stream = TcpStream::connect_timeout(&sock, Duration::from_secs(3))
            .map_err(|e| self.unreachable(e))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        let request = format!(
            "GET {target} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.host
        );
        (&stream).write_all(request.as_bytes()).map_err(stall)?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(stall)?;
        let code: u16 = line
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| format!("the X4 sent a bad HTTP reply: {line:?}"))?;
        let (mut len, mut chunked) = (None, false);
        loop {
            line.clear();
            reader.read_line(&mut line).map_err(stall)?;
            let Some((key, value)) = line.trim_end().split_once(':') else {
                break;
            };
            match key.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = value.trim().parse::<u64>().ok(),
                "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
                _ => {}
            }
        }
        if code != 200 {
            let mut body = String::new();
            let _ = reader.take(4096).read_to_string(&mut body);
            self.trace(|| format!("< {code} {body}"));
            return Err(format!("download failed: {} (HTTP {code})", body.trim()).into());
        }
        self.trace(|| {
            format!(
                "< {code} <{} byte body>",
                len.map_or("unknown".into(), |l| l.to_string())
            )
        });
        if chunked {
            return Err("the X4 sent a chunked download, which x4-cli can't read".into());
        }
        if let Some(l) = len {
            bar.set_length(l);
        }
        let body = reader.take(len.unwrap_or(u64::MAX));
        let n = std::io::copy(&mut bar.wrap_read(body), out).map_err(stall)?;
        match len {
            Some(l) if n < l => Err(self.lost(format!("download cut off at {n} of {l} bytes"))),
            _ => Ok(n),
        }
    }

    fn net_err(&self, e: ureq::Error) -> Box<dyn std::error::Error> {
        use std::io::ErrorKind::*;
        use ureq::Timeout;
        let connect_failed = match &e {
            ureq::Error::Timeout(t) => matches!(t, Timeout::Connect | Timeout::Resolve),
            ureq::Error::Io(io) => {
                matches!(
                    io.kind(),
                    ConnectionRefused | HostUnreachable | NetworkUnreachable | AddrNotAvailable
                )
            }
            ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => true,
            _ => false,
        };
        if connect_failed {
            self.unreachable(e)
        } else {
            self.lost(e)
        }
    }

    fn unreachable(&self, e: impl std::fmt::Display) -> Box<dyn std::error::Error> {
        Box::new(Transient(format!(
            "can't reach the X4 at {} ({e}).\n  Open File Transfer on the X4 and put this computer on the same Wi-Fi (2.4 GHz only).",
            self.host
        )))
    }

    pub(super) fn lost(&self, e: impl std::fmt::Display) -> Box<dyn std::error::Error> {
        Box::new(Transient(format!(
            "lost the connection to the X4 at {} ({e}).\n  Keep it on the File Transfer screen; a weak Wi-Fi signal also drops it.",
            self.host
        )))
    }
}

/// A network failure, as opposed to the device refusing a request: worth waiting and retrying.
#[derive(Debug)]
pub struct Transient(String);

impl std::fmt::Display for Transient {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Transient {}

pub fn is_transient(e: &(dyn std::error::Error + 'static)) -> bool {
    e.is::<Transient>()
}

/// Polls until the X4's web server accepts connections again.
pub fn wait_for(host: &str, timeout: Duration) -> bool {
    let addr = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:80")
    };
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let sock = addr.to_socket_addrs().ok().and_then(|mut a| a.next());
        if sock.is_some_and(|s| TcpStream::connect_timeout(&s, Duration::from_secs(2)).is_ok()) {
            return true;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    false
}

/// Percent-encodes a query value; the ESP32 server decodes %XX.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Turns a non-2xx reply into an error that quotes the device's own message.
pub fn check((code, body): Reply, what: &str) -> Result<String> {
    if code / 100 == 2 {
        Ok(body)
    } else {
        Err(format!("{what} failed: {} (HTTP {code})", body.trim()).into())
    }
}
