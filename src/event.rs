//! Everything the UI thread receives, plus the plain data carried by those events.

use chrono::{DateTime, FixedOffset};
use crossterm::event::{KeyEvent, MouseEvent};

use crate::location::Location;
use crate::rclone::RpcError;
use crate::rclone::api::{ListItem, Stats};
use crate::rclone::system_ssh::{SshAnswer, SshQuestion};

/// 0 is the left pane, 1 the right.
pub type PaneId = usize;
pub type ReqId = u64;
pub type JobId = u64;

/// One directory entry as the panes show it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// Bytes; negative when the backend does not know.
    pub size: i64,
    pub mtime: Option<DateTime<FixedOffset>>,
}

impl Entry {
    pub fn file(name: &str, size: i64) -> Self {
        Self { name: name.to_owned(), is_dir: false, size, mtime: None }
    }

    pub fn dir(name: &str) -> Self {
        Self { name: name.to_owned(), is_dir: true, size: -1, mtime: None }
    }

    pub fn mark(&self) -> Mark {
        Mark { is_dir: self.is_dir, name: self.name.clone() }
    }
}

/// A marked entry. The name alone is not enough: object stores such as S3 can hold a
/// file `x` and a directory `x` side by side, and marking the one must not mark the other.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mark {
    pub is_dir: bool,
    pub name: String,
}

impl From<ListItem> for Entry {
    fn from(item: ListItem) -> Self {
        Self {
            mtime: DateTime::parse_from_rfc3339(&item.mod_time).ok(),
            name: item.name,
            is_dir: item.is_dir,
            size: item.size,
        }
    }
}

/// The progress of one rclone job, sent by the job worker after every poll.
#[derive(Debug, Clone, PartialEq)]
pub struct JobSnapshot {
    pub id: JobId,
    /// `None` while running; the error text when the job failed.
    pub finished: Option<Result<(), String>>,
    pub stats: Stats,
}

#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    /// Every 250 ms, for the spinner and timeouts.
    Tick,
    Listed {
        generation: u64,
        result: Result<Vec<Entry>, RpcError>,
    },
    /// A listing was skipped because nobody waits for it any more.
    ListSkipped {
        generation: u64,
    },
    OpDone {
        req: ReqId,
        result: Result<(), RpcError>,
    },
    /// A listing fetched to check for name conflicts: the target of a copy or move, as it
    /// is queued and as it starts, or the directory of a rename.
    Probed {
        req: ReqId,
        result: Result<Vec<Entry>, RpcError>,
    },
    /// The configured remotes with their backend types.
    Remotes(Result<Vec<(String, String)>, RpcError>),
    /// ssh asks something to log in (a password, a one-time code); the answer, or `None`
    /// when the user cancels, goes to `reply`.
    SshPrompt {
        question: SshQuestion,
        reply: crossbeam_channel::Sender<Option<SshAnswer>>,
    },
    /// A password obscured for a connection string.
    Obscured {
        req: ReqId,
        result: Result<String, RpcError>,
    },
    Jobs(Vec<JobSnapshot>),
    /// A job has started, run by `engine`; `resumable` when a stop keeps what it has done
    /// of the file under way.
    JobStarted {
        id: JobId,
        engine: crate::jobs::Engine,
        resumable: bool,
    },
    /// Copies between these two endpoints (by `endpoint_key`, either way) run with
    /// `engine`.
    Engine {
        endpoints: (String, String),
        engine: crate::jobs::Engine,
    },
    /// How a terminal reaches the server a pane shows; `None` when no ssh does.
    Shell {
        req: ReqId,
        result: Result<Option<crate::terminal::ShellTarget>, RpcError>,
    },
    /// Output of the terminal's shell.
    TermOutput {
        id: crate::terminal::TermId,
        bytes: Vec<u8>,
    },
    /// The terminal's shell has ended.
    TermClosed {
        id: crate::terminal::TermId,
    },
    /// Whether the terminal's shell was in the foreground (nothing running in it), when
    /// that could be told.
    TermIdle {
        id: crate::terminal::TermId,
        idle: Option<bool>,
    },
}

/// Short operations run on the browse workers.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowseOp {
    Mkdir(Location),
    /// Rename inside one fs.
    Rename {
        from: Location,
        to: Location,
        is_dir: bool,
    },
    /// Remove a directory if it is empty; a missing directory is fine. Used after a
    /// directory move, because rclone leaves the emptied source root behind.
    RemoveEmptyDir(Location),
}
