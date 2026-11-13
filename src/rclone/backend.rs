//! The `Backend` trait and its implementations.

use std::sync::Mutex;

use serde_json::Value;

/// An error returned by an rc call, or a failure to reach rclone at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    /// HTTP-style status code from rclone; 0 when rclone could not be reached.
    pub status: i32,
    pub message: String,
}

impl RpcError {
    pub fn new(status: i32, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    /// Builds an error from rclone's error JSON (`{"error": "...", "status": 500, ...}`).
    pub fn from_output(status: i32, output: &str) -> Self {
        let message = serde_json::from_str::<Value>(output)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| output.trim().to_owned());
        Self::new(status, message)
    }
}

/// A way to call rclone's rc methods.
pub trait Backend: Send + Sync {
    /// Calls `method` with a JSON object and returns rclone's JSON reply.
    ///
    /// # Errors
    /// Returns the error rclone reported, or a transport error.
    fn rpc(&self, method: &str, input: Value) -> Result<Value, RpcError>;

    /// Releases rclone. No call may follow.
    fn finalize(&self) {}

    /// A short description for `--version`.
    fn describe(&self) -> String;
}

#[cfg(any(feature = "embedded", feature = "rcd", test))]
fn parse_reply(status: i32, output: &str) -> Result<Value, RpcError> {
    if status == 200 {
        if output.trim().is_empty() {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        serde_json::from_str(output)
            .map_err(|e| RpcError::new(status, tr!("rclone-bad-json", error = e.to_string())))
    } else {
        Err(RpcError::from_output(status, output))
    }
}

/// rclone linked into this process through librclone.
#[cfg(feature = "embedded")]
pub struct Embedded {
    finalized: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "embedded")]
impl Embedded {
    /// Initializes the embedded rclone (once per process).
    pub fn new() -> Self {
        super::ffi::initialize();
        Self { finalized: std::sync::atomic::AtomicBool::new(false) }
    }
}

#[cfg(feature = "embedded")]
impl Default for Embedded {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "embedded")]
impl Backend for Embedded {
    fn rpc(&self, method: &str, input: Value) -> Result<Value, RpcError> {
        use std::sync::atomic::Ordering;
        if self.finalized.load(Ordering::SeqCst) {
            return Err(RpcError::new(0, tr!("rclone-shut-down")));
        }
        let (status, output) =
            super::ffi::rpc(method, &input.to_string()).map_err(|e| match e {
                super::ffi::CallError::Nul(e) => {
                    RpcError::new(0, tr!("rclone-bad-call", error = e.to_string()))
                }
                super::ffi::CallError::ShutDown => RpcError::new(0, tr!("rclone-shut-down")),
            })?;
        parse_reply(status, &output)
    }

    fn finalize(&self) {
        use std::sync::atomic::Ordering;
        if !self.finalized.swap(true, Ordering::SeqCst) {
            super::ffi::finalize();
        }
    }

    fn describe(&self) -> String {
        tr!("backend-embedded")
    }
}

/// An external `rclone rcd` reached over HTTP.
#[cfg(feature = "rcd")]
pub struct Http {
    base: String,
    auth: Option<String>,
    agent: ureq::Agent,
}

#[cfg(feature = "rcd")]
impl Http {
    /// `url` is the rcd address, e.g. `http://localhost:5572`.
    ///
    /// # Errors
    /// Refuses plain `http://` to anything but this machine: the password, rclone.conf
    /// and every password typed into rse would cross the network readable by anyone.
    pub fn new(url: &str, user: Option<&str>, pass: Option<&str>) -> Result<Self, String> {
        use base64::Engine;
        if !private_url(url) {
            return Err(tr!("rcd-plain-http", url = url));
        }
        let auth = user.map(|u| {
            let token = base64::engine::general_purpose::STANDARD
                .encode(format!("{u}:{}", pass.unwrap_or_default()));
            format!("Basic {token}")
        });
        // Requests to this machine never go through a proxy from the environment, as with
        // Go's, which rclone uses: plain http on to a proxy elsewhere would carry the
        // passwords in clear. An rc server never redirects, so none is followed.
        let proxy = if loopback(url) { None } else { ureq::Proxy::try_from_env() };
        // A connect timeout has std connect without blocking and wait in poll, which goes
        // on after a signal: a blocking connect that a signal (a window change, SIGWINCH)
        // interrupts on macOS returns EINTR, std's retry finds the socket connecting, and
        // the first write then fails with "Socket is not connected".
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(proxy)
            .max_redirects(0)
            .timeout_connect(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();
        Ok(Self { base: url.trim_end_matches('/').to_owned(), auth, agent })
    }
}

/// Whether `url` is `https://`, or `http://` to the loopback interface.
#[cfg(feature = "rcd")]
fn private_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://") || (lower.starts_with("http://") && loopback(&lower))
}

/// Whether `url` names this machine: `localhost` or a loopback address.
#[cfg(feature = "rcd")]
fn loopback(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    let Some((_, rest)) = lower.split_once("://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match hostport.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map_or("", |(h, _)| h),
        None => hostport.split(':').next().unwrap_or_default(),
    };
    host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(feature = "rcd")]
impl Backend for Http {
    fn rpc(&self, method: &str, input: Value) -> Result<Value, RpcError> {
        let url = format!("{}/{method}", self.base);
        let mut req = self.agent.post(&url).header("Content-Type", "application/json");
        if let Some(auth) = &self.auth {
            req = req.header("Authorization", auth);
        }
        let mut resp = req.send(input.to_string()).map_err(|e| {
            RpcError::new(
                0,
                tr!("rcd-unreachable", url = self.base.as_str(), error = e.to_string()),
            )
        })?;
        let status = i32::from(resp.status().as_u16());
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| RpcError::new(status, tr!("rcd-bad-reply", error = e.to_string())))?;
        parse_reply(status, &body)
    }

    fn describe(&self) -> String {
        tr!("backend-rcd", url = self.base.as_str())
    }
}

type Handler = dyn Fn(&str, &Value) -> Result<Value, RpcError> + Send + Sync;

/// A scripted backend for tests: every call goes to a closure and is logged.
pub struct FakeBackend {
    handler: Box<Handler>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl FakeBackend {
    pub fn new(
        handler: impl Fn(&str, &Value) -> Result<Value, RpcError> + Send + Sync + 'static,
    ) -> Self {
        Self { handler: Box::new(handler), calls: Mutex::new(Vec::new()) }
    }

    /// All calls made so far, in order.
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// The calls to one method.
    pub fn calls_to(&self, method: &str) -> Vec<Value> {
        self.calls().into_iter().filter(|(m, _)| m == method).map(|(_, v)| v).collect()
    }
}

impl Backend for FakeBackend {
    fn rpc(&self, method: &str, input: Value) -> Result<Value, RpcError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((method.to_owned(), input.clone()));
        }
        (self.handler)(method, &input)
    }

    fn describe(&self) -> String {
        "fake backend".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn error_json_is_unwrapped() {
        let e = RpcError::from_output(404, r#"{"error":"directory not found","status":404}"#);
        assert_eq!(e, RpcError::new(404, "directory not found"));
    }

    #[test]
    fn non_json_error_is_kept_verbatim() {
        let e = RpcError::from_output(500, "boom\n");
        assert_eq!(e.message, "boom");
    }

    #[test]
    fn empty_ok_reply_is_an_empty_object() {
        assert_eq!(parse_reply(200, "").unwrap(), json!({}));
    }

    /// Serves one HTTP request with `status` and `body`, returning what was received.
    #[cfg(feature = "rcd")]
    fn one_shot_server(
        status: u16,
        body: &'static str,
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = conn.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if req.len() >= head_end + 4 + len {
                        break;
                    }
                }
            }
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            conn.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });
        (url, handle)
    }

    #[cfg(feature = "rcd")]
    #[test]
    fn plain_http_only_to_this_machine() {
        for url in [
            "http://localhost:5572",
            "http://127.0.0.1:1/",
            "http://[::1]:5572",
            "https://nas:5572",
        ] {
            assert!(private_url(url), "{url}");
        }
        for url in [
            "http://nas:5572",
            "http://10.0.0.2",
            "http://localhost.evil.com",
            "http://127.0.0.1@nas/",
            "ftp://localhost",
        ] {
            assert!(!private_url(url), "{url}");
        }
        assert!(Http::new("http://nas:5572", None, None).is_err());
    }

    #[cfg(feature = "rcd")]
    #[test]
    fn requests_to_this_machine_skip_proxies_and_redirects() {
        let local = Http::new("http://localhost:5572", None, None).unwrap();
        assert!(local.agent.config().proxy().is_none());
        assert_eq!(local.agent.config().max_redirects(), 0);
        assert!(loopback("https://[::1]:5572/") && loopback("http://127.0.0.2"));
        assert!(!loopback("https://nas:5572") && !loopback("http://localhost@nas"));
    }

    #[cfg(feature = "rcd")]
    #[test]
    fn http_backend_posts_json_with_basic_auth() {
        let (url, server) = one_shot_server(200, r#"{"a":1}"#);
        let b = Http::new(&url, Some("me"), Some("secret")).unwrap();
        assert_eq!(b.rpc("rc/noop", json!({"a": 1})).unwrap(), json!({"a": 1}));
        let req = server.join().unwrap();
        assert!(req.starts_with("POST /rc/noop "), "{req}");
        // base64("me:secret")
        assert!(req.contains("Basic bWU6c2VjcmV0"), "{req}");
        assert!(req.ends_with(r#"{"a":1}"#), "{req}");
    }

    #[cfg(feature = "rcd")]
    #[test]
    fn http_backend_maps_errors() {
        let (url, server) =
            one_shot_server(404, r#"{"error":"couldn't find method","status":404}"#);
        let b = Http::new(&url, None, None).unwrap();
        assert_eq!(
            b.rpc("no/such", json!({})).unwrap_err(),
            RpcError::new(404, "couldn't find method")
        );
        server.join().unwrap();
        let unreachable = Http::new("http://127.0.0.1:1", None, None).unwrap();
        let e = unreachable.rpc("rc/noop", json!({})).unwrap_err();
        assert_eq!(e.status, 0);
        assert!(e.message.contains("cannot reach rclone rcd"), "{e}");
    }

    #[test]
    fn fake_backend_logs_calls() {
        let b = FakeBackend::new(|m, _| Ok(json!({ "m": m })));
        assert_eq!(b.rpc("rc/noop", json!({"a": 1})).unwrap(), json!({"m": "rc/noop"}));
        assert_eq!(b.calls_to("rc/noop"), vec![json!({"a": 1})]);
    }
}
