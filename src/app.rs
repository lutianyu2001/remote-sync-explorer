//! All interface state and the handling of keys, mouse events and worker results.
//! Nothing here touches the terminal; drawing lives in `ui`.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::config::{DropAction, MultiSelectModifier, UiConfig};
use crate::endpoint::{Address, Endpoint, Scheme};
use crate::event::{BrowseOp, Entry, Event, JobId, JobSnapshot, Mark, PaneId, ReqId};
use crate::exec::Exec;
use crate::hitmap::{HitMap, Target};
use crate::jobs::{Effect, JobState, OpKind, Queue, Transfer, same_name};
use crate::keymap::{Action, Keymap};
use crate::location::{Destination, Location, safe_name};
use crate::rclone::RpcError;
use crate::rclone::system_ssh::{SshAnswer, SshQuestion};
use crate::ui::text::shown;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const QUIT_GRACE: Duration = Duration::from_secs(5);
const ERROR_LOG_LEN: usize = 100;
const WHEEL_STEP: usize = 3;
/// Default button of the conflict dialog: a stray Enter or double click must not overwrite.
const SKIP: usize = 1;
/// Clicks on a dialog's buttons this soon after it appeared are ignored, so the second
/// click of a double click cannot land on a dialog that opened under the pointer.
const CLICK_GUARD: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Time,
}

impl SortKey {
    fn next(self) -> Self {
        match self {
            Self::Name => Self::Size,
            Self::Size => Self::Time,
            Self::Time => Self::Name,
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Name => tr!("sort-name"),
            Self::Size => tr!("sort-size"),
            Self::Time => tr!("sort-time"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneStatus {
    Loading,
    Ready,
    Error(String),
}

/// One visible row of a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Parent,
    Entry(usize),
}

#[derive(Debug, Clone)]
pub struct Pane {
    pub loc: Location,
    pub entries: Vec<Entry>,
    pub view: Vec<Row>,
    pub cursor: usize,
    pub offset: usize,
    pub marks: BTreeSet<Mark>,
    pub generation: u64,
    pub status: PaneStatus,
    pub filter: String,
    pub sort: SortKey,
    /// Entry to put the cursor on when the pending listing arrives.
    select: Option<Mark>,
    /// The listing request being worked on. At most one per pane, so a hung remote
    /// occupies one browse worker, not all of them.
    in_flight: Option<u64>,
}

impl Pane {
    fn new(loc: Location) -> Self {
        Self {
            loc,
            entries: Vec::new(),
            view: Vec::new(),
            cursor: 0,
            offset: 0,
            marks: BTreeSet::new(),
            generation: 0,
            status: PaneStatus::Loading,
            filter: String::new(),
            sort: SortKey::Name,
            select: None,
            in_flight: None,
        }
    }

    pub fn entry(&self, row: usize) -> Option<&Entry> {
        match self.view.get(row)? {
            Row::Parent => None,
            Row::Entry(i) => self.entries.get(*i),
        }
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entry(self.cursor)
    }

    pub fn is_marked(&self, row: usize) -> bool {
        self.entry(row).is_some_and(|e| self.marks.contains(&e.mark()))
    }

    /// The marked entries, or the entry under the cursor when nothing is marked.
    pub fn selection(&self) -> Vec<Entry> {
        if self.marks.is_empty() {
            self.current().cloned().into_iter().collect()
        } else {
            self.entries.iter().filter(|e| self.marks.contains(&e.mark())).cloned().collect()
        }
    }

    /// The row of the entry `mark` names: by name and kind, since S3 and others can hold
    /// a file and a directory of one name side by side.
    fn row_of(&self, mark: &Mark) -> Option<usize> {
        (0..self.view.len()).find(|&r| self.entry(r).is_some_and(|e| e.mark() == *mark))
    }

    /// Recomputes the visible rows after a listing, filter, sort or hidden-file change,
    /// keeping the cursor on the same entry when possible.
    fn rebuild(&mut self, show_hidden: bool) {
        let keep = self.select.take().or_else(|| self.current().map(Entry::mark));
        let at_parent = self.view.get(self.cursor) == Some(&Row::Parent);
        let filter = self.filter.to_lowercase();
        let mut idx: Vec<usize> = (0..self.entries.len())
            .filter(|&i| {
                let name = &self.entries[i].name;
                (show_hidden || !name.starts_with('.'))
                    && (filter.is_empty() || name.to_lowercase().contains(&filter))
            })
            .collect();
        let entries = &self.entries;
        idx.sort_by(|&a, &b| {
            let (a, b) = (&entries[a], &entries[b]);
            b.is_dir.cmp(&a.is_dir).then_with(|| match self.sort {
                SortKey::Name => name_order(&a.name, &b.name),
                SortKey::Size => b.size.cmp(&a.size).then_with(|| name_order(&a.name, &b.name)),
                SortKey::Time => b.mtime.cmp(&a.mtime).then_with(|| name_order(&a.name, &b.name)),
            })
        });
        self.view.clear();
        if self.loc.parent().is_some() {
            self.view.push(Row::Parent);
        }
        self.view.extend(idx.into_iter().map(Row::Entry));
        let visible: BTreeSet<Mark> =
            (0..self.view.len()).filter_map(|r| self.entry(r)).map(Entry::mark).collect();
        self.marks.retain(|m| visible.contains(m));
        self.cursor = match keep.and_then(|mark| self.row_of(&mark)) {
            Some(r) => r,
            None if at_parent => 0,
            None => self.cursor.min(self.view.len().saturating_sub(1)),
        };
    }

    fn move_cursor(&mut self, delta: isize) {
        let last = self.view.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }
}

fn name_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b))
}

/// The first row to show so that `cursor` is visible in `height` rows.
pub fn visible_offset(offset: usize, cursor: usize, height: usize, len: usize) -> usize {
    if height == 0 {
        return 0;
    }
    let max = len.saturating_sub(height);
    let offset = offset.min(max);
    if cursor < offset {
        cursor
    } else if cursor >= offset + height {
        cursor + 1 - height
    } else {
        offset
    }
}

/// An operation waiting for confirmation, a conflict check or a drop decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub kind: OpKind,
    pub src_dir: Location,
    pub items: Vec<Entry>,
    pub dst_dir: Option<Location>,
}

impl Plan {
    /// Bytes in the selected files (directory sizes are not known without a scan).
    pub fn file_bytes(&self) -> i64 {
        // Sizes come from the server; a sum past i64::MAX must not stop rext.
        self.items.iter().filter(|e| !e.is_dir).fold(0, |n, e| n.saturating_add(e.size.max(0)))
    }

    pub fn dir_count(&self) -> usize {
        self.items.iter().filter(|e| e.is_dir).count()
    }

    pub fn crosses_remotes(&self) -> bool {
        self.dst_dir.as_ref().is_some_and(|d| d.fs != self.src_dir.fs)
    }

    /// The items but those in `skip`; those in `overwrite` replace what the target has
    /// without it being checked again. Both hold lower-cased names, matching how
    /// conflicts are found.
    fn transfers(&self, skip: &BTreeSet<String>, overwrite: &BTreeSet<String>) -> Vec<Transfer> {
        self.items
            .iter()
            .filter(|e| !skip.contains(&e.name.to_lowercase()))
            .map(|e| Transfer {
                kind: self.kind,
                src_dir: self.src_dir.clone(),
                name: e.name.clone(),
                is_dir: e.is_dir,
                dst_dir: self.dst_dir.clone(),
                size: (!e.is_dir).then_some(e.size),
                overwrite: overwrite.contains(&e.name.to_lowercase()),
            })
            .collect()
    }

    /// Why this copy or move cannot happen, if it cannot.
    fn invalid_reason(&self) -> Option<String> {
        let dst = self.dst_dir.as_ref()?;
        if *dst == self.src_dir {
            return Some(tr!("same-directory"));
        }
        if self.items.iter().any(|e| e.is_dir && self.src_dir.join(&e.name).contains(dst)) {
            return Some(tr!("into-itself"));
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputPurpose {
    Mkdir,
    Rename(Mark),
    /// `scheme` is the protocol tag in front of the field; the text is what follows
    /// `scheme://`. Without a tag the text is a whole address.
    Goto {
        scheme: Option<Scheme>,
    },
    /// The password for a server opened by address.
    Login(Box<Address>),
    /// A question from ssh while logging in, such as `me@box's password:`. `remember`:
    /// whether to remember the password, or `None` when that is not offered. `notice`:
    /// why a remembered password did not spare the question, shown under the dialog.
    SshPrompt {
        question: String,
        remember: Option<bool>,
        notice: Option<String>,
    },
}

/// In the ssh question dialog, the "remember password" box comes after OK and Cancel
/// among the clickable areas.
pub const REMEMBER: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    Confirm {
        plan: Plan,
        focus: usize,
    },
    Conflict {
        plan: Plan,
        conflicts: Vec<Entry>,
        index: usize,
        skip: BTreeSet<String>,
        focus: usize,
    },
    /// Queued jobs that found their target taken as they were about to start, each with
    /// what the target has; one is shown at a time.
    JobConflict {
        conflicts: Vec<(JobId, Entry)>,
        index: usize,
        focus: usize,
    },
    Input {
        purpose: InputPurpose,
        text: String,
        cursor: usize,
    },
    Remotes {
        filter: String,
        cursor: usize,
    },
    Drop {
        plan: Plan,
        focus: usize,
        at: (u16, u16),
    },
    Quit {
        focus: usize,
    },
    Help {
        scroll: usize,
    },
    Errors {
        scroll: usize,
    },
    /// Typing a name filter for the active pane.
    Filter,
    /// A yes-or-no question of ssh's own, such as whether to trust a new host key.
    SshConfirm {
        question: String,
        focus: usize,
    },
}

impl Dialog {
    pub fn buttons(&self) -> Vec<String> {
        match self {
            Self::Confirm { plan, .. } => vec![plan.kind.title(), tr!("button-cancel")],
            Self::Conflict { .. } | Self::JobConflict { .. } => {
                Choice::ALL.iter().map(|c| c.label()).collect()
            }
            Self::Drop { .. } => drop_buttons(),
            Self::Quit { .. } => vec![tr!("button-stop-and-quit"), tr!("button-back")],
            Self::Input { .. } => vec![tr!("button-ok"), tr!("button-cancel")],
            Self::SshConfirm { .. } => vec![tr!("button-yes"), tr!("button-no")],
            Self::Remotes { .. } | Self::Help { .. } | Self::Errors { .. } | Self::Filter => {
                Vec::new()
            }
        }
    }

    fn focus_mut(&mut self) -> Option<&mut usize> {
        match self {
            Self::Confirm { focus, .. }
            | Self::Conflict { focus, .. }
            | Self::JobConflict { focus, .. }
            | Self::Drop { focus, .. }
            | Self::SshConfirm { focus, .. }
            | Self::Quit { focus } => Some(focus),
            _ => None,
        }
    }

    pub fn focus(&self) -> Option<usize> {
        match self {
            Self::Confirm { focus, .. }
            | Self::Conflict { focus, .. }
            | Self::JobConflict { focus, .. }
            | Self::Drop { focus, .. }
            | Self::SshConfirm { focus, .. }
            | Self::Quit { focus } => Some(*focus),
            _ => None,
        }
    }
}

/// The buttons of the conflict dialogs, always all five, so that none moves while the
/// dialog is open. The "all" ones act on the conflict shown and those after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Overwrite,
    Skip,
    OverwriteAll,
    SkipAll,
    Cancel,
}

impl Choice {
    const ALL: [Self; 5] =
        [Self::Overwrite, Self::Skip, Self::OverwriteAll, Self::SkipAll, Self::Cancel];

    fn label(self) -> String {
        match self {
            Self::Overwrite => tr!("button-overwrite"),
            Self::Skip => tr!("button-skip"),
            Self::OverwriteAll => tr!("button-overwrite-all"),
            Self::SkipAll => tr!("button-skip-all"),
            Self::Cancel => tr!("button-cancel"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Panes,
    /// The active pane's address bar: Enter edits it.
    Address,
    Queue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DropTarget {
    pub pane: PaneId,
    /// The directory row hovered, if the drop goes into a subdirectory.
    pub row: Option<usize>,
    pub dst: Location,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Drag {
    Idle,
    /// Left button down on an entry, not moved yet. `mark` identifies the entry (`None`
    /// for `..`), because a reload can move rows before the mouse moves.
    Pressed {
        pane: PaneId,
        row: usize,
        mark: Option<Mark>,
        at: (u16, u16),
    },
    Dragging {
        src: PaneId,
        src_dir: Location,
        items: Vec<Entry>,
        at: (u16, u16),
        target: Option<DropTarget>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitState {
    Running,
    /// Waiting for stopped jobs to end before exiting.
    Stopping {
        deadline: Instant,
    },
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub time: String,
    pub message: String,
}

enum Pending {
    Mkdir {
        dir: Location,
        name: String,
    },
    Rename {
        dir: Location,
        new_name: String,
        is_dir: bool,
    },
    /// Listing `dir` to see that nothing else there has the name `old` is renamed to.
    RenameCheck {
        dir: Location,
        old: Mark,
        new_name: String,
    },
    Probe(Plan),
    /// Listing the target of a job about to start, for its check number `check`.
    JobCheck {
        id: JobId,
        check: u64,
    },
    /// Obscuring a password before opening `path` on `endpoint` in `pane`.
    Login {
        pane: PaneId,
        endpoint: Endpoint,
        path: String,
    },
    /// Removing a moved directory's empty source; reload `dir` afterwards.
    Cleanup {
        dir: Location,
    },
}

/// Starting state for `App::new`.
pub struct AppInit {
    pub ui: UiConfig,
    pub keymap: Keymap,
    pub max_concurrent: usize,
    pub left: Location,
    pub right: Location,
    /// Remote names and their backend types.
    pub remotes: Vec<(String, String)>,
    pub home: PathBuf,
    pub cwd: PathBuf,
}

pub struct App {
    pub panes: [Pane; 2],
    pub active: PaneId,
    pub focus: Focus,
    pub queue: Queue,
    pub queue_cursor: usize,
    pub dialog: Option<Dialog>,
    pub drag: Drag,
    pub hitmap: HitMap,
    pub errors: VecDeque<LogEntry>,
    pub status: Option<String>,
    pub remotes: Vec<String>,
    /// Backend type of each remote, for the protocol tag.
    pub remote_types: HashMap<String, String>,
    pub show_hidden: bool,
    pub ui: UiConfig,
    pub keymap: Keymap,
    pub spinner: usize,
    pub quit: QuitState,
    /// Time zone for modification times; `None` means local time.
    pub display_tz: Option<chrono::FixedOffset>,
    exec: Exec,
    home: PathBuf,
    cwd: PathBuf,
    next_gen: u64,
    next_req: ReqId,
    pending: HashMap<ReqId, Pending>,
    /// Obscured passwords of servers opened by address this session, by login.
    passwords: HashMap<Endpoint, String>,
    /// Questions from ssh waiting for an answer, oldest first; the first is shown when no
    /// other dialog is.
    ssh_prompts: VecDeque<(SshQuestion, crossbeam_channel::Sender<Option<SshAnswer>>)>,
    ssh_prompt_shown: bool,
    /// Jobs that found their target taken, with what it has, oldest first; asked about
    /// once no other dialog is open.
    job_conflicts: Vec<(JobId, Entry)>,
    last_click: Option<((u16, u16), Instant)>,
    /// When the current dialog (or conflict step) appeared, for `CLICK_GUARD`.
    dialog_since: Option<Instant>,
}

impl App {
    pub fn new(init: AppInit, exec: Exec) -> Self {
        let passwords = [&init.left, &init.right]
            .into_iter()
            .filter_map(|l| l.endpoint.as_ref())
            .filter_map(|ep| Some((ep.login(), ep.pass.clone()?)))
            .collect();
        let mut app = Self {
            panes: [Pane::new(init.left.clone()), Pane::new(init.right.clone())],
            active: 0,
            focus: Focus::Panes,
            queue: Queue::new(init.max_concurrent),
            queue_cursor: 0,
            dialog: None,
            drag: Drag::Idle,
            hitmap: HitMap::default(),
            errors: VecDeque::new(),
            status: None,
            remotes: init.remotes.iter().map(|(name, _)| name.clone()).collect(),
            remote_types: init.remotes.into_iter().collect(),
            show_hidden: init.ui.show_hidden,
            ui: init.ui,
            keymap: init.keymap,
            spinner: 0,
            quit: QuitState::Running,
            display_tz: None,
            exec,
            home: init.home,
            cwd: init.cwd,
            next_gen: 1,
            next_req: 1,
            pending: HashMap::new(),
            passwords,
            ssh_prompts: VecDeque::new(),
            ssh_prompt_shown: false,
            job_conflicts: Vec::new(),
            last_click: None,
            dialog_since: None,
        };
        app.navigate(0, init.left);
        app.navigate(1, init.right);
        app
    }

    /// Hands the workers back for shutdown.
    pub fn into_exec(self) -> Exec {
        self.exec
    }

    pub fn handle(&mut self, event: Event, now: Instant) {
        let before = dialog_identity(self.dialog.as_ref());
        self.dispatch(event, now);
        self.sync_ssh_prompt();
        self.sync_job_conflicts();
        let after = dialog_identity(self.dialog.as_ref());
        if after != before {
            self.dialog_since = after.is_some().then_some(now);
        }
    }

    fn dispatch(&mut self, event: Event, now: Instant) {
        match event {
            Event::Key(key) => self.on_key(key, now),
            Event::Mouse(m) => self.on_mouse(m, now),
            Event::Resize(..) => {}
            Event::Tick => self.on_tick(now),
            Event::Listed { generation, result } => self.on_listed(generation, Some(result)),
            Event::ListSkipped { generation } => self.on_listed(generation, None),
            Event::OpDone { req, result } => self.on_op_done(req, result),
            Event::Probed { req, result } => self.on_probed(req, result),
            Event::Remotes(result) => match result {
                Ok(r) => {
                    self.remotes = r.iter().map(|(name, _)| name.clone()).collect();
                    self.remote_types = r.into_iter().collect();
                }
                Err(e) => self.log_error(tr!("listing-remotes-failed", error = e.to_string())),
            },
            Event::Obscured { req, result } => self.on_obscured(req, result),
            Event::SshPrompt { question, reply } => self.ssh_prompts.push_back((question, reply)),
            Event::Jobs(snaps) => self.on_jobs(snaps, now),
        }
    }

    /// Stores what the last frame drew and adopts its scroll offsets.
    pub fn set_hitmap(&mut self, hitmap: HitMap) {
        for (pane, area) in self.panes.iter_mut().zip(&hitmap.panes) {
            pane.offset = area.offset;
        }
        self.hitmap = hitmap;
    }

    fn page(&self, pane: PaneId) -> usize {
        usize::from(self.hitmap.panes[pane].list.height).max(1)
    }

    // ----- navigation -------------------------------------------------------------

    fn navigate(&mut self, pane: PaneId, loc: Location) {
        let p = &mut self.panes[pane];
        p.loc = loc;
        p.entries.clear();
        p.view.clear();
        p.marks.clear();
        p.filter.clear();
        p.cursor = 0;
        p.offset = 0;
        self.request_listing(pane);
    }

    /// Lists the pane's location again, keeping what is shown until the result arrives.
    fn reload(&mut self, pane: PaneId) {
        if self.panes[pane].select.is_none() {
            self.panes[pane].select = self.panes[pane].current().map(Entry::mark);
        }
        self.request_listing(pane);
    }

    fn request_listing(&mut self, pane: PaneId) {
        let generation = self.next_gen;
        self.next_gen += 1;
        let p = &mut self.panes[pane];
        p.generation = generation;
        p.status = PaneStatus::Loading;
        self.exec.set_wanted([self.panes[0].generation, self.panes[1].generation]);
        // With a listing already running, the new one is sent when that one returns.
        if self.panes[pane].in_flight.is_none() {
            self.send_listing(pane);
        }
    }

    fn send_listing(&mut self, pane: PaneId) {
        let p = &mut self.panes[pane];
        p.in_flight = Some(p.generation);
        self.exec.list(p.generation, p.loc.clone());
    }

    fn reload_showing(&mut self, dirs: &[Location], select: Option<&Mark>) {
        for pane in 0..2 {
            if dirs.contains(&self.panes[pane].loc) {
                if let Some(mark) = select {
                    self.panes[pane].select = Some(mark.clone());
                }
                self.reload(pane);
            }
        }
    }

    /// A listing returned (`None`: the worker skipped it as no longer wanted).
    fn on_listed(&mut self, generation: u64, result: Option<Result<Vec<Entry>, RpcError>>) {
        let Some(pane) = self.panes.iter().position(|p| p.in_flight == Some(generation)) else {
            return;
        };
        self.panes[pane].in_flight = None;
        let (Some(result), true) = (result, self.panes[pane].generation == generation) else {
            // Outdated: the pane moved on meanwhile, so list what it shows now.
            self.send_listing(pane);
            return;
        };
        let show_hidden = self.show_hidden;
        let p = &mut self.panes[pane];
        match result {
            Ok(mut entries) => {
                let listed = entries.len();
                entries.retain(|e| safe_name(&e.name));
                let left_out = listed - entries.len();
                p.entries = entries;
                p.status = PaneStatus::Ready;
                p.rebuild(show_hidden);
                if left_out > 0 {
                    let dir = shown(&p.loc.to_string()).into_owned();
                    let message = tr!("names-unsafe", count = left_out, dir = dir);
                    self.log_error(message.clone());
                    self.status = Some(message);
                }
            }
            Err(e) => {
                p.entries.clear();
                p.select = None;
                p.rebuild(show_hidden);
                let msg = with_hint(&e.message);
                p.status = PaneStatus::Error(msg.clone());
                let loc = p.loc.clone();
                // Possibly a wrong password: ask again next time instead of reusing it.
                if let Some(ep) = &loc.endpoint {
                    self.passwords.remove(&ep.login());
                }
                self.log_error(tr!("listing-failed", dir = loc.to_string(), error = msg.as_str()));
            }
        }
    }

    fn enter(&mut self, pane: PaneId, row: usize) {
        let p = &self.panes[pane];
        match p.view.get(row) {
            Some(Row::Parent) => self.go_parent(pane),
            Some(Row::Entry(_)) => {
                if let Some(e) = p.entry(row).filter(|e| e.is_dir) {
                    let loc = p.loc.join(&e.name);
                    self.navigate(pane, loc);
                }
            }
            None => {}
        }
    }

    fn go_parent(&mut self, pane: PaneId) {
        if let Some((parent, name)) = self.panes[pane].loc.parent() {
            self.navigate(pane, parent);
            self.panes[pane].select = Some(Mark { is_dir: true, name });
        }
    }

    fn rebuild_all(&mut self) {
        let show_hidden = self.show_hidden;
        for p in &mut self.panes {
            p.rebuild(show_hidden);
        }
    }

    // ----- keys -------------------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent, now: Instant) {
        if self.quit != QuitState::Running {
            return;
        }
        self.status = None;
        if key.code == KeyCode::Esc && self.drag != Drag::Idle {
            self.drag = Drag::Idle;
            return;
        }
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if self.dialog.is_some() {
            if ctrl_c {
                self.dialog = None;
            } else {
                self.dialog_key(key, now);
            }
            return;
        }
        if ctrl_c {
            self.action(Action::Quit, now);
            return;
        }
        if self.focus == Focus::Queue {
            if key.code == KeyCode::Esc {
                self.focus = Focus::Panes;
            } else if let Some(action) = self.keymap.queue_action(&key) {
                self.queue_action(action, now);
            }
            return;
        }
        if self.focus == Focus::Address {
            match key.code {
                KeyCode::Enter => self.action(Action::Goto, now),
                KeyCode::Esc => self.focus = Focus::Panes,
                // The entries' keys wait for the entries.
                _ => {
                    if let Some(action) =
                        self.keymap.pane_action(&key).filter(|a| works_in_address_bar(*a))
                    {
                        self.action(action, now);
                    }
                }
            }
            return;
        }
        if key.code == KeyCode::Esc {
            self.panes[self.active].marks.clear();
            return;
        }
        if let Some(action) = self.keymap.pane_action(&key) {
            self.action(action, now);
        }
    }

    fn queue_action(&mut self, action: Action, now: Instant) {
        let len = self.queue.jobs.len();
        let last = len.saturating_sub(1);
        let id = self.queue.jobs.get(self.queue_cursor).map(|j| j.id);
        match action {
            Action::Up => self.queue_cursor = self.queue_cursor.saturating_sub(1),
            Action::Down => self.queue_cursor = (self.queue_cursor + 1).min(last),
            Action::PageUp => self.queue_cursor = self.queue_cursor.saturating_sub(8),
            Action::PageDown => self.queue_cursor = (self.queue_cursor + 8).min(last),
            Action::Home => self.queue_cursor = 0,
            Action::End => self.queue_cursor = last,
            Action::QueueCancel => {
                if let Some(id) = id {
                    let effects = self.queue.cancel(id);
                    self.run_effects(effects);
                }
            }
            Action::QueueRetry => {
                if let Some(id) = id {
                    let effects = self.queue.retry(id);
                    self.run_effects(effects);
                }
            }
            Action::QueueClear => {
                self.queue.clear_finished();
                self.queue_cursor = self.queue_cursor.min(self.queue.jobs.len().saturating_sub(1));
            }
            Action::Queue => self.focus = Focus::Panes,
            other => self.action(other, now),
        }
    }

    /// Runs an action from a key or a click on the key hints.
    pub fn action(&mut self, action: Action, _now: Instant) {
        let pane = self.active;
        let rows_per_page = self.page(pane);
        match action {
            Action::SwitchPane => self.active = 1 - self.active,
            Action::Up => self.panes[pane].move_cursor(-1),
            Action::Down => self.panes[pane].move_cursor(1),
            Action::PageUp => {
                self.panes[pane].move_cursor(-isize::try_from(rows_per_page).unwrap_or(1));
            }
            Action::PageDown => {
                self.panes[pane].move_cursor(isize::try_from(rows_per_page).unwrap_or(1));
            }
            Action::Home => self.panes[pane].cursor = 0,
            Action::End => self.panes[pane].cursor = self.panes[pane].view.len().saturating_sub(1),
            Action::Enter => self.enter(pane, self.panes[pane].cursor),
            Action::Parent => self.go_parent(pane),
            Action::Mark => {
                let p = &mut self.panes[pane];
                if let Some(mark) = p.current().map(Entry::mark) {
                    toggle(&mut p.marks, mark);
                }
                p.move_cursor(1);
            }
            Action::MarkAll => {
                let p = &mut self.panes[pane];
                let all: BTreeSet<Mark> =
                    (0..p.view.len()).filter_map(|r| p.entry(r)).map(Entry::mark).collect();
                p.marks = if p.marks == all { BTreeSet::new() } else { all };
            }
            Action::Filter => self.dialog = Some(Dialog::Filter),
            Action::Sort => {
                let p = &mut self.panes[pane];
                p.sort = p.sort.next();
                let show_hidden = self.show_hidden;
                p.rebuild(show_hidden);
                self.status = Some(tr!("sorted-by", key = p.sort.label()));
            }
            Action::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.rebuild_all();
            }
            Action::Goto => {
                let (scheme, text) = self.panes[pane].loc.address();
                self.dialog = Some(input(InputPurpose::Goto { scheme }, text));
            }
            Action::Remote => {
                self.exec.remotes();
                self.dialog = Some(Dialog::Remotes { filter: String::new(), cursor: 0 });
            }
            Action::Swap => {
                self.panes.swap(0, 1);
                self.exec.set_wanted([self.panes[0].generation, self.panes[1].generation]);
            }
            Action::Refresh => self.reload(pane),
            Action::Copy => self.start_transfer(OpKind::Copy),
            Action::Move => self.start_transfer(OpKind::Move),
            Action::Delete => self.start_delete(),
            Action::Rename => {
                if let Some(e) = self.panes[pane].current() {
                    let name = e.name.clone();
                    self.dialog = Some(input(InputPurpose::Rename(e.mark()), name));
                }
            }
            Action::Mkdir => self.dialog = Some(input(InputPurpose::Mkdir, String::new())),
            Action::OverwriteMode => {
                self.queue.overwrite = !self.queue.overwrite;
                self.status = Some(if self.queue.overwrite {
                    tr!("overwrite-on")
                } else {
                    tr!("overwrite-off")
                });
            }
            Action::Queue | Action::FocusQueue => {
                self.focus = Focus::Queue;
                self.queue_cursor = self.queue_cursor.min(self.queue.jobs.len().saturating_sub(1));
            }
            Action::ErrorLog => self.dialog = Some(Dialog::Errors { scroll: 0 }),
            Action::Help => self.dialog = Some(Dialog::Help { scroll: 0 }),
            Action::Quit => {
                if self.queue.unfinished_count() > 0 {
                    self.dialog = Some(Dialog::Quit { focus: 1 });
                } else {
                    self.quit = QuitState::Done;
                }
            }
            Action::FocusLeft | Action::FocusRight => {
                self.focus = Focus::Panes;
                self.active = usize::from(action == Action::FocusRight);
            }
            Action::FocusLeftAddress | Action::FocusRightAddress => {
                self.focus = Focus::Address;
                self.active = usize::from(action == Action::FocusRightAddress);
            }
            Action::QueueCancel | Action::QueueRetry | Action::QueueClear => {}
        }
    }

    // ----- dialogs ----------------------------------------------------------------

    fn dialog_key(&mut self, key: KeyEvent, now: Instant) {
        let Some(mut dialog) = self.dialog.take() else { return };
        match &mut dialog {
            Dialog::Filter => {
                let pane = self.active;
                match key.code {
                    KeyCode::Esc => {
                        self.panes[pane].filter.clear();
                        self.rebuild_all();
                        return;
                    }
                    KeyCode::Enter => return,
                    KeyCode::Backspace => {
                        self.panes[pane].filter.pop();
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.panes[pane].filter.push(c);
                    }
                    KeyCode::Up => self.panes[pane].move_cursor(-1),
                    KeyCode::Down => self.panes[pane].move_cursor(1),
                    _ => {}
                }
                self.rebuild_all();
            }
            Dialog::Input { purpose, text, cursor } => match key.code {
                KeyCode::Esc => return,
                KeyCode::Enter => {
                    self.submit_input(dialog);
                    return;
                }
                KeyCode::Tab => {
                    if let InputPurpose::SshPrompt { remember: Some(r), .. } = purpose {
                        *r = !*r;
                    }
                }
                _ => edit_input(purpose, text, cursor, key),
            },
            Dialog::Remotes { filter, cursor } => match key.code {
                KeyCode::Esc => return,
                KeyCode::Enter => {
                    let choice = self.remote_choices(filter).get(*cursor).cloned();
                    if let Some(loc) = choice.map(|(_, loc)| loc) {
                        self.navigate(self.active, loc);
                    }
                    return;
                }
                KeyCode::Up => *cursor = cursor.saturating_sub(1),
                KeyCode::Down => {
                    *cursor =
                        (*cursor + 1).min(self.remote_choices(filter).len().saturating_sub(1));
                }
                KeyCode::Backspace => {
                    filter.pop();
                    *cursor = 0;
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    filter.push(c);
                    *cursor = 0;
                }
                _ => {}
            },
            Dialog::Help { scroll } | Dialog::Errors { scroll } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll = (*scroll + 1).min(self.hitmap.scroll_max);
                }
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                KeyCode::PageDown => *scroll = (*scroll + 10).min(self.hitmap.scroll_max),
                _ => return,
            },
            Dialog::Confirm { .. }
            | Dialog::Conflict { .. }
            | Dialog::JobConflict { .. }
            | Dialog::Drop { .. }
            | Dialog::SshConfirm { .. }
            | Dialog::Quit { .. } => {
                let n = dialog.buttons().len();
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Enter => {
                        let focus = dialog.focus().unwrap_or(0);
                        self.press_button(dialog, focus, now);
                        return;
                    }
                    KeyCode::Left | KeyCode::BackTab | KeyCode::Up => {
                        if let Some(f) = dialog.focus_mut() {
                            *f = (*f + n - 1) % n;
                        }
                    }
                    KeyCode::Right | KeyCode::Tab | KeyCode::Down => {
                        if let Some(f) = dialog.focus_mut() {
                            *f = (*f + 1) % n;
                        }
                    }
                    _ => {}
                }
            }
        }
        self.dialog = Some(dialog);
    }

    /// The protocol a location is reached by (`sftp`, `s3`, …); `None` for local paths
    /// and remotes of unknown type.
    pub fn protocol(&self, loc: &Location) -> Option<String> {
        if let Some(ep) = &loc.endpoint {
            return Some(ep.scheme.name().to_owned());
        }
        if loc.is_local() {
            return None;
        }
        let name = loc.fs.trim_end_matches(':');
        let name = name.split(',').next().unwrap_or(name);
        match name.strip_prefix(':') {
            Some(backend) => Some(backend.to_owned()),
            None => self.remote_types.get(name).filter(|t| !t.is_empty()).cloned(),
        }
    }

    /// Whether the Go to text (behind tag `scheme`) differs from the active pane's address.
    pub fn address_edited(&self, scheme: Option<Scheme>, text: &str) -> bool {
        self.panes[self.active].loc.address() != (scheme, text.trim().to_owned())
    }

    /// Where the Go to text (with no tag) leads, when it names a ready location.
    pub fn input_location(&self, purpose: &InputPurpose, text: &str) -> Option<Location> {
        match purpose {
            InputPurpose::Goto { scheme: None } => {
                match Location::parse_address(text, None, &self.home, &self.cwd).ok()? {
                    Destination::Ready(loc) => Some(loc),
                    Destination::Login(_) => None,
                }
            }
            _ => None,
        }
    }

    /// The protocol tag for the Go to field: the tag itself, or the protocol the typed
    /// address leads to.
    pub fn input_protocol(&self, purpose: &InputPurpose, text: &str) -> Option<String> {
        match purpose {
            InputPurpose::Goto { scheme: Some(s) } => Some(s.name().to_owned()),
            InputPurpose::Goto { scheme: None } => {
                match Location::parse_address(text, None, &self.home, &self.cwd).ok()? {
                    Destination::Ready(loc) => self.protocol(&loc),
                    Destination::Login(a) => Some(a.endpoint.scheme.name().to_owned()),
                }
            }
            InputPurpose::Login(_)
            | InputPurpose::SshPrompt { .. }
            | InputPurpose::Mkdir
            | InputPurpose::Rename(_) => None,
        }
    }

    /// The remote picker's choices for a filter: local home first, then the remotes.
    pub fn remote_choices(&self, filter: &str) -> Vec<(String, Location)> {
        let f = filter.to_lowercase();
        std::iter::once((tr!("remote-local"), Location::local(&self.home)))
            .chain(self.remotes.iter().map(|r| (format!("{r}:"), Location::remote_root(r))))
            .filter(|(label, _)| f.is_empty() || label.to_lowercase().contains(&f))
            .collect()
    }

    fn submit_input(&mut self, dialog: Dialog) {
        let Dialog::Input { purpose, text, .. } = dialog else { return };
        let pane = self.active;
        let dir = self.panes[pane].loc.clone();
        // Done with the address bar: back to the entries.
        if matches!(purpose, InputPurpose::Goto { .. }) && self.focus == Focus::Address {
            self.focus = Focus::Panes;
        }
        match purpose {
            // An unchanged address reloads, like the address bar's button.
            InputPurpose::Goto { scheme } if !self.address_edited(scheme, &text) => {
                self.reload(pane);
            }
            InputPurpose::Goto { scheme } => {
                match Location::parse_address(&text, scheme, &self.home, &self.cwd) {
                    Ok(Destination::Ready(loc)) => self.navigate(pane, loc),
                    Ok(Destination::Login(address)) => self.log_in(pane, address),
                    Err(e) => self.status = Some(e),
                }
            }
            InputPurpose::Login(mut address) => {
                address.password = Some(text);
                self.log_in(pane, *address);
            }
            InputPurpose::SshPrompt { remember, .. } => {
                self.ssh_prompt_shown = false;
                if let Some((_, reply)) = self.ssh_prompts.pop_front() {
                    let _ = reply.send(Some(SshAnswer { text, remember: remember == Some(true) }));
                }
            }
            InputPurpose::Mkdir => {
                if let Err(e) = check_name(&text, &self.panes[pane]) {
                    self.status = Some(e);
                    return;
                }
                let req = self.req(Pending::Mkdir { dir: dir.clone(), name: text.clone() });
                self.exec.op(req, BrowseOp::Mkdir(dir.join(&text)));
            }
            InputPurpose::Rename(old) => {
                if text == old.name {
                    return;
                }
                // Overwrite mode renames onto an existing name.
                let overwrite = self.queue.overwrite;
                let checked = if overwrite {
                    valid_name(&text)
                } else {
                    check_name(&text, &self.panes[pane])
                };
                if let Err(e) = checked {
                    self.status = Some(e);
                    return;
                }
                if overwrite {
                    self.rename(dir, &old, text);
                } else {
                    // The pane may have listed the directory before an entry of the new
                    // name appeared there, which the rename would replace: ask the server.
                    let req =
                        self.req(Pending::RenameCheck { dir: dir.clone(), old, new_name: text });
                    self.exec.probe(req, dir);
                }
            }
        }
    }

    fn rename(&mut self, dir: Location, old: &Mark, new_name: String) {
        let is_dir = old.is_dir;
        let op = BrowseOp::Rename { from: dir.join(&old.name), to: dir.join(&new_name), is_dir };
        let req = self.req(Pending::Rename { dir, new_name, is_dir });
        self.exec.op(req, op);
    }

    /// The directory was listed for a rename: it goes ahead unless another entry there
    /// has the new name, in any case.
    fn on_rename_checked(
        &mut self,
        dir: Location,
        old: &Mark,
        new_name: String,
        result: Result<Vec<Entry>, RpcError>,
    ) {
        if self.quit != QuitState::Running {
            return;
        }
        match result {
            Ok(entries) => {
                match entries.iter().find(|e| same_name(&e.name, &new_name) && e.mark() != *old) {
                    // Overwrite mode, if it came on meanwhile, renames onto it.
                    Some(e) if !self.queue.overwrite => {
                        self.fail(tr!("name-exists", name = shown(&e.name)));
                        // The pane did not show it.
                        self.reload_showing(&[dir], None);
                    }
                    _ => self.rename(dir, old, new_name),
                }
            }
            Err(e) => self.fail(tr!("probe-failed", error = with_hint(&e.message))),
        }
    }

    /// Opens a server by address once its password is settled: a known one is reused,
    /// a missing one is asked for, and a typed one is obscured for rclone first.
    fn log_in(&mut self, pane: PaneId, address: Address) {
        let Address { mut endpoint, path, password } = address;
        let Some(clear) = password else {
            if let Some(pass) = self.passwords.get(&endpoint.login()) {
                endpoint.pass = Some(pass.clone());
                self.navigate(pane, Location::on(endpoint, &path));
            } else {
                let address = Address { endpoint, path, password: None };
                self.dialog = Some(input(InputPurpose::Login(Box::new(address)), String::new()));
            }
            return;
        };
        if clear.is_empty() {
            self.navigate(pane, Location::on(endpoint, &path));
            return;
        }
        let req = self.req(Pending::Login { pane, endpoint, path });
        self.exec.obscure(req, clear);
    }

    /// Shows the oldest question from ssh once no other dialog is open, and answers
    /// `None` (cancel) when its dialog goes away without OK.
    fn sync_ssh_prompt(&mut self) {
        let showing = matches!(
            &self.dialog,
            Some(
                Dialog::Input { purpose: InputPurpose::SshPrompt { .. }, .. }
                    | Dialog::SshConfirm { .. }
            )
        );
        if self.ssh_prompt_shown && !showing {
            self.ssh_prompt_shown = false;
            if let Some((_, reply)) = self.ssh_prompts.pop_front() {
                let _ = reply.send(None);
            }
        }
        if !self.ssh_prompt_shown
            && self.dialog.is_none()
            && let Some((q, _)) = self.ssh_prompts.front()
        {
            self.dialog = Some(if q.confirm {
                // No is the default: trusting a server's key must be a choice.
                Dialog::SshConfirm { question: q.prompt.clone(), focus: 1 }
            } else {
                let purpose = InputPurpose::SshPrompt {
                    question: q.prompt.clone(),
                    remember: q.can_remember.then_some(false),
                    notice: q.notice.clone(),
                };
                input(purpose, String::new())
            });
            self.ssh_prompt_shown = true;
        }
    }

    /// Asks about the jobs that found their target taken, once no other dialog is open.
    /// Those found while it is open join it, so that its "all" choices take them in.
    fn sync_job_conflicts(&mut self) {
        if self.job_conflicts.is_empty() || self.quit != QuitState::Running {
            return;
        }
        match &mut self.dialog {
            None => {
                let conflicts = std::mem::take(&mut self.job_conflicts);
                self.dialog = Some(Dialog::JobConflict { conflicts, index: 0, focus: SKIP });
            }
            Some(Dialog::JobConflict { conflicts, .. }) => {
                conflicts.append(&mut self.job_conflicts);
            }
            Some(_) => {}
        }
    }

    fn on_obscured(&mut self, req: ReqId, result: Result<String, RpcError>) {
        let Some(Pending::Login { pane, mut endpoint, path }) = self.pending.remove(&req) else {
            return;
        };
        match result {
            Ok(pass) => {
                self.passwords.insert(endpoint.login(), pass.clone());
                endpoint.pass = Some(pass);
                self.navigate(pane, Location::on(endpoint, &path));
            }
            Err(e) => self.fail(tr!("password-failed", error = e.message)),
        }
    }

    fn req(&mut self, pending: Pending) -> ReqId {
        let id = self.next_req;
        self.next_req += 1;
        self.pending.insert(id, pending);
        id
    }

    fn press_button(&mut self, dialog: Dialog, button: usize, now: Instant) {
        match dialog {
            Dialog::Confirm { plan, .. } => {
                if button == 0 {
                    self.confirmed(plan);
                }
            }
            Dialog::Drop { mut plan, .. } => match button {
                0 => self.check_conflicts(plan),
                1 => {
                    plan.kind = OpKind::Move;
                    self.dialog = Some(Dialog::Confirm { plan, focus: 0 });
                }
                _ => {}
            },
            Dialog::Conflict { plan, conflicts, mut index, mut skip, .. } => {
                match Choice::ALL.get(button) {
                    Some(Choice::Overwrite) => index += 1,
                    Some(Choice::Skip) => {
                        skip.insert(conflicts[index].name.to_lowercase());
                        index += 1;
                    }
                    Some(Choice::OverwriteAll) => index = conflicts.len(),
                    Some(Choice::SkipAll) => {
                        skip.extend(conflicts[index..].iter().map(|e| e.name.to_lowercase()));
                        index = conflicts.len();
                    }
                    Some(Choice::Cancel) | None => return,
                }
                if index < conflicts.len() {
                    self.dialog =
                        Some(Dialog::Conflict { plan, conflicts, index, skip, focus: SKIP });
                } else {
                    // What was not skipped is to be overwritten.
                    let overwrite = conflicts
                        .iter()
                        .map(|e| e.name.to_lowercase())
                        .filter(|n| !skip.contains(n))
                        .collect();
                    self.enqueue(&plan, &skip, &overwrite);
                }
            }
            Dialog::JobConflict { conflicts, index, .. } => {
                let (overwrite, decided) = match Choice::ALL.get(button) {
                    Some(Choice::Overwrite) => (true, 1),
                    Some(Choice::Skip) => (false, 1),
                    Some(Choice::OverwriteAll) => (true, conflicts.len() - index),
                    Some(Choice::SkipAll) => (false, conflicts.len() - index),
                    // All the jobs not decided stay failed; a retry checks again.
                    Some(Choice::Cancel) | None => return,
                };
                for (id, _) in &conflicts[index..index + decided] {
                    if overwrite {
                        let effects = self.queue.approve(*id);
                        self.run_effects(effects);
                    } else {
                        self.queue.skip(*id);
                    }
                }
                let index = index + decided;
                if index < conflicts.len() {
                    self.dialog = Some(Dialog::JobConflict { conflicts, index, focus: SKIP });
                }
            }
            Dialog::Quit { .. } => {
                if button == 0 {
                    self.stop_and_quit(now);
                }
            }
            Dialog::SshConfirm { .. } => {
                self.ssh_prompt_shown = false;
                if let Some((_, reply)) = self.ssh_prompts.pop_front() {
                    let yes = SshAnswer { text: "yes".to_owned(), remember: false };
                    let _ = reply.send((button == 0).then_some(yes));
                }
            }
            Dialog::Input {
                purpose: InputPurpose::SshPrompt { question, remember: Some(r), notice },
                text,
                cursor,
            } if button == REMEMBER => {
                let purpose = InputPurpose::SshPrompt { question, remember: Some(!r), notice };
                self.dialog = Some(Dialog::Input { purpose, text, cursor });
            }
            dialog @ Dialog::Input { .. } => {
                if button == 0 {
                    self.submit_input(dialog);
                }
            }
            Dialog::Remotes { .. }
            | Dialog::Help { .. }
            | Dialog::Errors { .. }
            | Dialog::Filter => {}
        }
    }

    // ----- operations -------------------------------------------------------------

    fn start_transfer(&mut self, kind: OpKind) {
        let src = &self.panes[self.active];
        let dst = &self.panes[1 - self.active];
        let plan = Plan {
            kind,
            src_dir: src.loc.clone(),
            items: src.selection(),
            dst_dir: Some(dst.loc.clone()),
        };
        if plan.items.is_empty() {
            self.status = Some(tr!("nothing-selected"));
        } else if let Some(reason) = plan.invalid_reason() {
            self.status = Some(reason);
        } else {
            self.dialog = Some(Dialog::Confirm { plan, focus: 0 });
        }
    }

    fn start_delete(&mut self) {
        let src = &self.panes[self.active];
        let plan = Plan {
            kind: OpKind::Delete,
            src_dir: src.loc.clone(),
            items: src.selection(),
            dst_dir: None,
        };
        if plan.items.is_empty() {
            self.status = Some(tr!("nothing-selected"));
        } else {
            // Deleting defaults to Cancel.
            self.dialog = Some(Dialog::Confirm { plan, focus: 1 });
        }
    }

    fn confirmed(&mut self, plan: Plan) {
        if plan.kind == OpKind::Delete {
            self.enqueue(&plan, &BTreeSet::new(), &BTreeSet::new());
        } else {
            self.check_conflicts(plan);
        }
    }

    /// Looks for names that already exist in the target by listing it now. A pane showing
    /// the target may have listed it minutes ago, and a file created since then would be
    /// overwritten without asking. Each job checks again as it starts. In overwrite mode
    /// nothing is checked.
    fn check_conflicts(&mut self, plan: Plan) {
        let Some(dst) = plan.dst_dir.clone() else { return };
        if self.queue.overwrite {
            self.enqueue(&plan, &BTreeSet::new(), &BTreeSet::new());
            return;
        }
        let req = self.req(Pending::Probe(plan));
        self.exec.probe(req, dst);
        self.status = Some(tr!("checking-target"));
    }

    fn resolve_conflicts(&mut self, plan: Plan, existing: &[Entry]) {
        let conflicts: Vec<Entry> = existing
            .iter()
            .filter(|e| plan.items.iter().any(|i| same_name(&i.name, &e.name)))
            .cloned()
            .collect();
        // Overwrite mode, if it came on meanwhile, replaces them.
        if conflicts.is_empty() || self.queue.overwrite {
            self.enqueue(&plan, &BTreeSet::new(), &BTreeSet::new());
        } else {
            self.dialog = Some(Dialog::Conflict {
                plan,
                conflicts,
                index: 0,
                skip: BTreeSet::new(),
                focus: SKIP,
            });
        }
    }

    fn enqueue(&mut self, plan: &Plan, skip: &BTreeSet<String>, overwrite: &BTreeSet<String>) {
        let transfers = plan.transfers(skip, overwrite);
        let n = transfers.len();
        if n == 0 {
            self.status = Some(tr!("all-skipped"));
            return;
        }
        let effects = self.queue.push(transfers);
        self.run_effects(effects);
        for p in &mut self.panes {
            if p.loc == plan.src_dir {
                p.marks.clear();
            }
        }
        self.status = Some(tr!("queued", op = plan.kind.verb(), count = n));
    }

    fn run_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Start { id, method, params } => self.exec.submit(id, method, params),
                Effect::Check { id, check, dir } => {
                    let req = self.req(Pending::JobCheck { id, check });
                    self.exec.probe(req, dir);
                }
                Effect::Stop(id) => self.exec.stop(id),
                Effect::Finished { id, touched, error } => {
                    let done =
                        self.queue.jobs.iter().find(|j| j.id == id && j.state == JobState::Done);
                    if let Some(t) = done.map(|j| &j.transfer)
                        && t.kind == OpKind::Move
                        && t.is_dir
                    {
                        // rclone moves the contents but leaves the emptied source root.
                        let dir = t.src_dir.clone();
                        let gone = dir.join(&t.name);
                        let req = self.req(Pending::Cleanup { dir });
                        self.exec.op(req, BrowseOp::RemoveEmptyDir(gone));
                    }
                    if let Some(e) = error {
                        let label = self
                            .queue
                            .jobs
                            .iter()
                            .find(|j| j.id == id)
                            .map(job_label)
                            .unwrap_or_default();
                        self.log_error(format!("{label}: {}", with_hint(&e)));
                    }
                    self.reload_showing(&touched, None);
                }
            }
        }
    }

    fn on_jobs(&mut self, snaps: Vec<JobSnapshot>, now: Instant) {
        let effects = self.queue.apply(snaps);
        self.run_effects(effects);
        self.check_quit(now);
    }

    fn on_op_done(&mut self, req: ReqId, result: Result<(), RpcError>) {
        let Some(pending) = self.pending.remove(&req) else { return };
        match (pending, result) {
            (Pending::Mkdir { dir, name }, Ok(())) => {
                self.reload_showing(&[dir], Some(&Mark { is_dir: true, name }));
            }
            (Pending::Rename { dir, new_name: name, is_dir }, Ok(())) => {
                self.reload_showing(&[dir], Some(&Mark { is_dir, name }));
            }
            (Pending::Mkdir { name, .. }, Err(e)) => {
                self.fail(tr!("mkdir-failed", name = name, error = e.message));
            }
            (Pending::Rename { new_name, .. }, Err(e)) => {
                self.fail(tr!("rename-failed", name = new_name, error = e.message));
            }
            (Pending::Cleanup { dir }, Ok(())) => self.reload_showing(&[dir], None),
            (Pending::Cleanup { dir }, Err(e)) => {
                self.fail(tr!("cleanup-failed", dir = dir.to_string(), error = e.message));
            }
            (
                Pending::Probe(_)
                | Pending::JobCheck { .. }
                | Pending::RenameCheck { .. }
                | Pending::Login { .. },
                _,
            ) => {}
        }
    }

    fn on_probed(&mut self, req: ReqId, result: Result<Vec<Entry>, RpcError>) {
        match self.pending.remove(&req) {
            Some(Pending::Probe(plan)) => self.on_target_listed(plan, result),
            Some(Pending::JobCheck { id, check }) => self.on_job_checked(id, check, result),
            Some(Pending::RenameCheck { dir, old, new_name }) => {
                self.on_rename_checked(dir, &old, new_name, result);
            }
            _ => {}
        }
    }

    /// The target of an operation about to be queued was listed.
    fn on_target_listed(&mut self, plan: Plan, result: Result<Vec<Entry>, RpcError>) {
        self.status = None;
        if self.dialog.is_some() || self.quit != QuitState::Running {
            // Never replace a dialog the user is looking at, or start work while quitting.
            self.fail(tr!("probe-late", op = plan.kind.verb()));
            return;
        }
        match result {
            Ok(entries) => self.resolve_conflicts(plan, &entries),
            // A missing target directory is created by the transfer.
            Err(e) if is_not_found(&e) => self.resolve_conflicts(plan, &[]),
            Err(e) => self.fail(tr!("probe-failed", error = with_hint(&e.message))),
        }
    }

    /// The target of a job about to start was listed. Something of its name there fails
    /// the job, as an error, and the user is asked what to do with it. Nothing changes
    /// when the check was cancelled since, even if the job is checking again.
    fn on_job_checked(&mut self, id: JobId, check: u64, result: Result<Vec<Entry>, RpcError>) {
        let Some(name) = self.queue.checking(id, check).map(|j| j.transfer.name.clone()) else {
            return;
        };
        let outcome = match result {
            Ok(entries) => match entries.into_iter().find(|e| same_name(&e.name, &name)) {
                // Overwrite mode, if it came on meanwhile, replaces it.
                Some(existing) if !self.queue.overwrite => {
                    let error = tr!("job-exists", name = shown(&existing.name));
                    self.job_conflicts.push((id, existing));
                    Err(error)
                }
                _ => Ok(()),
            },
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(tr!("probe-failed", error = with_hint(&e.message))),
        };
        let effects = self.queue.checked(id, check, outcome);
        self.run_effects(effects);
    }

    fn fail(&mut self, message: String) {
        self.status = Some(message.clone());
        self.log_error(message);
    }

    pub fn log_error(&mut self, message: String) {
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        self.errors.push_front(LogEntry { time, message });
        self.errors.truncate(ERROR_LOG_LEN);
    }

    fn stop_and_quit(&mut self, now: Instant) {
        let effects = self.queue.cancel_all();
        self.run_effects(effects);
        self.quit = QuitState::Stopping { deadline: now + QUIT_GRACE };
        self.check_quit(now);
    }

    fn check_quit(&mut self, now: Instant) {
        if let QuitState::Stopping { deadline } = self.quit
            && (self.queue.active_count() == 0 || now >= deadline)
        {
            self.quit = QuitState::Done;
        }
    }

    fn on_tick(&mut self, now: Instant) {
        self.spinner = self.spinner.wrapping_add(1);
        self.check_quit(now);
        // Keep scrolling while a drag rests on the top or bottom row of a pane.
        if let Drag::Dragging { at, .. } = self.drag {
            self.autoscroll(at);
        }
    }

    // ----- mouse ------------------------------------------------------------------

    fn multi_select_held(&self, mods: KeyModifiers) -> bool {
        match self.ui.multi_select_modifier {
            MultiSelectModifier::Ctrl => mods.contains(KeyModifiers::CONTROL),
            MultiSelectModifier::Alt => mods.contains(KeyModifiers::ALT),
        }
    }

    fn on_mouse(&mut self, m: MouseEvent, now: Instant) {
        if self.quit != QuitState::Running {
            return;
        }
        let at = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => self.left_down(at, m.modifiers, now),
            MouseEventKind::Down(MouseButton::Right) => {
                if self.dialog.is_none()
                    && let Target::Item { pane, row } = self.hitmap.hit(m.column, m.row)
                {
                    self.toggle_mark(pane, row);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => self.left_drag(at),
            MouseEventKind::Up(MouseButton::Left) => self.left_up(at),
            MouseEventKind::ScrollDown => self.wheel(at, true),
            MouseEventKind::ScrollUp => self.wheel(at, false),
            _ => {}
        }
    }

    fn left_down(&mut self, at: (u16, u16), mods: KeyModifiers, now: Instant) {
        self.status = None;
        let target = self.hitmap.hit(at.0, at.1);
        let multi = self.multi_select_held(mods);
        let double = !multi
            && self
                .last_click
                .is_some_and(|(pos, t)| pos == at && now.duration_since(t) < DOUBLE_CLICK);
        // Only plain clicks start a double click.
        self.last_click = if double || multi { None } else { Some((at, now)) };

        // A click ends filter typing (the filter stays) and then acts normally.
        if self.dialog == Some(Dialog::Filter) {
            self.dialog = None;
        }
        if let Some(dialog) = self.dialog.take() {
            let fresh =
                self.dialog_since.is_some_and(|t| now.saturating_duration_since(t) < CLICK_GUARD);
            match target {
                Target::Button(_) if fresh => self.dialog = Some(dialog),
                Target::Button(i) => self.press_button(dialog, i, now),
                Target::DialogRow(i) => {
                    if let Dialog::Remotes { filter, .. } = &dialog {
                        if let Some((_, loc)) = self.remote_choices(filter).get(i).cloned() {
                            self.navigate(self.active, loc);
                        } else {
                            self.dialog = Some(dialog);
                        }
                    } else {
                        self.dialog = Some(dialog);
                    }
                }
                // Clicking outside a drop menu or the address being edited dismisses it;
                // other dialogs stay.
                Target::Nothing
                    if matches!(
                        dialog,
                        Dialog::Drop { .. }
                            | Dialog::Help { .. }
                            | Dialog::Errors { .. }
                            | Dialog::Input { purpose: InputPurpose::Goto { .. }, .. }
                    ) => {}
                _ => self.dialog = Some(dialog),
            }
            return;
        }

        match target {
            Target::Item { pane, row } => {
                self.active = pane;
                self.focus = Focus::Panes;
                let opens = self.panes[pane].view.get(row) == Some(&Row::Parent)
                    || self.panes[pane].entry(row).is_some_and(|e| e.is_dir);
                if multi {
                    self.panes[pane].cursor = row;
                    self.toggle_mark(pane, row);
                } else if double && opens {
                    self.panes[pane].cursor = row;
                    self.enter(pane, row);
                } else {
                    let mark = self.panes[pane].entry(row).map(Entry::mark);
                    self.drag = Drag::Pressed { pane, row, mark, at };
                }
            }
            Target::Blank { pane } => {
                self.active = pane;
                self.focus = Focus::Panes;
                self.panes[pane].marks.clear();
            }
            // Like a file manager's address bar: clicking it edits the address.
            Target::Title { pane } => {
                self.active = pane;
                self.focus = Focus::Address;
                self.action(Action::Goto, now);
            }
            Target::QueueRow { index, cancel } => {
                self.focus = Focus::Queue;
                self.queue_cursor = index;
                if cancel && let Some(id) = self.queue.jobs.get(index).map(|j| j.id) {
                    let effects = self.queue.cancel(id);
                    self.run_effects(effects);
                }
            }
            Target::PaneButton { pane, action } => {
                self.active = pane;
                self.focus = Focus::Panes;
                self.action(action, now);
            }
            Target::Queue => self.focus = Focus::Queue,
            Target::Hint(action) => self.action(action, now),
            Target::Button(_) | Target::DialogRow(_) | Target::Dialog | Target::Nothing => {}
        }
    }

    fn toggle_mark(&mut self, pane: PaneId, row: usize) {
        self.active = pane;
        let p = &mut self.panes[pane];
        if let Some(mark) = p.entry(row).map(Entry::mark) {
            toggle(&mut p.marks, mark);
        }
    }

    fn left_drag(&mut self, at: (u16, u16)) {
        match &self.drag {
            Drag::Pressed { pane, mark, at: start, .. } => {
                if *start == at {
                    return;
                }
                let pane = *pane;
                let p = &self.panes[pane];
                let Some(row) = mark.as_ref().and_then(|m| p.row_of(m)) else {
                    self.drag = Drag::Idle;
                    return;
                };
                let items = if p.is_marked(row) {
                    p.selection()
                } else {
                    p.entry(row).cloned().into_iter().collect()
                };
                if items.is_empty() {
                    self.drag = Drag::Idle;
                    return;
                }
                self.drag =
                    Drag::Dragging { src: pane, src_dir: p.loc.clone(), items, at, target: None };
                self.update_drop_target(at);
            }
            Drag::Dragging { .. } => {
                self.autoscroll(at);
                self.update_drop_target(at);
            }
            Drag::Idle => {}
        }
    }

    fn update_drop_target(&mut self, at: (u16, u16)) {
        let target = self.hitmap.hit(at.0, at.1);
        let Drag::Dragging { src_dir, items, .. } = &self.drag else { return };
        let (src_dir, items) = (src_dir.clone(), items.clone());
        let drop = match target {
            Target::Item { pane, row } => {
                let p = &self.panes[pane];
                match p.view.get(row) {
                    Some(Row::Parent) => {
                        p.loc.parent().map(|(parent, _)| (pane, Some(row), parent))
                    }
                    Some(Row::Entry(_)) => match p.entry(row) {
                        Some(e) if e.is_dir => Some((pane, Some(row), p.loc.join(&e.name))),
                        _ => Some((pane, None, p.loc.clone())),
                    },
                    None => Some((pane, None, p.loc.clone())),
                }
            }
            Target::Blank { pane } | Target::Title { pane } | Target::PaneButton { pane, .. } => {
                Some((pane, None, self.panes[pane].loc.clone()))
            }
            _ => None,
        };
        let target = drop.map(|(pane, row, dst)| {
            let plan = Plan {
                kind: OpKind::Copy,
                src_dir: src_dir.clone(),
                items: items.clone(),
                dst_dir: Some(dst.clone()),
            };
            DropTarget { pane, row, dst, valid: plan.invalid_reason().is_none() }
        });
        if let Drag::Dragging { at: a, target: t, .. } = &mut self.drag {
            *a = at;
            *t = target;
        }
    }

    fn autoscroll(&mut self, at: (u16, u16)) {
        let Some(pane) = self.hitmap.pane_at(at.0, at.1) else { return };
        let list = self.hitmap.panes[pane].list;
        if list.height < 3 {
            return;
        }
        let p = &mut self.panes[pane];
        let max = p.view.len().saturating_sub(usize::from(list.height));
        if at.1 == list.y && p.offset > 0 {
            p.offset -= 1;
        } else if at.1 == list.y + list.height - 1 && p.offset < max {
            p.offset += 1;
        } else {
            return;
        }
        self.hitmap.panes[pane].offset = p.offset;
        let height = usize::from(list.height);
        p.cursor =
            p.cursor.clamp(p.offset, p.offset + height - 1).min(p.view.len().saturating_sub(1));
        self.update_drop_target(at);
    }

    fn left_up(&mut self, at: (u16, u16)) {
        match std::mem::replace(&mut self.drag, Drag::Idle) {
            Drag::Pressed { pane, row, mark, .. } => {
                // A plain click: move the cursor there and drop other marks.
                let p = &mut self.panes[pane];
                let row = mark.as_ref().and_then(|m| p.row_of(m)).unwrap_or(row);
                p.cursor = row.min(p.view.len().saturating_sub(1));
                p.marks.clear();
            }
            Drag::Dragging { src_dir, items, target, .. } => {
                let Some(target) = target.filter(|t| t.valid) else { return };
                // A dialog that came up during the drag, such as a conflict, keeps its
                // place: nothing is dropped.
                if self.dialog.is_some() {
                    self.status = Some(tr!("drop-dialog"));
                    return;
                }
                let plan = Plan { kind: OpKind::Copy, src_dir, items, dst_dir: Some(target.dst) };
                match self.ui.drop_action {
                    DropAction::Copy => self.check_conflicts(plan),
                    DropAction::Ask => self.dialog = Some(Dialog::Drop { plan, focus: 0, at }),
                }
            }
            Drag::Idle => {}
        }
    }

    fn wheel(&mut self, at: (u16, u16), down: bool) {
        let choices = match &self.dialog {
            Some(Dialog::Remotes { filter, .. }) => self.remote_choices(filter).len(),
            _ => 0,
        };
        if let Some(dialog) = &mut self.dialog {
            match dialog {
                Dialog::Help { scroll } | Dialog::Errors { scroll } => {
                    *scroll = if down {
                        (*scroll + WHEEL_STEP).min(self.hitmap.scroll_max)
                    } else {
                        scroll.saturating_sub(WHEEL_STEP)
                    };
                }
                Dialog::Remotes { cursor, .. } => {
                    let last = choices.saturating_sub(1);
                    *cursor = if down { (*cursor + 1).min(last) } else { cursor.saturating_sub(1) };
                }
                _ => {}
            }
            return;
        }
        if self.hitmap.queue.contains(ratatui::layout::Position::new(at.0, at.1)) {
            let last = self.queue.jobs.len().saturating_sub(1);
            self.queue_cursor = if down {
                (self.queue_cursor + 1).min(last)
            } else {
                self.queue_cursor.saturating_sub(1)
            };
            return;
        }
        let Some(pane) = self.hitmap.pane_at(at.0, at.1) else { return };
        let height = usize::from(self.hitmap.panes[pane].list.height).max(1);
        let p = &mut self.panes[pane];
        let max = p.view.len().saturating_sub(height);
        p.offset = if down {
            (p.offset + WHEEL_STEP).min(max)
        } else {
            p.offset.saturating_sub(WHEEL_STEP)
        };
        // Scrolling moves the view, not the focus; drag the cursor along so it stays visible.
        p.cursor =
            p.cursor.clamp(p.offset, p.offset + height - 1).min(p.view.len().saturating_sub(1));
        self.hitmap.panes[pane].offset = p.offset;
    }
}

/// Which dialog is shown, which step of a conflict dialog, and how many conflicts it
/// holds: a change means a new dialog appeared under the pointer, or "all" came to mean
/// more than the user saw.
fn dialog_identity(d: Option<&Dialog>) -> Option<(std::mem::Discriminant<Dialog>, usize, usize)> {
    d.map(|d| {
        let (step, conflicts) = match d {
            Dialog::Conflict { conflicts, index, .. } => (*index, conflicts.len()),
            // By job, as the next dialog can follow the last one at once.
            Dialog::JobConflict { conflicts, index, .. } => (
                conflicts.get(*index).map_or(0, |(id, _)| usize::try_from(*id).unwrap_or(0)),
                conflicts.len(),
            ),
            // The password prompt replaces Go to: a second click must not answer it.
            Dialog::Input {
                purpose: InputPurpose::Login(_) | InputPurpose::SshPrompt { .. },
                ..
            } => (1, 0),
            _ => (0, 0),
        };
        (std::mem::discriminant(d), step, conflicts)
    })
}

fn toggle(set: &mut BTreeSet<Mark>, mark: Mark) {
    if !set.remove(&mark) {
        set.insert(mark);
    }
}

fn input(purpose: InputPurpose, text: String) -> Dialog {
    let cursor = text.chars().count();
    Dialog::Input { purpose, text, cursor }
}

/// Line editing in an input dialog. In Go to, the protocol tag comes and goes: text
/// starting with `ftp://` turns that into the tag, Backspace at the start removes the
/// tag and leaves the text, and Ctrl+U clears both.
fn edit_input(purpose: &mut InputPurpose, text: &mut String, cursor: &mut usize, key: KeyEvent) {
    if let InputPurpose::Goto { scheme } = purpose {
        if key.code == KeyCode::Backspace && *cursor == 0 && scheme.is_some() {
            *scheme = None;
            return;
        }
        if key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
            *scheme = None;
        }
        edit_text(text, cursor, key.code, key.modifiers);
        if let Some((s, rest)) = Scheme::split_prefix(text) {
            let typed = text.chars().count() - rest.chars().count();
            *scheme = Some(s);
            *text = rest.to_owned();
            *cursor = cursor.saturating_sub(typed);
        }
    } else {
        edit_text(text, cursor, key.code, key.modifiers);
    }
}

/// Basic line editing on a char-indexed cursor.
fn edit_text(text: &mut String, cursor: &mut usize, code: KeyCode, mods: KeyModifiers) {
    let byte = |t: &String, c: usize| t.char_indices().nth(c).map_or(t.len(), |(i, _)| i);
    let len = text.chars().count();
    match code {
        KeyCode::Left => *cursor = cursor.saturating_sub(1),
        KeyCode::Right => *cursor = (*cursor + 1).min(len),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = len,
        KeyCode::Backspace if *cursor > 0 => {
            let at = byte(text, *cursor - 1);
            text.remove(at);
            *cursor -= 1;
        }
        KeyCode::Delete if *cursor < len => {
            let at = byte(text, *cursor);
            text.remove(at);
        }
        KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
            text.clear();
            *cursor = 0;
        }
        KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
            let at = byte(text, *cursor);
            text.insert(at, c);
            *cursor += 1;
        }
        _ => {}
    }
}

fn check_name(name: &str, pane: &Pane) -> Result<(), String> {
    valid_name(name)?;
    if pane.entries.iter().any(|e| e.name == name) {
        return Err(tr!("name-exists", name = name));
    }
    Ok(())
}

fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(tr!("name-invalid"));
    }
    if name.contains('/') {
        return Err(tr!("name-slash"));
    }
    Ok(())
}

fn is_not_found(e: &RpcError) -> bool {
    e.status == 404 || e.message.contains("not found") || e.message.contains("no such file")
}

/// Adds a hint to errors from remotes that run the system ssh: the terminal belongs to
/// rext, so ssh cannot ask for a password.
pub fn with_hint(message: &str) -> String {
    let m = message.to_lowercase();
    let hint = tr!("ssh-hint");
    if (m.contains("ssh") || m.contains("sftp")) && !message.contains(&hint) {
        format!("{message} ({hint})")
    } else {
        message.to_owned()
    }
}

/// The drop menu's choices.
pub fn drop_buttons() -> Vec<String> {
    vec![tr!("button-copy-here"), tr!("button-move-here"), tr!("button-cancel")]
}

/// `copy huge.bin → nas:backup/2026`
pub fn job_label(job: &crate::jobs::Job) -> String {
    let t = &job.transfer;
    let name = shown(&t.name);
    let name = if t.is_dir { format!("{name}/") } else { name.into_owned() };
    let dir = shown(&t.dst_dir.as_ref().unwrap_or(&t.src_dir).to_string()).into_owned();
    if t.dst_dir.is_some() {
        tr!("job-to", op = t.kind.verb(), name = name, dir = dir)
    } else {
        tr!("job-in", op = t.kind.verb(), name = name, dir = dir)
    }
}

/// Whether a job is in a state where cancel makes sense.
/// Keys that work while the address bar has the focus: switching windows, reloading
/// and the like.
fn works_in_address_bar(action: Action) -> bool {
    matches!(
        action,
        Action::FocusLeftAddress
            | Action::FocusLeft
            | Action::FocusRightAddress
            | Action::FocusRight
            | Action::FocusQueue
            | Action::Queue
            | Action::Goto
            | Action::Remote
            | Action::Refresh
            | Action::ErrorLog
            | Action::OverwriteMode
            | Action::Help
            | Action::Quit
    )
}

pub fn cancellable(state: &JobState) -> bool {
    matches!(state, JobState::Queued | JobState::Checking | JobState::Running)
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
