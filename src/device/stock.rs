use indicatif::ProgressBar;
use serde::Deserialize;

use super::{Capabilities, Device, Entry, Firmware, Http, check, join};
use crate::Result;

/// Stock Xteink firmware: an FSBrowser-style ESP32 server, usually at 192.168.3.3.
pub struct Stock {
    pub http: Http,
}

#[derive(Deserialize)]
struct ListItem {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    #[serde(default)]
    size: u64,
}

impl Device for Stock {
    fn firmware(&self) -> Firmware {
        Firmware::Stock
    }

    fn caps(&self) -> Capabilities {
        Capabilities {
            download: false,
            rename: false,
            move_file: false,
        }
    }

    fn host(&self) -> &str {
        &self.http.host
    }

    fn status(&self) -> Result<Vec<(&'static str, String)>> {
        Ok(vec![])
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let dir_arg = if dir == "/" {
            "/".to_string()
        } else {
            format!("{dir}/")
        };
        let reply = self.http.get("/list", &[("dir", &dir_arg)])?;
        let body = check(reply, &format!("listing {dir}"))
            .map_err(|e| format!("{e}\n  Does the folder exist? If you reflashed the X4, run `x4-cli status` to re-detect its firmware."))?;
        let items: Vec<ListItem> = serde_json::from_str(&body)?;
        Ok(items
            .into_iter()
            .map(|i| Entry {
                name: percent_decode(&i.name),
                size: i.size,
                is_dir: i.kind == "dir",
            })
            .collect())
    }

    fn mkdir(&self, parent: &str, name: &str) -> Result<()> {
        let path = format!("{}/", join(parent, name));
        check(
            self.http
                .multipart("PUT", "/edit", &[], "path", None, path.as_bytes())?,
            "mkdir",
        )?;
        Ok(())
    }

    fn upload(&self, dir: &str, name: &str, data: &[u8], bar: &ProgressBar) -> Result<()> {
        // The multipart filename carries the full destination path.
        check(
            self.http
                .multipart("POST", "/edit", &[], "data", Some(&join(dir, name)), data)?,
            "upload",
        )?;
        bar.set_position(data.len() as u64);
        Ok(())
    }

    fn delete(&self, paths: &[String]) -> Result<()> {
        for p in paths {
            check(
                self.http.form("DELETE", "/edit", &[("path", p)])?,
                &format!("delete {p}"),
            )?;
        }
        Ok(())
    }
}

// ponytail: assumes an encoded name is the list's quirk and the card holds the decoded name.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b
            .get(i + 1..i + 3)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}
