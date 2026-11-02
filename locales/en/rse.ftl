# English interface text. Every message here must also be in zh-CN/rse.ftl;
# `cargo test` checks that both files define the same messages and that all are used.

## Common

items = { $count ->
    [one] 1 item
   *[other] { $count } items
}
list-separator = {", "}
cause-separator = {": "}
too-small = terminal too small

## Windows

window-address = Address
window-local = Local
window-remote = Remote
window-transfers = Transfers
tag-local = Local
marked = [{ $count } marked]

## Key hints at the bottom

hint-copy = Copy
hint-move = Move
hint-mkdir = Mkdir
hint-delete = Delete
hint-remote = Remote
hint-filter = Filter
hint-help = Help
hint-quit = Quit

## Overwrite mode: the row across the top while it is on, and the status when it is turned on or off

overwrite-mode = Overwrite mode
overwrite-mode-note = existing files are overwritten without asking; { $key } turns it off
overwrite-on = overwrite mode on: existing files are overwritten without asking
overwrite-off = overwrite mode off: existing files are asked about again

## Address bar

address-revising = [Revising]
address-hint = path, remote:path or ftp://me@box/pub
empty-path = empty path
unknown-protocol = unknown protocol { $name }:// (known: sftp, ftp, ftps, smb, webdav, webdavs)
needs-host = { $scheme }:// needs a host, e.g. { $scheme }://me@box
ipv6-bracket = missing ] after the IPv6 address
bad-port = bad port: { $port }

## Panes

pane-error = error: { $error }
pane-loading = { $spinner } loading…
pane-empty = (empty)
pane-no-match = (nothing matches)
dir-size = <DIR>
sort-name = name
sort-size = size
sort-time = time
sorted-by = sorted by { $key }

## Operations

op-copy = copy
op-move = move
op-delete = delete
op-copy-title = Copy
op-move-title = Move
op-delete-title = Delete
nothing-selected = nothing selected
same-directory = source and target are the same directory
into-itself = cannot put a directory inside itself
checking-target = checking the target…
all-skipped = nothing to do: all items skipped
queued = { $op } { $count ->
    [one] 1 item
   *[other] { $count } items
} queued
probe-late = { $op } not started: the target check finished while another dialog was open
probe-failed = cannot check the target: { $error }
drop-dialog = nothing dropped: a dialog opened during the drag
mkdir-failed = cannot create { $name }: { $error }
rename-failed = cannot rename to { $name }: { $error }
cleanup-failed = moved, but cannot remove the source directory in { $dir }: { $error }
name-invalid = invalid name
name-slash = a name cannot contain /
name-exists = { $name } already exists
password-failed = cannot use the password: { $error }
listing-failed = listing { $dir }: { $error }
names-unsafe = { $dir }: left out { $count ->
    [one] an entry whose name
   *[other] { $count } entries whose names
} could lead out of it, such as ..
listing-remotes-failed = listing remotes: { $error }
ssh-hint = hint: sftp remotes using the ssh option need non-interactive login: a key, ssh-agent or an existing ControlMaster connection

## Transfer queue

queue-idle = idle
queue-running = { $count } running
queue-queued = { $count } queued
queue-done = { $count } done
queue-failed = { $count } failed
queue-keys = x cancel · r retry · c clear · Esc back
job-to = { $op } { $name } → { $dir }
job-in = { $op } { $name } in { $dir }
job-queued = queued
job-checking = { $spinner } checking the target
job-done = done
job-failed = failed: { $error }
job-cancelled = cancelled
job-skipped = skipped
job-exists = { $name } already exists there
job-stopping = stopping…
job-starting = { $spinner } starting
job-lost = lost track of rclone job { $id }: { $error }
job-failed-unknown = job failed

## Dialogs

button-ok = OK
button-cancel = Cancel
button-yes = Yes
button-no = No
button-back = Back
button-overwrite = Overwrite
button-skip = Skip
button-overwrite-all = Overwrite all
button-skip-all = Skip all
button-stop-and-quit = Pause and quit
button-pause-and-leave = Pause and leave
button-stay = Stay
button-copy-here = Copy here
button-move-here = Move here
confirm-title = { $op }?
confirm-from = from  { $dir }
confirm-to = to    { $dir }
confirm-in = in    { $dir }
summary = { $items } ({ $details })
summary-files = { $size } in { $count ->
    [one] 1 file
   *[other] { $count } files
}
summary-dirs = { $count ->
    [one] 1 directory
   *[other] { $count } directories
}
and-more = … and { $count } more
note-merge = Directories are merged into existing ones: identical files are skipped, different files are overwritten.
note-cross-remote-move = Across remotes a move copies, then deletes the source. If cancelled midway, files already copied are already gone from the source.
note-delete-dirs = Directories are deleted including all subdirectories.
note-no-undo = This cannot be undone.
conflict-title = Already exists
conflict-exists = { $name } already exists in { $dir }
conflict-existing = existing  { $entry }
conflict-new = new       { $entry }
conflict-progress = conflict { $index } of { $count }
note-dirs-merged = The directories are merged: identical files are skipped, different files are overwritten.
conflict-job = { $job } did not start:
conflict-job-note = { $count ->
    [1] Cancel leaves the job failed in the queue; retrying it checks the target again.
   *[other] Cancel leaves the { $count } jobs not yet decided failed in the queue; retrying one checks its target again.
}
note-overwrite-mode = Overwrite mode is on: what the target has of these names is overwritten without asking.
entry-dir = directory
entry-modified = { $size }, modified { $time }
quit-title = Quit
quit-note = Quitting pauses the transfers; they go on at the next start. These cannot go on from where they stop, so the file under way starts over:
leave-title = Leave
leave-note = Leaving pauses the transfers between these two places; they go on when you come back. These cannot go on from where they stop, so the file under way starts over:
interrupted-file = { $file }: { $done } of { $size } would be sent again
interrupted-done = { $job }: done, nothing is lost
interrupted-running = { $job }: running
queue-not-saved = The unfinished transfers could not be saved in { $path }: { $error }
queue-not-loaded = The transfers left unfinished last time could not be read from { $path }: { $error }
mkdir-title = New directory
mkdir-prompt = Name of the new directory in { $dir }:
rename-title = Rename
rename-prompt = New name for { $name }:
login-title = Log in
login-prompt = Password for { $address } (empty for none):
ssh-title = SSH login
ssh-remember = Remember password
ssh-remember-key = (Tab)
ssh-notice-title = Why you are asked
remotes-title = Remotes  (type to filter)
remotes-filter = filter: { $filter }
remotes-none = no match
remote-local = Local (~)
errors-title = Error log, newest first  (Esc to close)
errors-none = No errors.

## Help

help-title = Help  (Esc to close)
help-keys = Keys
help-mouse = Mouse
help-shift = Hold Shift while dragging to select text in most terminals.
mouse-click = click
mouse-click-does = move the cursor, clear marks
mouse-blank = click blank space
mouse-blank-does = clear marks
mouse-mark = { $modifier }+click, right click
mouse-mark-does = mark or unmark
mouse-double = double click
mouse-double-does = open directory (.. goes up)
mouse-wheel = wheel
mouse-wheel-does = scroll the pane under the pointer
mouse-address = click the address bar
mouse-address-does = edit the address
mouse-button = click the ↻ / > button
mouse-button-does = reload / go to the edited address
mouse-drag = drag
mouse-drag-does = copy or move (marked entries if you drag a marked one)
mouse-cancel = click ✕ in the queue
mouse-cancel-does = cancel that job

## Command line (`rse --help` and its errors)

cli-about = Dual-pane terminal file manager for local paths and rclone remotes
cli-usage-heading = Usage:
cli-usage = rse [OPTIONS] [LEFT] [RIGHT]
cli-arguments = Arguments:
cli-options = Options:
cli-left = Initial location of the left pane, e.g. ~/work
cli-left-value = LEFT
cli-right = Initial location of the right pane, e.g. nas:backup. Given alone, a remote opens here instead of on the left
cli-right-value = RIGHT
cli-config = remote-sync-explorer config file
cli-rclone-config = rclone config file
cli-path-value = PATH
cli-ask-password = Ask for the rclone config password before starting
cli-forget-password = Forget the ssh password remembered for LOGIN (user@host) and exit
cli-login-value = LOGIN
cli-rcd = Use an external `rclone rcd` instead of the embedded rclone (needs the rcd feature)
cli-url-value = URL
cli-rc-user = User for --rcd
cli-user-value = USER
cli-rc-pass = Password for --rcd (better: the RCLONE_RC_PASS environment variable)
cli-pass-value = PASS
cli-version = Print the remote-sync-explorer and rclone versions
cli-help = Print help
cli-error = error:
cli-tip = tip:
cli-unexpected-argument = unexpected argument { $arg } found
cli-after-dashes = to pass { $arg } as a value, use { $escaped }
cli-similar = { $count ->
    [one] a similar argument exists: { $args }
   *[other] some similar arguments exist: { $args }
}
cli-needs-value = a value is required for { $arg } but none was supplied
cli-invalid-value = invalid value { $value } for { $arg }
cli-unexpected-value = unexpected value { $value } for { $arg } found; no more were expected
cli-repeated = the argument { $arg } cannot be used multiple times
cli-invalid-utf8 = invalid UTF-8 was detected in one or more arguments
cli-other = { $detail }
cli-try-help = For more information, try '{ $flag }'.

## Start-up (`rse --version`, the config files, rclone and ssh)

startup-no-rcd = --rcd { $url }: this rse was built without the `rcd` feature
startup-no-embedded = this rse was built without embedded rclone; use --rcd <url>
startup-keys = in the config file
startup-config-path = setting the rclone config path
startup-configuring = configuring rclone
startup-config-password = rclone config password:
startup-reading-password = reading the password
startup-unlocking = unlocking the rclone config
startup-transfers = setting transfers
startup-reading-config = reading the rclone config: { $error }
startup-reading-encrypted-config = reading the rclone config: { $error } (use --ask-password or set RCLONE_CONFIG_PASS)
startup-obscuring = obscuring the password
startup-terminal = setting up the terminal
startup-events-closed = event channel closed
startup-still-listing = a listing was still running at exit
version-unknown = unknown ({ $error })
version-rclone = rclone { $running } ({ $backend }; built against { $built })
config-in = in { $path }
config-cannot-read = cannot read { $path }
config-cannot-create = cannot create { $path }
config-cannot-write = cannot write { $path }
keys-unknown-key = unknown key `{ $key }`
keys-unknown-action = [keys]: unknown action `{ $name }`
keys-bad-key = [keys] { $name }: { $error }
keys-bound-twice = [keys]: `{ $key }` is bound to both `{ $first }` and `{ $second }`
backend-embedded = embedded librclone
backend-rcd = rclone rcd at { $url }
rcd-unreachable = cannot reach rclone rcd at { $url }: { $error }
rcd-plain-http = refusing { $url }: plain http would send passwords readable over the network; use https, or http to localhost
rcd-bad-reply = reading reply from rcd: { $error }
rclone-shut-down = rclone has been shut down
rclone-bad-call = invalid rc call: { $error }
rclone-bad-json = invalid JSON from rclone: { $error }
rclone-unexpected-reply = unexpected reply from { $method }: { $error }
rclone-no-jobid = { $method } did not return a jobid
ssh-socket-dir = cannot use { $dir } for ssh sockets ({ $error }), so sftp uses rclone's built-in ssh, which checks host keys as ssh is set to
ssh-dir-not-private = exists and is not a directory of this user
ssh-missing = no ssh program: sftp uses rclone's built-in ssh
ssh-cancelled = ssh login cancelled
ssh-timed-out = ssh made no progress logging in for two minutes, apart from waiting for your answers, so the login was given up
ssh-secret-unusable = The { $key } set for { $remote } was not made by rclone obscure (it may be plain text), so it cannot be used.
ssh-password-refused = The server refused the pass set for { $remote }.
ssh-passphrase-wrong = The key_file_pass set for { $remote } does not unlock the key.
passwords-refused = The server refused the remembered password.
passwords-unusable = The remembered password cannot be used: { $error }
passwords-not-saved-before = Last time the password could not be remembered: { $error }
passwords-not-private = { $path } can be read by other users, so rse does not use it; run chmod 600 { $path }
passwords-bad-entry = { $path }: the password for { $login } is not obscured the way rclone obscures it ({ $error })
passwords-not-saved = cannot save the password for { $login } in { $path }: { $error }
passwords-no-home = cannot find the home directory
passwords-updating = updating { $path }
passwords-forgotten = Forgot the password for { $login } (in { $path }).
passwords-none-for = no password is remembered for { $login } in { $path }
ssh-host-key = ssh refused the server's host key ({ $reason }); check it, and fix known_hosts if the change is expected
rsync-stopped = rsync was stopped; the file it was in the middle of is kept, and the next run picks it up
rsync-exit = rsync failed (exit code { $code })
rsync-signal = rsync was ended by signal { $signal }
internal-lock = internal lock poisoned

## Key bindings (the help screen), one per action in the keymap

key-switch_pane = Switch between the panes
key-up = Cursor up
key-down = Cursor down
key-page_up = Page up
key-page_down = Page down
key-home = First entry
key-end = Last entry
key-enter = Open directory
key-parent = Parent directory
key-mark = Mark entry and move down
key-mark_all = Mark all / unmark all
key-filter = Filter by name
key-sort = Sort by name, size, time
key-toggle_hidden = Show or hide dot files
key-goto = Go to a path, remote:path or URL
key-remote = Choose a remote
key-swap = Swap the panes
key-refresh = Reload the pane
key-copy = Copy to the other pane
key-move = Move to the other pane
key-rename = Rename
key-mkdir = New directory
key-delete = Delete
key-overwrite_mode = Overwrite mode on or off: overwrite existing files without asking
key-queue = Enter or leave the transfer queue
key-focus_left_address = Focus the left address bar
key-focus_left = Focus the left pane
key-focus_right_address = Focus the right address bar
key-focus_right = Focus the right pane
key-focus_queue = Focus the transfer queue
key-error_log = Error log (last 100)
key-help = This help
key-quit = Quit
key-queue_cancel = Queue: cancel the job
key-queue_retry = Queue: retry a failed job
key-queue_clear = Queue: clear finished jobs
