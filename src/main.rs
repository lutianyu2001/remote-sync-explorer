//! `rse`: command line, start-up, the event loop and clean shutdown.

use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::builder::StyledStr;
use clap::builder::styling::{Style, Styles};
use clap::error::{ContextKind, ContextValue, ErrorFormatter, ErrorKind};
use clap::{Arg, ArgAction, CommandFactory, FromArgMatches, Parser};
mod licenses;

use crossbeam_channel::{Sender, unbounded};
use crossterm::event::{self as ct, DisableMouseCapture, EnableMouseCapture, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use rse::app::{App, AppInit, QuitState};
use rse::config::{Config, Language, UiConfig};
use rse::endpoint::Address;
use rse::engine::Engines;
use rse::event::Event;
use rse::exec::Exec;
use rse::hitmap::HitMap;
use rse::keymap::Keymap;
use rse::location::{Destination, Location};
use rse::rclone::ssh_passwords::PasswordFile;
use rse::rclone::system_ssh::{self, SystemSsh};
use rse::rclone::{Backend, api};
use rse::tr;
use rse::ui::text::shown;

const TICK: Duration = Duration::from_millis(250);

/// The command line. Its help texts and value names are the `cli-*` messages in
/// `locales/`, filled in by `command`.
#[derive(Parser, Debug)]
#[command(name = "rse", disable_version_flag = true, disable_help_flag = true)]
#[allow(clippy::struct_excessive_bools, reason = "the command line's own flags")]
struct Cli {
    left: Option<String>,
    right: Option<String>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    rclone_config: Option<PathBuf>,
    #[arg(long)]
    ask_password: bool,
    #[arg(long)]
    forget_password: Option<String>,
    #[arg(long)]
    rcd: Option<String>,
    #[arg(long)]
    rc_user: Option<String>,
    #[arg(long)]
    rc_pass: Option<String>,
    #[arg(short = 'T', long)]
    no_terminal: bool,
    #[arg(long)]
    version: bool,
    #[arg(long)]
    licenses: bool,
}

/// `Cli` with its help in the interface language.
fn command() -> clap::Command {
    let styles = Styles::default();
    let header = *styles.get_header();
    let template = format!(
        "{{about-with-newline}}\n{}{{usage}}\n\n{header}{}{header:#}\n{{positionals}}\n\n\
         {header}{}{header:#}\n{{options}}{{after-help}}",
        label(*styles.get_usage(), &tr!("cli-usage-heading")),
        tr!("cli-arguments"),
        tr!("cli-options"),
    );
    Cli::command()
        .about(tr!("cli-about"))
        .override_usage(tr!("cli-usage"))
        .help_template(template)
        .mut_arg("left", |a| a.help(tr!("cli-left")).value_name(tr!("cli-left-value")))
        .mut_arg("right", |a| a.help(tr!("cli-right")).value_name(tr!("cli-right-value")))
        .mut_arg("config", |a| a.help(tr!("cli-config")).value_name(tr!("cli-path-value")))
        .mut_arg("rclone_config", |a| {
            a.help(tr!("cli-rclone-config")).value_name(tr!("cli-path-value"))
        })
        .mut_arg("ask_password", |a| a.help(tr!("cli-ask-password")))
        .mut_arg("forget_password", |a| {
            a.help(tr!("cli-forget-password")).value_name(tr!("cli-login-value"))
        })
        .mut_arg("rcd", |a| a.help(tr!("cli-rcd")).value_name(tr!("cli-url-value")))
        .mut_arg("rc_user", |a| a.help(tr!("cli-rc-user")).value_name(tr!("cli-user-value")))
        .mut_arg("rc_pass", |a| a.help(tr!("cli-rc-pass")).value_name(tr!("cli-pass-value")))
        .mut_arg("no_terminal", |a| a.help(tr!("cli-no-terminal")))
        .mut_arg("version", |a| a.help(tr!("cli-version")))
        .mut_arg("licenses", |a| a.help(tr!("cli-licenses")))
        .arg(Arg::new("help").short('h').long("help").action(ArgAction::Help).help(tr!("cli-help")))
}

/// `text` in `style`, then a space before what follows unless it ends in a
/// full-width colon (`用法：`).
fn label(style: Style, text: &str) -> String {
    let space = if text.ends_with('：') { "" } else { " " };
    format!("{style}{text}{style:#}{space}")
}

/// Parses the command line, or exits with help or an error in the interface language.
fn parse_cli() -> Cli {
    command()
        .try_get_matches()
        .and_then(|m| Cli::from_arg_matches(&m))
        .unwrap_or_else(|e| e.apply::<Localized>().exit())
}

/// clap's errors in the interface language, laid out like clap's own.
struct Localized;

impl ErrorFormatter for Localized {
    fn format_error(error: &clap::error::Error<Self>) -> StyledStr {
        use std::fmt::Write as _;
        let styles = Styles::default();
        let (valid, invalid) = (*styles.get_valid(), *styles.get_invalid());
        let quoted = |style: Style, s: &str| format!("'{style}{s}{style:#}'");
        let text = |kind| match error.get(kind) {
            Some(ContextValue::String(s)) => s.as_str(),
            _ => "",
        };
        let (arg, value) = (text(ContextKind::InvalidArg), text(ContextKind::InvalidValue));
        let message = match error.kind() {
            ErrorKind::UnknownArgument => {
                tr!("cli-unexpected-argument", arg = quoted(invalid, arg))
            }
            ErrorKind::InvalidValue if value.is_empty() => {
                tr!("cli-needs-value", arg = quoted(invalid, arg))
            }
            ErrorKind::InvalidValue => {
                tr!("cli-invalid-value", value = quoted(invalid, value), arg = quoted(invalid, arg))
            }
            ErrorKind::TooManyValues => tr!(
                "cli-unexpected-value",
                value = quoted(invalid, value),
                arg = quoted(invalid, arg)
            ),
            ErrorKind::ArgumentConflict if text(ContextKind::PriorArg) == arg => {
                tr!("cli-repeated", arg = quoted(invalid, arg))
            }
            ErrorKind::InvalidUtf8 => tr!("cli-invalid-utf8"),
            kind => tr!("cli-other", detail = kind.as_str().unwrap_or_default()),
        };
        let mut out = label(*styles.get_error(), &tr!("cli-error")) + &message;
        let mut tips = Vec::new();
        let similar = match error.get(ContextKind::SuggestedArg) {
            Some(ContextValue::String(s)) => vec![s.as_str()],
            Some(ContextValue::Strings(v)) => v.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        };
        if !similar.is_empty() {
            let args: Vec<_> = similar.iter().map(|s| quoted(valid, s)).collect();
            tips.push(tr!(
                "cli-similar",
                count = similar.len(),
                args = args.join(&tr!("list-separator"))
            ));
        }
        // rse has no subcommands, so clap's only other tip is this one.
        if error.get(ContextKind::Suggested).is_some() {
            let escaped = quoted(valid, &format!("-- {arg}"));
            tips.push(tr!("cli-after-dashes", arg = quoted(invalid, arg), escaped = escaped));
        }
        if !tips.is_empty() {
            out.push('\n');
        }
        for tip in tips {
            let _ = write!(out, "\n  {}{tip}", label(valid, &tr!("cli-tip")));
        }
        if error.get(ContextKind::Usage).is_some() {
            let usage = label(*styles.get_usage(), &tr!("cli-usage-heading"));
            let _ = write!(out, "\n\n{usage}{}", tr!("cli-usage"));
        }
        let literal = *styles.get_literal();
        let help = format!("{literal}--help{literal:#}");
        let _ = write!(out, "\n\n{}\n", tr!("cli-try-help", flag = help));
        StyledStr::from(out)
    }
}

/// The language set in the config file, read before the command line is parsed so
/// that `--help` and command line errors use it too.
fn language_before_parsing() -> Language {
    let mut args = std::env::args_os().skip(1);
    let mut path = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        } else if arg == "--config" {
            path = args.next().map(PathBuf::from);
        } else if let Some(p) = arg.to_str().and_then(|a| a.strip_prefix("--config=")) {
            path = Some(PathBuf::from(p));
        }
    }
    // A config file that cannot be read is reported once the command line is parsed.
    path.or_else(Config::default_path)
        .and_then(|p| Config::load(&p).ok())
        .map_or(Language::Auto, |c| c.ui.language)
}

fn main() {
    // ssh runs rse as its SSH_ASKPASS program to ask the running rse a question.
    if let Some(socket) = system_ssh::askpass_socket() {
        std::process::exit(system_ssh::askpass_main(&socket));
    }
    rse::i18n::set(language_before_parsing());
    let cli = parse_cli();
    if cli.licenses {
        if let Err(e) = licenses::print() {
            eprintln!("rse: {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(e) = run(&cli) {
        let causes: Vec<_> = e.chain().map(ToString::to_string).collect();
        eprintln!("rse: {}", shown(&causes.join(&tr!("cause-separator"))));
        std::process::exit(1);
    }
}

#[allow(clippy::unnecessary_wraps, reason = "fails when a backend feature is compiled out")]
fn make_backend(cli: &Cli) -> Result<Arc<dyn Backend>> {
    if let Some(url) = &cli.rcd {
        #[cfg(feature = "rcd")]
        {
            // As `rclone rc` reads them; unlike --rc-pass, others cannot see them in `ps`.
            let env = |name| std::env::var(name).ok();
            let user = cli.rc_user.clone().or_else(|| env("RCLONE_RC_USER"));
            let pass = cli.rc_pass.clone().or_else(|| env("RCLONE_RC_PASS"));
            let http = rse::rclone::Http::new(url, user.as_deref(), pass.as_deref());
            return Ok(Arc::new(http.map_err(anyhow::Error::msg)?));
        }
        #[cfg(not(feature = "rcd"))]
        bail!("{}", tr!("startup-no-rcd", url = url.as_str()));
    }
    #[cfg(feature = "embedded")]
    return Ok(Arc::new(rse::rclone::Embedded::new()));
    #[cfg(not(feature = "embedded"))]
    bail!("{}", tr!("startup-no-embedded"));
}

fn run(cli: &Cli) -> Result<()> {
    let migrated = rse::config::migrate_from_rext();
    let config_path = cli.config.clone().or_else(Config::default_path);
    let config = match &config_path {
        Some(p) => Config::load(p)?,
        None => Config::default(),
    };
    rse::i18n::set(config.ui.language);
    if let Some(login) = &cli.forget_password {
        return forget_password(login);
    }
    let keymap = Keymap::new(&config.keys).context(tr!("startup-keys"))?;

    let mut backend = make_backend(cli)?;
    if cli.version {
        println!("rse {} (remote-sync-explorer)", env!("CARGO_PKG_VERSION"));
        let running = api::version(backend.as_ref())
            .unwrap_or_else(|e| tr!("version-unknown", error = e.to_string()));
        let built = env!("RSE_RCLONE_VERSION");
        let line =
            tr!("version-rclone", running = running, backend = backend.describe(), built = built);
        println!("{}", shown(&line));
        backend.finalize();
        return Ok(());
    }

    // An external rcd runs its own ssh, on its own machine.
    let mut ssh = None;
    if config.rclone.system_ssh && cli.rcd.is_none() {
        let wrapped = system_ssh(backend);
        ssh = Some(wrapped.clone());
        backend = wrapped;
    }
    let remotes = setup_rclone(cli, &config, backend.as_ref());
    let remotes = match remotes {
        Ok(r) => r,
        Err(e) => {
            backend.finalize();
            return Err(e);
        }
    };

    let home = rse::config::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let parse = |s: &str| match Location::parse_address(s, None, &home, &cwd)
        .map_err(anyhow::Error::msg)?
    {
        Destination::Ready(loc) => Ok(loc),
        Destination::Login(address) => log_in(backend.as_ref(), address),
    };
    let (left, right) = initial_locations(cli, &config.ui, &remotes, &home, parse)?;

    let (tx, rx) = unbounded();
    if let Some(ssh) = &ssh {
        ask_in_interface(ssh, tx.clone());
    }
    let mut engines = Engines::new(ssh.clone(), config.jobs.engine, config.jobs.checksum);
    engines.terminal = !cli.no_terminal;
    let exec = Exec::spawn_with(
        backend.clone(),
        tx.clone(),
        config.rclone.browse_threads,
        Duration::from_millis(config.jobs.poll_ms),
        engines,
    );
    let (queue_file, _queue_lock, held) = queue_file();
    let (saved, unsaved) = saved_queue(queue_file.as_deref());
    let init = AppInit {
        ui: config.ui.clone(),
        keymap,
        max_concurrent: config.jobs.max_concurrent,
        left,
        right,
        remotes,
        home,
        cwd,
        saved,
        queue_file: queue_file.clone(),
        terminal: !cli.no_terminal,
    };
    let mut app = App::new(init, exec);
    if let Some(e) = unsaved.or(held) {
        app.log_error(e);
    }
    if let Some(warning) = ssh.as_ref().and_then(|s| s.warning()) {
        app.log_error(warning.to_owned());
        app.status = Some(warning.to_owned());
    }

    // rclone logs to stderr, which would draw over the screen: send it to a file.
    redirect_stderr(log_path().as_deref());
    for (from, to) in &migrated {
        eprintln!("rse: moved {} to {}", from.display(), to.display());
    }
    let result = run_tui(&mut app, &rx, &tx, config.ui.mouse);

    let workers_done = app.into_exec().shutdown_timeout(Duration::from_secs(2));
    // Finalizing while a worker is still inside an rc call is unsafe; the process is
    // about to exit anyway.
    if workers_done {
        backend.finalize();
    }
    restore_stderr();
    if !workers_done {
        eprintln!("rse: {}", tr!("startup-still-listing"));
    }
    result
}

/// Questions from ssh (passwords, codes) are asked in the interface, through `tx`.
fn ask_in_interface(ssh: &SystemSsh, tx: Sender<Event>) {
    ssh.set_prompter(Box::new(move |question| {
        let (reply, answer) = crossbeam_channel::bounded(1);
        tx.send(Event::SshPrompt { question: question.clone(), reply }).ok()?;
        answer.recv().ok().flatten()
    }));
}

/// sftp through the system ssh around `backend`, remembering passwords in the usual file.
fn system_ssh(backend: Arc<dyn Backend>) -> Arc<SystemSsh> {
    let dir = SystemSsh::default_socket_dir();
    let mut ssh = SystemSsh::new(backend, &dir, std::env::current_exe().ok());
    if let Some(path) = PasswordFile::default_path() {
        ssh.set_password_file(PasswordFile::new(path));
    }
    Arc::new(ssh)
}

/// The original stderr while fd 2 points at the log file.
static SAVED_STDERR: Mutex<Option<OwnedFd>> = Mutex::new(None);

/// Points rclone at the right config, unlocks it, applies options, and lists remotes.
fn setup_rclone(
    cli: &Cli,
    config: &Config,
    backend: &dyn Backend,
) -> Result<Vec<(String, String)>> {
    let rclone_config = cli
        .rclone_config
        .clone()
        .or_else(|| (!config.rclone.config.is_empty()).then(|| expand(&config.rclone.config)));
    if let Some(p) = &rclone_config {
        api::set_config_path(backend, &p.to_string_lossy()).context(tr!("startup-config-path"))?;
    }
    // Never let rclone prompt on the terminal: it would draw over the interface.
    if cli.rcd.is_none() {
        api::disable_ask_password(backend).context(tr!("startup-configuring"))?;
    }
    if cli.ask_password {
        let pass = rpassword::prompt_password(format!("{} ", tr!("startup-config-password")))
            .context(tr!("startup-reading-password"))?;
        api::unlock_config(backend, &pass).context(tr!("startup-unlocking"))?;
    }
    api::set_parallelism(backend, config.rclone.transfers, config.rclone.checkers)
        .context(tr!("startup-transfers"))?;
    api::remotes(backend).map_err(|e| {
        let error = e.to_string();
        anyhow::anyhow!(
            "{}",
            if e.message.contains("decrypt") {
                tr!("startup-reading-encrypted-config", error = error)
            } else {
                tr!("startup-reading-config", error = error)
            }
        )
    })
}

/// Where the unfinished jobs are saved, with the lock that keeps them to this rse; no
/// file, and why, while another rse holds it.
fn queue_file() -> (Option<PathBuf>, Option<std::fs::File>, Option<String>) {
    let Some(path) = rse::state::queue_path() else { return (None, None, None) };
    let shown = path.display().to_string();
    match rse::state::lock_queue(&path) {
        Ok(Some(lock)) => (Some(path), Some(lock), None),
        Ok(None) => (None, None, Some(tr!("queue-held", path = shown))),
        Err(e) => (None, None, Some(tr!("queue-not-loaded", path = shown, error = e.to_string()))),
    }
}

/// The jobs left unfinished last time, and why they could not be read when they could
/// not.
fn saved_queue(path: Option<&Path>) -> (Vec<rse::jobs::Transfer>, Option<String>) {
    let Some(path) = path else { return (Vec::new(), None) };
    match rse::state::load_queue(path) {
        Ok(jobs) => (jobs, None),
        Err(e) => {
            let path = path.display().to_string();
            (Vec::new(), Some(tr!("queue-not-loaded", path = path, error = e.to_string())))
        }
    }
}

/// `--forget-password`: deletes a remembered ssh password.
fn forget_password(login: &str) -> Result<()> {
    let Some(path) = PasswordFile::default_path() else { bail!("{}", tr!("passwords-no-home")) };
    let file = PasswordFile::new(path);
    let path = file.path().display().to_string();
    if file.remove(login).with_context(|| tr!("passwords-updating", path = path.as_str()))? {
        println!("{}", tr!("passwords-forgotten", login = login, path = path));
        Ok(())
    } else {
        bail!("{}", tr!("passwords-none-for", login = login, path = path))
    }
}

/// The panes' locations at start: the command line's, else the config's, else the home
/// directory on the left and the first remote on the right. A single remote given on the
/// command line opens on the right, beside the local disk on the left.
fn initial_locations(
    cli: &Cli,
    ui: &UiConfig,
    remotes: &[(String, String)],
    home: &Path,
    parse: impl Fn(&str) -> Result<Location>,
) -> Result<(Location, Location)> {
    let default_left = if ui.left.is_empty() { "~" } else { &ui.left };
    let first = cli.left.as_deref().map(&parse).transpose()?;
    if let Some(loc) = &first
        && cli.right.is_none()
        && !loc.is_local()
    {
        return Ok((parse(default_left)?, loc.clone()));
    }
    let left = match first {
        Some(loc) => loc,
        None => parse(default_left)?,
    };
    let right = match cli.right.as_deref().or(Some(ui.right.as_str()).filter(|s| !s.is_empty())) {
        Some(s) => parse(s)?,
        None => {
            remotes.first().map_or_else(|| Location::local(home), |(r, _)| Location::remote_root(r))
        }
    };
    Ok((left, right))
}

/// Opens a server given on the command line, asking for its password before the
/// interface starts if the address has none.
fn log_in(backend: &dyn Backend, address: Address) -> Result<Location> {
    let Address { mut endpoint, path, password } = address;
    let clear = if let Some(p) = password {
        p
    } else {
        let address = format!("{}://{}", endpoint.scheme.name(), endpoint.authority());
        rpassword::prompt_password(format!("{} ", tr!("login-prompt", address = address)))
            .context(tr!("startup-reading-password"))?
    };
    if !clear.is_empty() {
        endpoint.pass = Some(api::obscure(backend, &clear).context(tr!("startup-obscuring"))?);
    }
    Ok(Location::on(endpoint, &path))
}

fn expand(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), rse::config::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(p),
    }
}

fn log_path() -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt as _;
    let dir = rse::config::cache_dir()?.join("rse");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).ok()?;
    Some(dir.join("rclone.log"))
}

/// The log at `path` to append to: private, since rclone's messages can name servers,
/// accounts and paths, and only a file of this user's, never one a link leads to.
fn log_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != system_ssh::current_uid() {
        return Err(std::io::Error::other("not a file of this user"));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Sends fd 2 to the log at `path` (appending), keeping a copy of the old stderr in
/// `SAVED_STDERR`. Without a usable log, to /dev/null: rclone's messages hold text from
/// servers, which must not reach the terminal unmarked.
fn redirect_stderr(path: Option<&Path>) {
    let file = path.and_then(|p| log_file(p).ok());
    let Some(file) = file.or_else(|| std::fs::File::options().write(true).open("/dev/null").ok())
    else {
        return;
    };
    let Ok(mut slot) = SAVED_STDERR.lock() else { return };
    // Close-on-exec (F_DUPFD_CLOEXEC, in one step): the programs rse starts must not
    // inherit the terminal through it.
    let Ok(saved) = io::stderr().as_fd().try_clone_to_owned() else { return };
    // fd 2 is replaced in one step; on failure the copy is closed as it drops.
    if rustix::stdio::dup2_stderr(&file).is_ok() {
        *slot = Some(saved);
    }
}

/// Points fd 2 back at the original stderr. Safe to call more than once.
fn restore_stderr() {
    let saved = SAVED_STDERR.lock().ok().and_then(|mut slot| slot.take());
    if let Some(saved) = saved {
        let _ = rustix::stdio::dup2_stderr(&saved);
    }
}

fn setup_terminal(mouse: bool) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    if mouse {
        execute!(io::stdout(), EnableMouseCapture)?;
    }
    Ok(())
}

fn restore_terminal() {
    let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), crossterm::cursor::Show);
    let _ = io::stdout().flush();
}

fn run_tui(
    app: &mut App,
    rx: &crossbeam_channel::Receiver<Event>,
    tx: &Sender<Event>,
    mouse: bool,
) -> Result<()> {
    let default_hook = std::panic::take_hook();
    let ui_thread = thread::current().id();
    std::panic::set_hook(Box::new(move |info| {
        // A panic on the UI thread ends rse: give the terminal and stderr back so the
        // message is visible. A worker thread's panic only goes to the log.
        if thread::current().id() == ui_thread {
            restore_terminal();
            restore_stderr();
        }
        default_hook(info);
    }));
    if let Err(e) = setup_terminal(mouse) {
        restore_terminal();
        return Err(e).context(tr!("startup-terminal"));
    }
    let result = event_loop(app, rx, tx);
    restore_terminal();
    let _ = std::panic::take_hook();
    result
}

fn spawn_input(tx: Sender<Event>) {
    thread::spawn(move || {
        loop {
            let event = match ct::read() {
                Ok(ct::Event::Key(k)) if k.kind != KeyEventKind::Release => Event::Key(k),
                Ok(ct::Event::Mouse(m)) => Event::Mouse(m),
                Ok(ct::Event::Resize(w, h)) => Event::Resize(w, h),
                Ok(_) => continue,
                Err(_) => break,
            };
            if tx.send(event).is_err() {
                break;
            }
        }
    });
}

fn spawn_ticker(tx: Sender<Event>) {
    thread::spawn(move || {
        while tx.send(Event::Tick).is_ok() {
            thread::sleep(TICK);
        }
    });
}

fn event_loop(
    app: &mut App,
    rx: &crossbeam_channel::Receiver<Event>,
    tx: &Sender<Event>,
) -> Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    spawn_input(tx.clone());
    spawn_ticker(tx.clone());
    loop {
        let mut map = HitMap::default();
        terminal.draw(|f| map = rse::ui::draw(f, app))?;
        app.set_hitmap(map);

        let Ok(first) = rx.recv() else { bail!("{}", tr!("startup-events-closed")) };
        let now = Instant::now();
        app.handle(first, now);
        // Apply everything that piled up before drawing the next frame.
        while let Ok(ev) = rx.try_recv() {
            app.handle(ev, now);
        }
        if app.quit == QuitState::Done {
            return Ok(());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn start(args: &[&str], remotes: &[&str]) -> (String, String) {
        let cli = Cli::try_parse_from(std::iter::once("rse").chain(args.iter().copied())).unwrap();
        let remotes: Vec<_> = remotes.iter().map(|r| ((*r).to_owned(), String::new())).collect();
        let home = Path::new("/home/me");
        let parse = |s: &str| Location::parse(s, home, home).map_err(anyhow::Error::msg);
        let (left, right) =
            initial_locations(&cli, &UiConfig::default(), &remotes, home, parse).unwrap();
        (left.to_string(), right.to_string())
    }

    #[test]
    fn a_single_remote_opens_on_the_right() {
        assert_eq!(start(&["nas:backup"], &["other"]), ("/home/me".into(), "nas:backup".into()));
    }

    #[test]
    fn a_single_local_path_opens_on_the_left() {
        assert_eq!(start(&["/tmp"], &["nas"]), ("/tmp".into(), "nas:".into()));
    }

    #[test]
    fn two_locations_keep_their_order() {
        assert_eq!(start(&["nas:", "/tmp"], &[]), ("nas:".into(), "/tmp".into()));
    }
}
