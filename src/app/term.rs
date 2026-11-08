//! The terminal under the panes, as the interface sees it: opening it for the active pane,
//! keys and the wheel while it has them, and following the pane's directory (see
//! [`crate::terminal`]).

use std::time::Instant;

use crossterm::event::KeyEvent;

use super::{App, Focus};
use crate::event::{PaneId, ReqId};
use crate::location::{Location, endpoint_key};
use crate::rclone::RpcError;
use crate::terminal::{Session, ShellTarget, TermId, key_bytes};

/// Rows and columns until a frame has drawn the terminal.
const DEFAULT_SIZE: (u16, u16) = (24, 80);
const WHEEL_LINES: isize = 3;

pub struct Terminal {
    /// The pane whose directory it follows.
    pub pane: PaneId,
    /// The server, by `endpoint_key`: the pane is followed while it shows this one.
    endpoint: String,
    pub state: TermState,
}

pub enum TermState {
    /// Finding out how to reach the server, which may ask the user to log in first.
    Opening(ReqId),
    Running(Box<Session>),
}

impl App {
    /// Opens the terminal for the active pane, or gives it the keys; from the terminal,
    /// back to the panes.
    pub(super) fn toggle_terminal(&mut self) {
        if self.focus == Focus::Terminal {
            self.focus = Focus::Panes;
            return;
        }
        if self.terminal.is_some() {
            self.focus = Focus::Terminal;
            return;
        }
        if !self.terminal_allowed {
            self.status = Some(tr!("terminal-disabled"));
            return;
        }
        let pane = self.active;
        let loc = self.panes[pane].loc.clone();
        let req = self.next_req;
        self.next_req += 1;
        self.terminal =
            Some(Terminal { pane, endpoint: endpoint_key(&loc), state: TermState::Opening(req) });
        self.focus = Focus::Terminal;
        self.exec.shell(req, loc);
    }

    pub(super) fn on_shell(
        &mut self,
        req: ReqId,
        result: Result<Option<ShellTarget>, RpcError>,
        now: Instant,
    ) {
        let Some(t) = &self.terminal else { return };
        if !matches!(t.state, TermState::Opening(r) if r == req) {
            return;
        }
        let loc = &self.panes[t.pane].loc;
        // The pane may have left the server meanwhile: then its login directory.
        let path = if endpoint_key(loc) == t.endpoint { loc.path.as_str() } else { "" };
        let opened = match result {
            Ok(Some(target)) => {
                let dir = target.dir(path);
                let size = self.terminal_size();
                Session::open(req, target, dir, size, self.exec.events(), now)
                    .map_err(|e| e.to_string())
            }
            Ok(None) => Err(String::new()),
            Err(e) => Err(e.message),
        };
        match opened {
            Ok(session) => {
                if let Some(t) = &mut self.terminal {
                    t.state = TermState::Running(Box::new(session));
                }
            }
            Err(why) => {
                self.status = Some(if why.is_empty() {
                    tr!("terminal-not-ssh")
                } else {
                    tr!("terminal-failed", error = why)
                });
                self.close_terminal();
            }
        }
    }

    fn close_terminal(&mut self) {
        self.terminal = None;
        if self.focus == Focus::Terminal {
            self.focus = Focus::Panes;
        }
    }

    fn session(&mut self, id: TermId) -> Option<&mut Session> {
        match &mut self.terminal.as_mut()?.state {
            TermState::Running(s) if s.id == id => Some(s),
            _ => None,
        }
    }

    pub(super) fn on_term_output(&mut self, id: TermId, bytes: &[u8], now: Instant) {
        if let Some(s) = self.session(id) {
            s.output(bytes, now);
        }
    }

    pub(super) fn on_term_closed(&mut self, id: TermId) {
        let Some(s) = self.session(id) else { return };
        if let Some(why) = s.closed() {
            self.status = Some(tr!("terminal-failed", error = why));
        }
        self.close_terminal();
    }

    pub(super) fn on_term_idle(&mut self, id: TermId, idle: Option<bool>, now: Instant) {
        if let Some(note) = self.session(id).and_then(|s| s.idle(idle, now)) {
            self.status = Some(note);
        }
    }

    pub(super) fn terminal_tick(&mut self, now: Instant) {
        if let Some(Terminal { state: TermState::Running(s), .. }) = &mut self.terminal {
            s.tick(now);
        }
    }

    /// Sends the terminal along when `pane`, the one it follows, went to `loc`.
    pub(super) fn terminal_follow(&mut self, pane: PaneId, loc: &Location, now: Instant) {
        if let Some(t) = &mut self.terminal
            && t.pane == pane
            && t.endpoint == endpoint_key(loc)
            && let TermState::Running(s) = &mut t.state
        {
            let dir = s.target().dir(&loc.path);
            s.want(dir, now);
        }
    }

    /// The panes swapped places: the terminal follows the one it followed.
    pub(super) fn terminal_swapped(&mut self) {
        if let Some(t) = &mut self.terminal {
            t.pane = 1 - t.pane;
        }
    }

    pub(super) fn terminal_key(&mut self, key: &KeyEvent) {
        if let Some(Terminal { state: TermState::Running(s), .. }) = &mut self.terminal {
            let bytes = key_bytes(key, s.screen().application_cursor());
            if !bytes.is_empty() {
                s.input(&bytes);
            }
        }
    }

    /// The wheel over the terminal: through the lines above its screen.
    pub(super) fn terminal_wheel(&mut self, down: bool) {
        if let Some(Terminal { state: TermState::Running(s), .. }) = &mut self.terminal {
            s.scroll_by(if down { -WHEEL_LINES } else { WHEEL_LINES });
        }
    }

    /// The size the last frame gave the terminal's screen, which it takes on.
    pub(super) fn terminal_resize(&mut self) {
        let screen = self.hitmap.terminal_screen;
        if screen.is_empty() {
            return;
        }
        if let Some(Terminal { state: TermState::Running(s), .. }) = &mut self.terminal {
            s.resize(screen.height, screen.width);
        }
    }

    fn terminal_size(&self) -> (u16, u16) {
        let screen = self.hitmap.terminal_screen;
        if screen.is_empty() { DEFAULT_SIZE } else { (screen.height, screen.width) }
    }
}
