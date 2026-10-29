//! Servers opened by address go through real rclone: a `WebDAV` server with a password,
//! served by a minimal in-test HTTP server.

#![cfg(feature = "embedded")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;

use rext::location::{Destination, Location};
use rext::rclone::{Embedded, api};

/// `me:pw` in Basic authentication.
const AUTH: &str = "Basic bWU6cHc=";

fn entry(href: &str, dir: bool) -> String {
    let kind = if dir { "<d:collection/>" } else { "" };
    let size = if dir { "" } else { "<d:getcontentlength>5</d:getcontentlength>" };
    format!(
        "<d:response><d:href>{href}</d:href><d:propstat><d:prop>\
         <d:resourcetype>{kind}</d:resourcetype>{size}\
         <d:getlastmodified>Mon, 01 Jan 2024 00:00:00 GMT</d:getlastmodified>\
         </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
    )
}

/// Serves `/files/` holding `a.txt` and `sub/` to `me:pw`, answering 401 otherwise.
fn serve_webdav() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (mut auth, mut depth, mut length) = (String::new(), String::new(), 0);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap_or_default();
                let value = value.trim().to_owned();
                match name.to_ascii_lowercase().as_str() {
                    "authorization" => auth = value,
                    "depth" => depth = value,
                    "content-length" => length = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let (status, extra, body) = if auth == AUTH {
                let mut xml = entry("/files/", true);
                if depth != "0" {
                    xml += &entry("/files/a.txt", false);
                    xml += &entry("/files/sub/", true);
                }
                let xml = format!(
                    "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\">{xml}</d:multistatus>"
                );
                ("207 Multi-Status", "Content-Type: application/xml\r\n", xml)
            } else {
                ("401 Unauthorized", "WWW-Authenticate: Basic realm=\"t\"\r\n", String::new())
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

fn names(backend: &Embedded, loc: &Location) -> Result<Vec<String>, String> {
    let list = api::list(backend, &loc.fs_string(), "").map_err(|e| e.message)?;
    let mut names: Vec<String> = list.into_iter().map(|i| i.name).collect();
    names.sort();
    Ok(names)
}

/// What the app does after asking for a password: obscure it and open the location.
fn log_in(backend: &Embedded, address: &str, password: &str) -> Location {
    let root = Path::new("/");
    let Ok(Destination::Login(a)) = Location::parse_address(address, None, root, root) else {
        panic!("{address} should ask for a password");
    };
    let mut endpoint = a.endpoint;
    endpoint.pass = Some(api::obscure(backend, password).unwrap());
    Location::on(endpoint, &a.path)
}

#[test]
fn webdav_with_a_password() {
    let port = serve_webdav();
    let backend = Embedded::new();
    let address = format!("webdav://me@127.0.0.1:{port}/files");

    let loc = log_in(&backend, &address, "pw");
    assert_eq!(loc.to_string(), address);
    assert_eq!(names(&backend, &loc).unwrap(), ["a.txt", "sub"]);

    let wrong = log_in(&backend, &address, "nope");
    let err = names(&backend, &wrong).unwrap_err();
    assert!(err.contains("401"), "{err}");
}
