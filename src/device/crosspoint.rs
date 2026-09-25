use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use indicatif::ProgressBar;
use serde::Deserialize;
use tungstenite::Message;

use super::{Capabilities, Device, Entry, Firmware, Http, check};
use crate::Result;

pub struct CrossPoint {
    pub http: Http,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileItem {
    name: String,
    size: u64,
    is_directory: bool,
}

// The firmware's web page streams 4 KB frames; arduinoWebSockets caps frames at 15 KB.
const CHUNK: usize = 4096;
// Unacknowledged bytes allowed in flight, as in send-to-x4. PROGRESS acks arrive every 64 KB.
const MAX_IN_FLIGHT: u64 = 128 * 1024;

enum WsError {
    /// Port 81 did not accept a WebSocket; HTTP upload may still work.
    Connect(String),
    Upload(Box<dyn std::error::Error>),
}

impl Device for CrossPoint {
    fn firmware(&self) -> Firmware {
        Firmware::CrossPoint
    }

    fn caps(&self) -> Capabilities {
        Capabilities {
            download: true,
            rename: true,
            move_file: true,
        }
    }

    fn host(&self) -> &str {
        &self.http.host
    }

    fn status(&self) -> Result<Vec<(&'static str, String)>> {
        let body = check(self.http.get("/api/status", &[])?, "status")?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        let field = |k: &str| match &v[k] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => "?".into(),
            other => other.to_string(),
        };
        // Below about -80 dBm the X4 drops off Wi-Fi mid-transfer (seen at -89 and -93).
        let weak = v["rssi"].as_i64().is_some_and(|r| r != 0 && r < -80);
        Ok(vec![
            ("version", field("version")),
            ("device", field("device")),
            ("ip", field("ip")),
            (
                "wifi",
                format!(
                    "{} (rssi {} dBm){}",
                    field("mode"),
                    field("rssi"),
                    if weak {
                        " weak: move the X4 closer to the router"
                    } else {
                        ""
                    }
                ),
            ),
            ("free heap", format!("{} bytes", field("freeHeap"))),
            ("uptime", format!("{} s", field("uptime"))),
        ])
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let reply = self.http.get("/api/files", &[("path", dir)])?;
        if reply.0 == 404 {
            return Err(format!(
                "{} no longer answers like CrossPoint firmware. If you reflashed it, run `x4-cli status` to re-detect.",
                self.http.host
            )
            .into());
        }
        let items: Vec<FileItem> = serde_json::from_str(&check(reply, "listing")?)?;
        Ok(items
            .into_iter()
            .map(|i| Entry {
                name: i.name,
                size: i.size,
                is_dir: i.is_directory,
            })
            .collect())
    }

    fn mkdir(&self, parent: &str, name: &str) -> Result<()> {
        check(
            self.http
                .form("POST", "/mkdir", &[("name", name), ("path", parent)])?,
            "mkdir",
        )?;
        Ok(())
    }

    fn upload(&self, dir: &str, name: &str, data: &[u8], bar: &ProgressBar) -> Result<()> {
        match self.ws_upload(dir, name, data, bar) {
            Ok(()) => Ok(()),
            Err(WsError::Upload(e)) => Err(e),
            Err(WsError::Connect(why)) => {
                bar.suspend(|| {
                    eprintln!("x4-cli: WebSocket upload unavailable ({why}); using HTTP")
                });
                check(
                    self.http.multipart(
                        "POST",
                        "/upload",
                        &[("path", dir)],
                        "file",
                        Some(name),
                        data,
                    )?,
                    "upload",
                )?;
                bar.set_position(data.len() as u64);
                Ok(())
            }
        }
    }

    fn delete(&self, paths: &[String]) -> Result<()> {
        let reply = match paths {
            [one] => self.http.form("POST", "/delete", &[("path", one)])?,
            many => self.http.form(
                "POST",
                "/delete",
                &[("paths", &serde_json::to_string(many)?)],
            )?,
        };
        check(reply, "delete")?;
        Ok(())
    }

    fn download(&self, path: &str, out: &mut dyn Write, bar: &ProgressBar) -> Result<u64> {
        self.http.download("/download", &[("path", path)], out, bar)
    }

    fn rename(&self, path: &str, new_name: &str) -> Result<()> {
        check(
            self.http
                .form("POST", "/rename", &[("path", path), ("name", new_name)])?,
            "rename",
        )?;
        Ok(())
    }

    fn move_to(&self, path: &str, dest_dir: &str) -> Result<()> {
        check(
            self.http
                .form("POST", "/move", &[("path", path), ("dest", dest_dir)])?,
            "move",
        )?;
        Ok(())
    }
}

impl CrossPoint {
    /// Protocol: START:<name>:<size>:<dir> -> READY -> binary frames (PROGRESS acks) -> DONE | ERROR:<msg>.
    fn ws_upload(
        &self,
        dir: &str,
        name: &str,
        data: &[u8],
        bar: &ProgressBar,
    ) -> std::result::Result<(), WsError> {
        let host = self
            .http
            .host
            .rsplit_once(':')
            .map_or(self.http.host.as_str(), |(h, _)| h);
        let port: u16 = std::env::var("X4_WS_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(81);
        let conn = |e: &dyn std::fmt::Display| WsError::Connect(e.to_string());
        let addr = (host, port)
            .to_socket_addrs()
            .map_err(|e| conn(&e))?
            .next()
            .ok_or_else(|| conn(&"no address"))?;
        let stream =
            TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| conn(&e))?;
        // A stall longer than this means the device left File Transfer or hung.
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| conn(&e))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| conn(&e))?;
        let (mut ws, _) =
            tungstenite::client(format!("ws://{host}:{port}/"), stream).map_err(|e| conn(&e))?;

        let trace = |line: String| {
            if self.http.trace {
                bar.suspend(|| eprintln!("{line}"));
            }
        };
        let lost =
            |e: tungstenite::Error| WsError::Upload(self.http.lost(format!("WebSocket: {e}")));
        let device_err =
            |msg: &str| WsError::Upload(format!("upload of {name} failed: {msg}").into());

        let start = format!("START:{name}:{}:{dir}", data.len());
        trace(format!("> WS {start}"));
        ws.send(Message::text(start)).map_err(lost)?;

        let mut acked: u64 = 0;
        let mut sent: u64 = 0;
        let mut chunks = data.chunks(CHUNK);
        let mut ready = false;
        loop {
            let may_send = ready && sent < data.len() as u64 && sent - acked < MAX_IN_FLIGHT;
            if may_send {
                let chunk = chunks.next().expect("chunks remain while sent < len");
                ws.send(Message::binary(chunk.to_vec())).map_err(lost)?;
                sent += chunk.len() as u64;
                continue;
            }
            let text = match ws.read().map_err(lost)? {
                Message::Text(t) => t.to_string(),
                Message::Close(_) => return Err(lost(tungstenite::Error::ConnectionClosed)),
                _ => continue,
            };
            trace(format!("< WS {text}"));
            if text == "READY" {
                ready = true;
            } else if text == "DONE" {
                bar.set_position(data.len() as u64);
                let _ = ws.close(None);
                return Ok(());
            } else if let Some(p) = text.strip_prefix("PROGRESS:") {
                acked = p
                    .split(':')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(acked);
                bar.set_position(acked);
            } else if text == "ERROR:Upload already in progress" {
                // After a drop the firmware can still hold the dead socket's upload;
                // HTTP upload has its own state, so it still works.
                return Err(WsError::Connect(
                    "the X4 is still busy with a broken upload".into(),
                ));
            } else if let Some(msg) = text.strip_prefix("ERROR:") {
                return Err(device_err(msg));
            }
        }
    }
}
