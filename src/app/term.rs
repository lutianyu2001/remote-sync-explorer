//! The terminals under the panes, one a pane, as the interface sees them: opening one for the
//! active pane,
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
    /// The server, by `endpoint_key`: its pane is followed while it shows this one.
    endpoint: String,
    pub state: TermState,
}

pub enum TermState {
    /// Finding out how to reach the server, which may ask the user to log in first.
    Opening(ReqId),
    Running(Box<Session>),
}

impl App {
    /// Opens the active pane's terminal, under it, or gives it the keys; from the
    /// terminal, back to the panes.
    pub(super) fn toggle_terminal(&mut self) {
        if self.focus == Focus::Terminal {
            self.focus = Focus::Panes;
            return;
        }
        let pane = self.active;
        if self.terminals[pane].is_some() {
            self.focus = Focus::Terminal;
            return;
        }
        if !self.terminal_allowed {
            self.status = Some(tr!("terminal-disabled"));
            return;
        }
        let loc = self.panes[pane].loc.clone();
        let req = self.next_req;
        self.next_req += 1;
        self.terminals[pane] =
            Some(Terminal { endpoint: endpoint_key(&loc), state: TermState::Opening(req) });
        self.focus = Focus::Terminal;
        self.exec.shell(req, loc);
    }

    pub(super) fn on_shell(
        &mut self,
        req: ReqId,
        result: Result<Option<ShellTarget>, RpcError>,
        now: Instant,
    ) {
        let Some(pane) = self.terminals.iter().position(
            |t| matches!(t, Some(Terminal { state: TermState::Opening(r), .. }) if *r == req),
        ) else {
            return;
        };
        let loc = &self.panes[pane].loc;
        let same = self.terminals[pane].as_ref().is_some_and(|t| endpoint_key(loc) == t.endpoint);
        // The pane may have left the server meanwhile: then its login directory.
        let path = if same { loc.path.as_str() } else { "" };
        let opened = match result {
            Ok(Some(target)) => {
                let dir = target.dir(path);
                let size = self.terminal_size(pane);
                Session::open(req, target, dir, size, self.exec.events(), now)
                    .map_err(|e| e.to_string())
            }
            Ok(None) => Err(String::new()),
            Err(e) => Err(e.message),
        };
        match opened {
            Ok(session) => {
                if let Some(t) = &mut self.terminals[pane] {
                    t.state = TermState::Running(Box::new(session));
                }
            }
            Err(why) => {
                self.status = Some(if why.is_empty() {
                    tr!("terminal-not-ssh")
                } else {
                    tr!("terminal-failed", error = why)
                });
                self.close_terminal(pane);
            }
        }
    }

    fn close_terminal(&mut self, pane: PaneId) {
        self.terminals[pane] = None;
        self.terminal_focus_check();
    }

    /// The terminal focus belongs to the active pane's terminal: without one, the panes.
    pub(super) fn terminal_focus_check(&mut self) {
        if self.focus == Focus::Terminal && self.terminals[self.active].is_none() {
            self.focus = Focus::Panes;
        }
    }

    /// The pane whose running terminal is `id`.
    fn session_pane(&self, id: TermId) -> Option<PaneId> {
        self.terminals.iter().position(
            |t| matches!(t, Some(Terminal { state: TermState::Running(s), .. }) if s.id == id),
        )
    }

    fn session(&mut self, id: TermId) -> Option<&mut Session> {
        let pane = self.session_pane(id)?;
        match &mut self.terminals[pane].as_mut()?.state {
            TermState::Running(s) => Some(s),
            TermState::Opening(_) => None,
        }
    }

    pub(super) fn on_term_output(&mut self, id: TermId, bytes: &[u8], now: Instant) {
        if let Some(s) = self.session(id) {
            s.output(bytes, now);
        }
    }

    pub(super) fn on_term_closed(&mut self, id: TermId) {
        let Some(pane) = self.session_pane(id) else { return };
        if let Some(why) = self.session(id).and_then(Session::closed) {
            self.status = Some(tr!("terminal-failed", error = why));
        }
        self.close_terminal(pane);
    }

    pub(super) fn on_term_idle(&mut self, id: TermId, idle: Option<bool>, now: Instant) {
        if let Some(note) = self.session(id).and_then(|s| s.idle(idle, now)) {
            self.status = Some(note);
        }
    }

    pub(super) fn terminal_tick(&mut self, now: Instant) {
        for t in &mut self.terminals {
            if let Some(Terminal { state: TermState::Running(s), .. }) = t {
                s.tick(now);
            }
        }
    }

    /// Sends `pane`'s terminal along when the pane went to `loc`.
    pub(super) fn terminal_follow(&mut self, pane: PaneId, loc: &Location, now: Instant) {
        if let Some(t) = &mut self.terminals[pane]
            && t.endpoint == endpoint_key(loc)
            && let TermState::Running(s) = &mut t.state
        {
            let dir = s.target().dir(&loc.path);
            s.want(dir, now);
        }
    }

    /// The panes swapped places: each terminal goes along with its pane.
    pub(super) fn terminal_swapped(&mut self) {
        self.terminals.swap(0, 1);
    }

    /// Keys for the active pane's terminal, which has the focus.
    pub(super) fn terminal_key(&mut self, key: &KeyEvent) {
        if let Some(Terminal { state: TermState::Running(s), .. }) =
            &mut self.terminals[self.active]
        {
            let bytes = key_bytes(key, s.screen().application_cursor());
            if !bytes.is_empty() {
                s.input(&bytes);
            }
        }
    }

    /// The wheel over `pane`'s terminal: through the lines above its screen.
    pub(super) fn terminal_wheel(&mut self, pane: PaneId, down: bool) {
        if let Some(Terminal { state: TermState::Running(s), .. }) = &mut self.terminals[pane] {
            s.scroll_by(if down { -WHEEL_LINES } else { WHEEL_LINES });
        }
    }

    /// The sizes the last frame gave the terminals' screens, which they take on.
    pub(super) fn terminal_resize(&mut self) {
        for (t, screen) in self.terminals.iter_mut().zip(self.hitmap.terminal_screen) {
            if screen.is_empty() {
                continue;
            }
            if let Some(Terminal { state: TermState::Running(s), .. }) = t {
                s.resize(screen.height, screen.width);
            }
        }
    }

    fn terminal_size(&self, pane: PaneId) -> (u16, u16) {
        let screen = self.hitmap.terminal_screen[pane];
        if screen.is_empty() { DEFAULT_SIZE } else { (screen.height, screen.width) }
    }
}
