use clap::{Args, Parser, Subcommand};
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use serde::Serialize;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(
    name = "pj",
    version = VERSION,
    about = "Portable dev bootstrap helper",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Check required local tooling
    Doctor(DoctorArgs),
    /// Bring up local dev stack
    Up,
    /// Configure global git identity
    GitConfig(GitConfigArgs),
    /// Interface to managed dotfiles tasks
    Dot(DotArgs),
    /// Open terminal UI dashboard
    Tui,
}

#[derive(Args, Debug)]
struct DoctorArgs {
    /// Output machine-readable JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct GitConfigArgs {
    /// Git user.name
    #[arg(long)]
    name: Option<String>,
    /// Git user.email
    #[arg(long)]
    email: Option<String>,
}

#[derive(Args, Debug)]
struct DotArgs {
    #[command(subcommand)]
    command: DotCommand,
}

#[derive(Subcommand, Debug)]
enum DotCommand {
    /// Print resolved dotfiles directory
    Where,
    /// Show dotfiles git status (short)
    RepoStatus,
    /// Show dotfiles git diff
    RepoDiff,
    /// Show recent dotfiles commits
    RepoLog,
    /// Pull latest dotfiles changes
    Pull,
    /// Push current dotfiles branch
    Push,
    /// List available task runners and tasks
    Tasks,
    /// Detect stow/chezmoi capabilities and print dotfiles environment info
    Info,
    /// Apply dotfiles with detected manager (chezmoi or stow)
    Apply,
    Install,
    Doctor,
    Up,
    Drift,
    Observe,
    ObserveK8s,
    ObserveLogs,
    Status,
    /// Run dotfiles container status task
    ContainerStatus,
}

#[derive(Serialize, Debug, Clone)]
struct DoctorCheck {
    command: String,
    found: bool,
    location: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
struct DoctorReport {
    checks: Vec<DoctorCheck>,
    missing: Vec<String>,
}

struct App {
    menu_index: usize,
    message: String,
    report: Option<DoctorReport>,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            menu_index: 0,
            message: "Press Enter to run an action. q to quit.".to_string(),
            report: None,
            should_quit: false,
        }
    }
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Some(Commands::Doctor(args)) => run_doctor(args),
        Some(Commands::Up) => run_up(),
        Some(Commands::GitConfig(args)) => run_git_config(args),
        Some(Commands::Dot(args)) => run_dot(args),
        Some(Commands::Tui) | None => run_tui(),
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn collect_doctor_report() -> DoctorReport {
    let required = [
        "git", "gh", "mise", "uv", "bun", "docker", "colima", "kubectl", "k3d",
    ];

    let mut checks = Vec::with_capacity(required.len());
    let mut missing = Vec::new();

    for cmd in required {
        match which(cmd) {
            Some(path) => checks.push(DoctorCheck {
                command: cmd.to_string(),
                found: true,
                location: Some(path),
            }),
            None => {
                missing.push(cmd.to_string());
                checks.push(DoctorCheck {
                    command: cmd.to_string(),
                    found: false,
                    location: None,
                });
            }
        }
    }

    DoctorReport { checks, missing }
}

fn run_doctor(args: DoctorArgs) -> Result<(), String> {
    let report = collect_doctor_report();

    if args.json {
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        println!("{json}");
    } else {
        println!("pj doctor");
        for check in &report.checks {
            if check.found {
                println!(
                    "  [ok] {:<8} -> {}",
                    check.command,
                    check.location.clone().unwrap_or_default()
                );
            } else {
                println!("  [missing] {}", check.command);
            }
        }
    }

    if !report.missing.is_empty() {
        return Err(format!(
            "missing required tools: {}",
            report.missing.join(", ")
        ));
    }
    Ok(())
}

fn run_up() -> Result<(), String> {
    if dotfiles_dir().is_dir() {
        return run_dot_task("up");
    }

    if which("mise").is_some() {
        return run_cmd("mise", &["run", "up"]);
    }

    println!("mise not found, using fallback: colima start + k3d create");
    if which("colima").is_none() {
        return Err("colima not found and mise unavailable".to_string());
    }
    if which("k3d").is_none() {
        return Err("k3d not found and mise unavailable".to_string());
    }

    run_cmd("colima", &["start", "--runtime", "docker"])?;

    let output = Command::new("k3d")
        .args(["cluster", "list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run k3d cluster list: {e}"))?;

    let list = String::from_utf8_lossy(&output.stdout);
    if list.lines().any(|line| line.starts_with("dev ")) {
        println!("k3d cluster 'dev' already exists");
        return Ok(());
    }

    run_cmd("k3d", &["cluster", "create", "dev", "--wait"])
}

fn run_dot(args: DotArgs) -> Result<(), String> {
    ensure_dotfiles_dir()?;
    match args.command {
        DotCommand::Info => run_dot_info(),
        DotCommand::Apply => run_dot_apply(),
        DotCommand::Where => {
            println!("{}", dotfiles_dir().display());
            Ok(())
        }
        DotCommand::RepoStatus => run_cmd_in_dir(&dotfiles_dir(), "git", &["status", "-sb"]),
        DotCommand::RepoDiff => run_cmd_in_dir(&dotfiles_dir(), "git", &["diff"]),
        DotCommand::RepoLog => run_cmd_in_dir(
            &dotfiles_dir(),
            "git",
            &["log", "--oneline", "--decorate", "-n", "20"],
        ),
        DotCommand::Pull => run_cmd_in_dir(&dotfiles_dir(), "git", &["pull", "--ff-only"]),
        DotCommand::Push => run_cmd_in_dir(&dotfiles_dir(), "git", &["push"]),
        DotCommand::Tasks => run_dot_tasks_list(),
        DotCommand::Install => run_dot_task("install"),
        DotCommand::Doctor => run_dot_task("doctor"),
        DotCommand::Up => run_dot_task("up"),
        DotCommand::Drift => run_dot_task("drift"),
        DotCommand::Observe => run_dot_task("observe"),
        DotCommand::ObserveK8s => run_dot_task("observe-k8s"),
        DotCommand::ObserveLogs => run_dot_task("observe-logs"),
        DotCommand::Status => run_dot_info(),
        DotCommand::ContainerStatus => run_dot_task("container-status"),
    }
}

fn run_dot_tasks_list() -> Result<(), String> {
    let dot = dotfiles_dir();
    ensure_dotfiles_dir()?;

    println!("dotfiles dir: {}", dot.display());
    println!();

    if which("mise").is_some() {
        println!("== mise tasks ==");
        run_cmd_in_dir(&dot, "mise", &["tasks", "ls"])?;
        println!();
    }
    if which("just").is_some() {
        println!("== just recipes ==");
        run_cmd_in_dir(&dot, "just", &["--list"])?;
        println!();
    }
    if which("make").is_some() {
        println!("== make targets ==");
        if which("rg").is_some() {
            run_cmd_in_dir(&dot, "rg", &["-n", "^([a-zA-Z0-9_-]+):", "Makefile"])?;
        } else {
            println!("rg not found; skipping make target scan");
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum DotManager {
    Chezmoi,
    Stow,
    Unknown,
}

fn detect_dot_manager(dot: &Path) -> DotManager {
    let has_chezmoi_files = dot.join(".chezmoi.toml").is_file()
        || dot.join(".chezmoi.toml.tmpl").is_file()
        || dot.join(".chezmoiroot").is_file()
        || dot.join(".chezmoiignore").is_file();
    let has_stow_layout = dot.join("config").join("stow-packages.txt").is_file();

    if has_chezmoi_files && which("chezmoi").is_some() {
        return DotManager::Chezmoi;
    }
    if has_stow_layout && which("stow").is_some() {
        return DotManager::Stow;
    }
    if has_chezmoi_files {
        return DotManager::Chezmoi;
    }
    if has_stow_layout {
        return DotManager::Stow;
    }
    DotManager::Unknown
}

fn run_dot_info() -> Result<(), String> {
    let dot = dotfiles_dir();
    ensure_dotfiles_dir()?;

    println!("dotfiles dir: {}", dot.display());
    println!("task runners:");
    println!(
        "  mise: {}",
        if which("mise").is_some() { "yes" } else { "no" }
    );
    println!(
        "  just: {}",
        if which("just").is_some() { "yes" } else { "no" }
    );
    println!(
        "  make: {}",
        if which("make").is_some() { "yes" } else { "no" }
    );
    println!("dotfiles managers:");
    println!(
        "  stow command: {}",
        if which("stow").is_some() { "yes" } else { "no" }
    );
    println!(
        "  chezmoi command: {}",
        if which("chezmoi").is_some() {
            "yes"
        } else {
            "no"
        }
    );

    let manager = detect_dot_manager(&dot);
    println!(
        "detected manager: {}",
        match manager {
            DotManager::Chezmoi => "chezmoi",
            DotManager::Stow => "stow",
            DotManager::Unknown => "unknown",
        }
    );

    run_cmd_in_dir(&dot, "git", &["status", "-sb"])
}

fn run_dot_apply() -> Result<(), String> {
    let dot = dotfiles_dir();
    ensure_dotfiles_dir()?;

    match detect_dot_manager(&dot) {
        DotManager::Chezmoi => {
            if which("chezmoi").is_none() {
                return Err("chezmoi layout detected but `chezmoi` command not found".to_string());
            }
            run_cmd(
                "chezmoi",
                &["apply", "--source", &dot.display().to_string()],
            )
        }
        DotManager::Stow => {
            if which("stow").is_none() {
                return Err("stow layout detected but `stow` command not found".to_string());
            }
            if dot.join("install.sh").is_file() {
                run_cmd_in_dir(&dot, "./install.sh", &[])
            } else {
                Err("stow layout detected but install.sh missing".to_string())
            }
        }
        DotManager::Unknown => {
            Err("no stow/chezmoi layout detected in dotfiles repository".to_string())
        }
    }
}

fn ensure_dotfiles_dir() -> Result<(), String> {
    let dot = dotfiles_dir();
    if dot.is_dir() {
        Ok(())
    } else {
        Err(format!(
            "dotfiles directory not found: {} (set PJ_DOTFILES_DIR)",
            dot.display()
        ))
    }
}

fn dotfiles_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("PJ_DOTFILES_DIR") {
        return PathBuf::from(custom);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join("dotfiles");
    }
    PathBuf::from(".")
}

fn run_dot_task(task: &str) -> Result<(), String> {
    let dot = dotfiles_dir();
    if !dot.is_dir() {
        return Err(format!(
            "dotfiles directory not found: {} (set PJ_DOTFILES_DIR)",
            dot.display()
        ));
    }

    if which("mise").is_some() {
        return run_cmd_in_dir(&dot, "mise", &["run", task]);
    }
    if which("just").is_some() {
        return run_cmd_in_dir(&dot, "just", &[task]);
    }
    if which("make").is_some() {
        return run_cmd_in_dir(&dot, "make", &[task]);
    }

    Err("none of mise/just/make found to run dotfiles tasks".to_string())
}

fn run_git_config(args: GitConfigArgs) -> Result<(), String> {
    let current_name = git_config_get("user.name").unwrap_or_default();
    let current_email = git_config_get("user.email").unwrap_or_default();
    if !current_name.is_empty() && !current_email.is_empty() {
        println!("git identity already set: {current_name} <{current_email}>");
        return Ok(());
    }

    let mut desired_name = args
        .name
        .or_else(|| std::env::var("GIT_USER_NAME").ok())
        .unwrap_or_default();
    let mut desired_email = args
        .email
        .or_else(|| std::env::var("GIT_USER_EMAIL").ok())
        .unwrap_or_default();

    if desired_name.is_empty() {
        desired_name = gh_user_field(".name // .login // \"\"").unwrap_or_default();
    }
    if desired_email.is_empty() {
        desired_email = gh_user_field(".email // \"\"").unwrap_or_default();
        if desired_email.is_empty()
            && let Some(login) = gh_user_field(".login // \"\"")
            && !login.is_empty()
        {
            desired_email = format!("{login}@users.noreply.github.com");
        }
    }

    if current_name.is_empty() && desired_name.is_empty() && atty_stdin() {
        desired_name = prompt("Git user.name")?;
    }
    if current_email.is_empty() && desired_email.is_empty() && atty_stdin() {
        desired_email = prompt("Git user.email")?;
    }

    if current_name.is_empty() && !desired_name.is_empty() {
        run_cmd("git", &["config", "--global", "user.name", &desired_name])?;
    }
    if current_email.is_empty() && !desired_email.is_empty() {
        run_cmd("git", &["config", "--global", "user.email", &desired_email])?;
    }

    let final_name = git_config_get("user.name").unwrap_or_default();
    let final_email = git_config_get("user.email").unwrap_or_default();
    if final_name.is_empty() || final_email.is_empty() {
        return Err(
            "git identity incomplete; set --name/--email or env GIT_USER_NAME/GIT_USER_EMAIL"
                .to_string(),
        );
    }

    println!("git identity set: {final_name} <{final_email}>");
    Ok(())
}

fn run_tui() -> Result<(), String> {
    let mut stdout = io::stdout();
    enable_raw_mode().map_err(|e| e.to_string())?;
    stdout
        .execute(EnterAlternateScreen)
        .map_err(|e| e.to_string())?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend).map_err(|e| e.to_string())?;

    let mut app = App::new();
    let run_result = run_tui_loop(&mut terminal, &mut app);

    disable_raw_mode().map_err(|e| e.to_string())?;
    terminal
        .backend_mut()
        .execute(LeaveAlternateScreen)
        .map_err(|e| e.to_string())?;
    terminal.show_cursor().map_err(|e| e.to_string())?;

    run_result
}

fn run_tui_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<(), String> {
    let menu = ["Doctor", "Dot Up", "Dot Doctor", "Dot Info", "Quit"];

    while !app.should_quit {
        terminal
            .draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Length(7),
                        Constraint::Min(8),
                    ])
                    .split(f.area());

                let title = Paragraph::new(
                    "pj - portable bootstrap dashboard  |  arrows: move  enter: select  q: quit",
                )
                .block(Block::default().borders(Borders::ALL).title("pj"));
                f.render_widget(title, chunks[0]);

                let items: Vec<ListItem> = menu.iter().map(|m| ListItem::new(*m)).collect();
                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title("Actions"))
                    .highlight_style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("➜ ");

                let mut state = ratatui::widgets::ListState::default();
                state.select(Some(app.menu_index));
                f.render_stateful_widget(list, chunks[1], &mut state);

                let mut lines = vec![Line::from(app.message.clone())];
                if let Some(report) = &app.report {
                    lines.push(Line::from(""));
                    lines.push(Line::from("Doctor checks:"));
                    for c in &report.checks {
                        let status = if c.found { "ok" } else { "missing" };
                        let loc = c.location.clone().unwrap_or_default();
                        lines.push(Line::from(format!("- {status:7} {:<8} {loc}", c.command)));
                    }
                    if !report.missing.is_empty() {
                        lines.push(Line::from(""));
                        lines.push(Line::from(format!(
                            "Missing: {}",
                            report.missing.join(", ")
                        )));
                    }
                }

                let details = Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("Status"))
                    .wrap(Wrap { trim: true });
                f.render_widget(details, chunks[2]);
            })
            .map_err(|e| e.to_string())?;

        if event::poll(Duration::from_millis(200)).map_err(|e| e.to_string())?
            && let Event::Key(key) = event::read().map_err(|e| e.to_string())?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Char('q') => app.should_quit = true,
                KeyCode::Up => {
                    if app.menu_index == 0 {
                        app.menu_index = menu.len() - 1;
                    } else {
                        app.menu_index -= 1;
                    }
                }
                KeyCode::Down => {
                    app.menu_index = (app.menu_index + 1) % menu.len();
                }
                KeyCode::Enter => match app.menu_index {
                    0 => {
                        let report = collect_doctor_report();
                        if report.missing.is_empty() {
                            app.message = "Doctor passed. All required tools found.".to_string();
                        } else {
                            app.message = format!(
                                "Doctor found missing tools: {}",
                                report.missing.join(", ")
                            );
                        }
                        app.report = Some(report);
                    }
                    1 => {
                        app.message =
                            "Run `pj dot up` in a normal shell for full output.".to_string();
                    }
                    2 => {
                        app.message = "Run `pj dot doctor` in a normal shell.".to_string();
                    }
                    3 => {
                        app.message = "Run `pj dot info` in a normal shell.".to_string();
                    }
                    4 => app.should_quit = true,
                    _ => {}
                },
                _ => {}
            }
        }
    }

    Ok(())
}

fn run_cmd(bin: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(bin)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to execute {bin}: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed: {} {}", bin, args.join(" ")))
    }
}

fn run_cmd_in_dir(dir: &Path, bin: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(bin)
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to execute {bin} in {}: {e}", dir.display()))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "command failed in {}: {} {}",
            dir.display(),
            bin,
            args.join(" ")
        ))
    }
}

fn which(bin: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if is_executable(&candidate) {
            return Some(candidate.display().to_string());
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = path.metadata() {
            return meta.permissions().mode() & 0o111 != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn git_config_get(key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--global", "--get", key])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn gh_user_field(query: &str) -> Option<String> {
    if which("gh").is_none() {
        return None;
    }
    let auth = Command::new("gh")
        .args(["auth", "status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !auth.success() {
        return None;
    }
    let out = Command::new("gh")
        .args(["api", "user", "-q", query])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn atty_stdin() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal()
}

fn prompt(label: &str) -> Result<String, String> {
    print!("{label}: ");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("failed to read input: {e}"))?;
    Ok(input.trim().to_string())
}
