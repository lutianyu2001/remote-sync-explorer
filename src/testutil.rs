//! Test helpers: an in-memory file tree behind `FakeBackend`, and an `App` harness that
//! pumps worker events and renders into a `TestBackend`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use crate::app::{App, AppInit};
use crate::config::UiConfig;
use crate::engine::Engines;
use crate::event::Event;
use crate::exec::Exec;
use crate::hitmap::HitMap;
use crate::keymap::Keymap;
use crate::location::Location;
use crate::rclone::{FakeBackend, RpcError};

pub type Tree = Arc<Mutex<BTreeMap<String, Vec<Value>>>>;

pub fn file(name: &str, size: i64) -> Value {
    json!({"Name": name, "Size": size, "IsDir": false, "ModTime": "2026-10-20T08:15:00Z"})
}

pub fn dir(name: &str) -> Value {
    json!({"Name": name, "Size": -1, "IsDir": true, "ModTime": "2026-10-19T12:00:00Z"})
}

fn key(fs: &str, remote: &str) -> String {
    if remote.is_empty() { fs.to_owned() } else { Location::new(fs, remote).fs_string() }
}

/// A backend serving `dirs` (fs string → entries). Listing an unknown directory fails
/// with "directory not found". Jobs finish successfully at once. Listings of
/// directories in `slow` take that long.
pub fn fake_fs(dirs: &[(&str, Vec<Value>)], slow: &[(&str, Duration)]) -> (Arc<FakeBackend>, Tree) {
    let tree: Tree =
        Arc::new(Mutex::new(dirs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()));
    let slow: HashMap<String, Duration> = slow.iter().map(|(k, d)| ((*k).to_owned(), *d)).collect();
    let t = tree.clone();
    let next_job = AtomicI64::new(1);
    let backend = FakeBackend::new(move |method, input| {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        match method {
            "operations/list" => {
                let k = key(&s("fs"), &s("remote"));
                if let Some(d) = slow.get(&k) {
                    std::thread::sleep(*d);
                }
                let tree = t.lock().map_err(|_| RpcError::new(500, "poisoned"))?;
                match tree.get(&k) {
                    Some(list) => Ok(json!({ "list": list })),
                    None => Err(RpcError::new(404, "directory not found")),
                }
            }
            "operations/mkdir" => {
                let (parent, name) = (s("fs"), s("remote"));
                let mut tree = t.lock().map_err(|_| RpcError::new(500, "poisoned"))?;
                tree.entry(parent.clone()).or_default().push(dir(&name));
                tree.entry(key(&parent, &name)).or_default();
                Ok(json!({}))
            }
            "config/listremotes" => Ok(json!({"remotes": ["nas", "s3"]})),
            "config/dump" => Ok(json!({"nas": {"type": "smb"}, "s3": {"type": "s3"}})),
            // Not rclone's obscuring, but recognisable in fs strings.
            "core/obscure" => Ok(json!({"obscured": format!("OB({})", s("clear"))})),
            "job/status" => {
                Ok(json!({"id": input["jobid"], "finished": true, "success": true, "error": ""}))
            }
            "core/stats"
            | "core/stats-delete"
            | "job/stop"
            | "operations/movefile"
            | "sync/move"
            | "operations/rmdir" => {
                if input.get("_async").is_some() {
                    Ok(json!({"jobid": next_job.fetch_add(1, Ordering::SeqCst)}))
                } else {
                    Ok(json!({}))
                }
            }
            _ if input.get("_async").is_some() => {
                Ok(json!({"jobid": next_job.fetch_add(1, Ordering::SeqCst)}))
            }
            _ => Ok(json!({})),
        }
    });
    (Arc::new(backend), tree)
}

pub struct Harness {
    pub app: App,
    pub backend: Arc<FakeBackend>,
    rx: Receiver<Event>,
    terminal: Terminal<TestBackend>,
    frame: Buffer,
    pub screen: String,
    /// Time for clicks: always well after the previous click and after "now".
    clock: Instant,
}

impl Harness {
    pub fn new(backend: Arc<FakeBackend>, left: Location, right: Location) -> Self {
        Self::with(backend, left, right, UiConfig::default(), 80, 24)
    }

    pub fn with(
        backend: Arc<FakeBackend>,
        left: Location,
        right: Location,
        ui: UiConfig,
        w: u16,
        h: u16,
    ) -> Self {
        Self::with_engines(backend, left, right, ui, (w, h), Engines::default())
    }

    /// With terminals that run `shell` wherever the pane is.
    pub fn with_shell(
        backend: Arc<FakeBackend>,
        left: Location,
        right: Location,
        shell: crate::terminal::ShellTarget,
    ) -> Self {
        let mut engines = Engines::default();
        engines.local_shell = Some(shell);
        Self::with_engines(backend, left, right, UiConfig::default(), (100, 30), engines)
    }

    fn with_engines(
        backend: Arc<FakeBackend>,
        left: Location,
        right: Location,
        ui: UiConfig,
        (w, h): (u16, u16),
        engines: Engines,
    ) -> Self {
        let (tx, rx) = unbounded();
        let exec = Exec::spawn_with(backend.clone(), tx, 2, Duration::from_millis(10), engines);
        let init = AppInit {
            ui,
            keymap: Keymap::default(),
            max_concurrent: 2,
            left,
            right,
            remotes: vec![("nas".to_owned(), "smb".to_owned()), ("s3".to_owned(), "s3".to_owned())],
            home: PathBuf::from("/home/sky"),
            cwd: PathBuf::from("/home/sky"),
            saved: Vec::new(),
            queue_file: None,
            terminal: true,
        };
        let mut app = App::new(init, exec);
        app.display_tz = chrono::FixedOffset::east_opt(0);
        let terminal = Terminal::new(TestBackend::new(w, h)).unwrap_or_else(|e| panic!("{e}"));
        let frame = Buffer::empty(Rect::new(0, 0, w, h));
        let mut h = Self {
            app,
            backend,
            rx,
            terminal,
            frame,
            screen: String::new(),
            clock: Instant::now(),
        };
        h.settle();
        h
    }

    /// Handles events until none arrive for a while (or for at most 300 ms while jobs keep
    /// reporting progress), then redraws.
    pub fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            let Ok(ev) = self.rx.recv_timeout(Duration::from_millis(80)) else { break };
            self.app.handle(ev, Instant::now());
        }
        self.render();
    }

    /// Handles events until `done` holds (or panics after 5 s), then redraws.
    pub fn wait_until(&mut self, done: impl Fn(&App) -> bool) {
        self.wait_until_at(None, done);
    }

    /// Like `wait_until`, handling what arrives as if at time `at` (now if `None`).
    pub fn wait_until_at(&mut self, at: Option<Instant>, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(&self.app) {
            assert!(Instant::now() < deadline, "condition not reached");
            if let Ok(ev) = self.rx.recv_timeout(Duration::from_millis(20)) {
                self.app.handle(ev, at.unwrap_or_else(Instant::now));
            }
        }
        self.render();
    }

    /// Draws a frame as the event loop does, the cursor put away after it; whether the text
    /// cursor is shown, and where the terminal's cursor is.
    pub fn draw_and_park(&mut self) -> (bool, ratatui::layout::Position) {
        let app = &self.app;
        let mut park = None;
        self.terminal.draw(|f| park = crate::ui::draw(f, app).1).unwrap_or_else(|e| panic!("{e}"));
        crate::ui::park_cursor(&mut self.terminal, park).unwrap_or_else(|e| panic!("{e}"));
        (park.is_none(), self.terminal.get_cursor_position().unwrap_or_else(|e| panic!("{e}")))
    }

    pub fn render(&mut self) -> &str {
        let mut map = HitMap::default();
        let app = &self.app;
        // The frame as drawn, not the test backend's copy patched with the changes: that one
        // keeps stale cells beside wide characters, which a terminal would have cleared.
        self.frame = self
            .terminal
            .draw(|f| map = crate::ui::draw(f, app).0)
            .unwrap_or_else(|e| panic!("{e}"))
            .buffer
            .clone();
        self.app.set_hitmap(map);
        let buf = &self.frame;
        let mut out = String::new();
        for y in 0..buf.area.height {
            let mut line = String::new();
            let mut x = 0;
            while x < buf.area.width {
                let sym = buf[(x, y)].symbol();
                line.push_str(sym);
                // A wide character covers the next cell too.
                x += u16::try_from(crate::ui::text::width(sym).max(1)).unwrap_or(1);
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        self.screen = out;
        &self.screen
    }

    /// The foreground color of a cell on the last drawn screen.
    pub fn fg(&self, x: u16, y: u16) -> ratatui::style::Color {
        self.frame[(x, y)].fg
    }

    /// The background color of a cell on the last drawn screen.
    pub fn bg(&self, x: u16, y: u16) -> ratatui::style::Color {
        self.frame[(x, y)].bg
    }

    /// The modifiers (bold, reversed, …) of a cell on the last drawn screen.
    pub fn cell_modifiers(&self, x: u16, y: u16) -> ratatui::style::Modifier {
        self.frame[(x, y)].modifier
    }

    pub fn press(&mut self, code: KeyCode) {
        self.press_mod(code, KeyModifiers::NONE);
    }

    pub fn press_mod(&mut self, code: KeyCode, mods: KeyModifiers) {
        self.app.handle(Event::Key(KeyEvent::new(code, mods)), Instant::now());
        self.settle();
    }

    pub fn type_text(&mut self, s: &str) {
        for c in s.chars() {
            self.app.handle(
                Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                Instant::now(),
            );
        }
        self.settle();
    }

    pub fn mouse_at(
        &mut self,
        kind: MouseEventKind,
        col: u16,
        row: u16,
        mods: KeyModifiers,
        now: Instant,
    ) {
        self.app.handle(Event::Mouse(MouseEvent { kind, column: col, row, modifiers: mods }), now);
    }

    pub fn click(&mut self, col: u16, row: u16) {
        self.click_mod(col, row, KeyModifiers::NONE);
    }

    /// A click well after anything appeared (past the dialog click guard).
    pub fn click_mod(&mut self, col: u16, row: u16, mods: KeyModifiers) {
        self.clock =
            (self.clock + Duration::from_secs(1)).max(Instant::now() + Duration::from_secs(1));
        let now = self.clock;
        self.mouse_at(MouseEventKind::Down(MouseButton::Left), col, row, mods, now);
        self.mouse_at(MouseEventKind::Up(MouseButton::Left), col, row, mods, now);
        self.settle();
    }

    /// Screen position of `text` in the last frame: the left pane is searched first,
    /// then the right pane, then the rest of the screen.
    pub fn find(&self, text: &str) -> (u16, u16) {
        let w = crate::ui::text::width;
        let lines: Vec<&str> = self.screen.lines().collect();
        let half = lines.first().map_or(0, |l| w(l) / 2);
        let found: Vec<(usize, usize)> = lines
            .iter()
            .enumerate()
            .flat_map(|(y, line)| line.match_indices(text).map(move |(i, _)| (y, w(&line[..i]))))
            .collect();
        // The left pane first, as the left half of the screen.
        let (y, col) = found
            .iter()
            .find(|(_, col)| *col < half)
            .or(found.first())
            .copied()
            .unwrap_or_else(|| panic!("`{text}` not on screen:\n{}", self.screen));
        (u16::try_from(col).unwrap_or(0), u16::try_from(y).unwrap_or(0))
    }

    /// The calls made to `method`, ignoring polling noise.
    pub fn calls(&self, method: &str) -> Vec<Value> {
        self.backend.calls_to(method)
    }
}
