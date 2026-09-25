//! Runs the real `x4-cli` binary against a fake device of each firmware dialect.
//! Each test writes a transcript plus the final SD-card tree to
//! target/tmp/e2e/<test>.txt. Rerun one with `cargo test --test e2e <test>`.

mod fake;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use fake::Kind;

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Env {
    name: String,
    fake: fake::Fake,
    home: tempfile::TempDir,
    log: String,
    host: String,
    ws_port: u16,
}

impl Env {
    fn new(name: &str, kind: Kind) -> Env {
        let fake = fake::start(kind);
        let host = fake.host.clone();
        let ws_port = fake.ws_port;
        let log = format!(
            "# {name}: {kind:?} fake device at {host}\n# rerun: cargo test --test e2e {name}\n\n"
        );
        Env {
            name: name.into(),
            fake,
            home: tempfile::tempdir().unwrap(),
            log,
            host,
            ws_port,
        }
    }

    /// Writes a local file; commands run in `home`, so tests pass the bare name.
    fn local(&self, name: &str, data: &[u8]) -> String {
        std::fs::write(self.home.path().join(name), data).unwrap();
        name.to_string()
    }

    fn x4(&mut self, args: &[&str]) -> Run {
        self.x4_stdin(args, "")
    }

    fn x4_stdin(&mut self, args: &[&str], stdin: &str) -> Run {
        let mut child = Command::new(env!("CARGO_BIN_EXE_x4-cli"))
            .args(args)
            .current_dir(self.home.path())
            .env("X4_HOST", &self.host)
            .env("X4_WS_PORT", self.ws_port.to_string())
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_CACHE_HOME", self.home.path().join("cache"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let run = Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        };
        let shown: Vec<String> = args.iter().map(|a| format!("{a:?}")).collect();
        let _ = writeln!(self.log, "$ x4-cli {}", shown.join(" "));
        if !stdin.is_empty() {
            let _ = writeln!(self.log, "[stdin] {stdin:?}");
        }
        let _ = writeln!(
            self.log,
            "[exit {}]\n{}{}",
            run.code, run.stdout, run.stderr
        );
        run
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("e2e");
        std::fs::create_dir_all(&dir).unwrap();
        let body = format!(
            "{}--- SD card after test ---\n{}\n",
            self.log,
            self.fake.tree()
        );
        std::fs::write(dir.join(format!("{}.txt", self.name)), body).unwrap();
    }
}

fn epub(title: &str, author: &str) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let files = [
        ("mimetype", "application/epub+zip".to_string()),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.into(),
        ),
        (
            "OEBPS/content.opf",
            format!(
                r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">x4-test</dc:identifier><dc:title>{title}</dc:title><dc:creator>{author}</dc:creator><dc:language>en</dc:language></metadata><manifest><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c1"/></spine></package>"#
            ),
        ),
        ("OEBPS/c1.xhtml", r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>hi</p></body></html>"#.into()),
    ];
    for (name, body) in files {
        z.start_file(name, stored).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn noise(len: usize) -> Vec<u8> {
    let mut x: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

fn read(env: &Env, device_path: &str) -> Vec<u8> {
    std::fs::read(env.fake.path(device_path))
        .unwrap_or_else(|_| panic!("{device_path} missing on device"))
}

fn both(test: fn(Kind, &str), name: &str) {
    test(Kind::CrossPoint, &format!("{name}_crosspoint"));
    test(Kind::Stock, &format!("{name}_stock"));
}

#[test]
fn status_reports_firmware() {
    let mut e = Env::new("status_crosspoint", Kind::CrossPoint);
    let r = e.x4(&["status"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stdout.contains("CrossPoint") && r.stdout.contains("1.2.0-fake"),
        "{}",
        r.stdout
    );

    let mut e = Env::new("status_stock", Kind::Stock);
    let r = e.x4(&["status"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("stock"), "{}", r.stdout);
}

#[test]
fn unreachable_device_says_open_file_transfer() {
    let mut e = Env::new("unreachable", Kind::Stock);
    e.host = "127.0.0.1:1".into();
    let r = e.x4(&["ls"]);
    assert_ne!(r.code, 0);
    assert!(r.stderr.contains("File Transfer"), "{}", r.stderr);
}

#[test]
fn add_renames_from_metadata_then_ls_and_rm() {
    both(
        |kind, name| {
            let mut e = Env::new(name, kind);
            let book = epub("Dune: Messiah", "Frank Herbert");
            let b = e.local("dune.epub", &book);
            let t = e.local("notes.txt", b"plain text");
            let r = e.x4(&["add", b.as_str(), t.as_str()]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert_eq!(read(&e, "/Books/Frank Herbert - Dune Messiah.epub"), book);
            assert_eq!(read(&e, "/Books/notes.txt"), b"plain text");

            let r = e.x4(&["ls"]);
            assert!(
                r.stdout.contains("Frank Herbert - Dune Messiah.epub")
                    && r.stdout.contains("notes.txt"),
                "{}",
                r.stdout
            );

            let r = e.x4(&["rm", "*.TXT", "-y"]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert!(!e.fake.path("/Books/notes.txt").exists());
            assert!(
                e.fake
                    .path("/Books/Frank Herbert - Dune Messiah.epub")
                    .exists()
            );

            let r = e.x4(&["add", "--raw-name", "--to", "/Raw/Nested", b.as_str()]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert_eq!(read(&e, "/Raw/Nested/dune.epub"), book);
        },
        "add_ls_rm",
    );
}

#[test]
fn add_skips_duplicates_and_needs_force_to_replace() {
    both(
        |kind, name| {
            let mut e = Env::new(name, kind);
            let t = e.local("notes.txt", b"v1");
            assert_eq!(e.x4(&["add", t.as_str()]).code, 0);

            let r = e.x4(&["add", t.as_str()]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert!(
                format!("{}{}", r.stdout, r.stderr).contains("skip"),
                "{}{}",
                r.stdout,
                r.stderr
            );

            e.local("notes.txt", b"version two");
            let r = e.x4(&["add", t.as_str()]);
            assert_ne!(r.code, 0);
            assert!(r.stderr.contains("--force"), "{}", r.stderr);
            assert_eq!(read(&e, "/Books/notes.txt"), b"v1");

            let r = e.x4(&["add", "--force", t.as_str()]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert_eq!(read(&e, "/Books/notes.txt"), b"version two");
        },
        "dedup",
    );
}

#[test]
fn add_rejects_unsupported_format_before_uploading() {
    let mut e = Env::new("reject_format", Kind::CrossPoint);
    let ok = e.local("ok.txt", b"fine");
    let bad = e.local("paper.pdf", b"%PDF");
    let r = e.x4(&["add", ok.as_str(), bad.as_str()]);
    assert_ne!(r.code, 0);
    assert!(r.stderr.contains("paper.pdf"), "{}", r.stderr);
    assert!(!e.fake.path("/Books/ok.txt").exists());
}

#[test]
fn large_upload_is_byte_identical() {
    both(
        |kind, name| {
            let mut e = Env::new(name, kind);
            let data = noise(300_000);
            let f = e.local("big.xtc", &data);
            let r = e.x4(&["--trace", "add", f.as_str()]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert!(read(&e, "/Books/big.xtc") == data, "uploaded bytes differ");
            let path_used = if kind == Kind::CrossPoint {
                "< WS DONE"
            } else {
                "multipart data"
            };
            assert!(
                r.stderr.contains(path_used) && !r.stderr.contains("using HTTP"),
                "{}",
                r.stderr
            );
        },
        "large_upload",
    );
}

#[test]
fn rm_directories_need_r_and_recurse() {
    both(
        |kind, name| {
            let mut e = Env::new(name, kind);
            e.fake.put("/Books/Series/One/a.txt", b"a");
            e.fake.put("/Books/Series/b.txt", b"b");
            e.fake.put("/Books/keep.txt", b"k");

            let r = e.x4(&["rm", "Series", "-y"]);
            assert_ne!(r.code, 0);
            assert!(r.stderr.contains("-r"), "{}", r.stderr);
            assert!(e.fake.path("/Books/Series/b.txt").exists());

            let r = e.x4(&["rm", "-r", "-y", "Series"]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert!(!e.fake.path("/Books/Series").exists());
            assert!(e.fake.path("/Books/keep.txt").exists());
        },
        "rm_recursive",
    );
}

#[test]
fn rm_without_confirmation_deletes_nothing() {
    let mut e = Env::new("rm_declined", Kind::CrossPoint);
    e.fake.put("/Books/a.txt", b"a");
    let r = e.x4_stdin(&["rm", "a.txt"], "n\n");
    assert!(r.stdout.contains("a.txt") || r.stderr.contains("a.txt"));
    assert!(e.fake.path("/Books/a.txt").exists());
}

#[test]
fn ls_json_decodes_names_and_rm_uses_them() {
    both(
        |kind, name| {
            let mut e = Env::new(name, kind);
            e.fake.put("/Books/My Book.txt", b"12345");
            e.fake.put("/Books/Sub/x.txt", b"x");
            let r = e.x4(&["ls", "--json"]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            let v: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
            let names: Vec<&str> = v
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["name"].as_str().unwrap())
                .collect();
            assert!(
                names.contains(&"My Book.txt") && names.contains(&"Sub"),
                "{names:?}"
            );

            let r = e.x4(&["rm", "My Book.txt", "-y"]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert!(!e.fake.path("/Books/My Book.txt").exists());
        },
        "ls_json",
    );
}

#[test]
fn pull_and_mv_on_crosspoint() {
    let mut e = Env::new("pull_mv_crosspoint", Kind::CrossPoint);
    let data = noise(70_000);
    e.fake.put("/Books/a.epub", &data);
    let r = e.x4(&["pull", "a.epub"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(std::fs::read(e.home.path().join("a.epub")).unwrap() == data);
    let r = e.x4(&["pull", "a.epub"]);
    assert_ne!(r.code, 0, "pull must not overwrite a local file");
    let r = e.x4(&["pull", "a.epub", "-o", "copy.epub"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(std::fs::read(e.home.path().join("copy.epub")).unwrap() == data);

    assert_eq!(e.x4(&["mv", "a.epub", "b.epub"]).code, 0);
    assert!(e.fake.path("/Books/b.epub").exists());
    let r = e.x4(&["mv", "b.epub", "/Read/"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(e.fake.path("/Read/b.epub").exists());
    let r = e.x4(&["mv", "/Read/b.epub", "/Books/c.epub"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(read(&e, "/Books/c.epub") == data);
}

#[test]
fn pull_and_mv_are_refused_on_stock() {
    let mut e = Env::new("pull_mv_stock", Kind::Stock);
    e.fake.put("/Books/a.txt", b"a");
    for args in [&["pull", "a.txt"][..], &["mv", "a.txt", "b.txt"][..]] {
        let r = e.x4(args);
        assert_ne!(r.code, 0);
        assert!(
            r.stderr.contains("not supported on stock firmware"),
            "{}",
            r.stderr
        );
    }
    assert!(e.fake.path("/Books/a.txt").exists());
}

#[test]
fn crosspoint_falls_back_to_http_when_websocket_port_is_closed() {
    let mut e = Env::new("http_fallback_crosspoint", Kind::CrossPoint);
    e.ws_port = 1;
    let data = noise(50_000);
    let f = e.local("fallback.txt", &data);
    let r = e.x4(&["add", f.as_str()]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stderr.contains("using HTTP"), "{}", r.stderr);
    assert!(
        read(&e, "/Books/fallback.txt") == data,
        "uploaded bytes differ"
    );
}

#[test]
fn no_spaces_turns_spaces_into_underscores() {
    let mut e = Env::new("no_spaces", Kind::Stock);
    let book = epub("Dune: Messiah", "Frank Herbert");
    let b = e.local("dune.epub", &book);
    let t = e.local("my notes.txt", b"n");
    let r = e.x4(&["add", "--no-spaces", b.as_str(), t.as_str()]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(read(&e, "/Books/Frank_Herbert-Dune_Messiah.epub"), book);
    assert_eq!(read(&e, "/Books/my_notes.txt"), b"n");

    let cfg = e.home.path().join("config/x4-cli");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("config.toml"), "no_spaces = true\n").unwrap();
    let t = e.local("more notes.txt", b"m");
    let r = e.x4(&["add", t.as_str()]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(read(&e, "/Books/more_notes.txt"), b"m");
}

#[test]
fn add_rides_out_a_dropped_connection() {
    use std::sync::atomic::Ordering::SeqCst;
    let mut e = Env::new("retry_after_drop", Kind::CrossPoint);
    e.fake.drop_uploads.store(1, SeqCst);
    let big = noise(300_000);
    let a = e.local("big.xtc", &big);
    let b = e.local("small.txt", b"small");
    let r = e.x4(&["add", a.as_str(), b.as_str()]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        e.fake.drop_uploads.load(SeqCst),
        0,
        "the fake must have dropped once"
    );
    assert!(r.stderr.contains("waiting"), "{}", r.stderr);
    assert!(
        read(&e, "/Books/big.xtc") == big,
        "the retry must replace the partial file"
    );
    assert_eq!(read(&e, "/Books/small.txt"), b"small");
}

#[test]
fn add_reports_a_failed_file_and_uploads_the_rest() {
    let mut e = Env::new("partial_failure", Kind::CrossPoint);
    // A folder where the file should go: the device refuses that one upload.
    e.fake.put("/Books/clash.txt/inside.txt", b"x");
    let a = e.local("a.txt", b"a");
    let c = e.local("clash.txt", b"c");
    let b = e.local("b.txt", b"b");
    let r = e.x4(&["add", a.as_str(), c.as_str(), b.as_str()]);
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains("clash.txt") && r.stderr.contains("1 of 3"),
        "{}",
        r.stderr
    );
    assert_eq!(read(&e, "/Books/a.txt"), b"a");
    assert_eq!(read(&e, "/Books/b.txt"), b"b");
}

#[test]
fn sync_mirrors_a_local_folder() {
    both(
        |kind, name| {
            let mut e = Env::new(name, kind);
            let lib = e.home.path().join("lib");
            std::fs::create_dir_all(lib.join("Sci-Fi")).unwrap();
            let dune = epub("Dune", "Frank Herbert");
            std::fs::write(lib.join("dune.epub"), &dune).unwrap();
            std::fs::write(lib.join("notes.txt"), b"notes").unwrap();
            std::fs::write(lib.join("Sci-Fi/other.txt"), b"other").unwrap();
            std::fs::write(lib.join(".hidden.txt"), b"h").unwrap();
            std::fs::write(lib.join("paper.pdf"), b"%PDF").unwrap();

            let r = e.x4(&["sync"]);
            assert_ne!(r.code, 0);
            assert!(r.stderr.contains("--save"), "{}", r.stderr);

            let r = e.x4(&["sync", "lib", "--save"]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert_eq!(read(&e, "/Books/Frank Herbert - Dune.epub"), dune);
            assert_eq!(read(&e, "/Books/notes.txt"), b"notes");
            assert_eq!(read(&e, "/Books/Sci-Fi/other.txt"), b"other");
            assert!(!e.fake.path("/Books/.hidden.txt").exists());
            assert!(r.stdout.contains("paper.pdf"), "{}", r.stdout);

            let r = e.x4(&["sync"]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert!(r.stdout.contains("0 added"), "{}", r.stdout);

            std::fs::remove_file(lib.join("notes.txt")).unwrap();
            e.fake.put("/Books/extra.txt", b"e");
            e.fake.put("/Books/update.bin", b"not a book");
            let r = e.x4_stdin(&["sync", "--delete"], "n\n");
            assert!(r.stdout.contains("extra.txt"), "{}", r.stdout);
            assert!(e.fake.path("/Books/notes.txt").exists(), "declined");

            let r = e.x4(&["sync", "--delete", "-y"]);
            assert_eq!(r.code, 0, "{}", r.stderr);
            assert!(!e.fake.path("/Books/notes.txt").exists());
            assert!(!e.fake.path("/Books/extra.txt").exists());
            assert!(
                e.fake.path("/Books/update.bin").exists(),
                "only books are deleted"
            );
            assert!(e.fake.path("/Books/Sci-Fi/other.txt").exists());
            assert!(e.fake.path("/Books/Frank Herbert - Dune.epub").exists());
        },
        "sync",
    );
}
