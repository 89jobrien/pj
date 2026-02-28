use clap::{Args, Parser, Subcommand};
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use regex::Regex;
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Inspect current project + environment context (no secret values)
    #[command(name = "context", alias = "ctx")]
    Ctx(ContextArgs),
    /// Bring up local dev stack
    Up,
    /// Build and install pj into ~/.local/bin
    InstallLocal(InstallLocalArgs),
    /// Update pj from source and reinstall locally
    Update(UpdateArgs),
    /// Configure global git identity
    GitConfig(GitConfigArgs),
    /// Cache/build artifact inspection and cleanup
    Cache(CacheArgs),
    /// Secret hygiene tools (redaction, scans, git hooks)
    Secret(SecretArgs),
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
struct ContextArgs {
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
struct InstallLocalArgs {
    /// Source directory containing Cargo.toml (defaults to current dir or ~/dev/pj)
    #[arg(long)]
    source: Option<String>,
    /// Install debug build instead of release
    #[arg(long)]
    debug: bool,
}

#[derive(Args, Debug)]
struct UpdateArgs {
    /// Source directory containing Cargo.toml (defaults to current dir or ~/dev/pj)
    #[arg(long)]
    source: Option<String>,
    /// Pull latest git changes before reinstall
    #[arg(long)]
    pull: bool,
    /// Install debug build instead of release
    #[arg(long)]
    debug: bool,
}

#[derive(Args, Debug)]
struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommand,
}

#[derive(Subcommand, Debug)]
enum CacheCommand {
    /// Show detected cache/build directories and sizes
    Status(CacheStatusArgs),
    /// Clean caches (defaults to Rust debug artifacts in current project)
    Clean(CacheCleanArgs),
}

#[derive(Args, Debug)]
struct CacheStatusArgs {
    /// Output machine-readable JSON
    #[arg(long)]
    json: bool,
    /// Include global caches in addition to project caches
    #[arg(long)]
    global: bool,
    /// Detect promotable project binaries from target/{debug,release}
    #[arg(long)]
    binaries: bool,
}

#[derive(Args, Debug)]
struct CacheCleanArgs {
    /// Show what would be removed without deleting
    #[arg(long)]
    dry_run: bool,
    /// Assume yes for confirmation
    #[arg(long)]
    yes: bool,
    /// Also remove non-Rust project caches (node/python)
    #[arg(long)]
    all_project: bool,
    /// Include global caches (~/.cargo/..., ~/.npm/_cacache, etc.)
    #[arg(long)]
    global: bool,
    /// Prompt to install/update detected binaries into ~/.local/bin before cleanup
    #[arg(long)]
    promote_binaries: bool,
}

#[derive(Args, Debug)]
struct SecretArgs {
    #[command(subcommand)]
    command: SecretCommand,
}

#[derive(Subcommand, Debug)]
enum SecretCommand {
    /// Redact sensitive values from input text/stdin
    Redact(SecretRedactArgs),
    /// Scan for potential secrets (staged diff by default)
    Scan(SecretScanArgs),
    /// Install global git hooks to block commits with secrets
    InstallHooks,
}

#[derive(Args, Debug)]
struct SecretRedactArgs {
    /// Inline text to redact; if omitted reads stdin
    text: Option<String>,
}

#[derive(Args, Debug)]
struct SecretScanArgs {
    /// Scan staged git diff
    #[arg(long, default_value_t = true)]
    staged: bool,
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
    /// Detect stow conflicts, prompt to back up conflicting files, and restow
    Adopt(DotAdoptArgs),
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

#[derive(Args, Debug, Clone)]
struct DotAdoptArgs {
    /// Assume yes for all conflict adoption prompts
    #[arg(long)]
    yes: bool,
    /// Print detected conflicts without changing files
    #[arg(long)]
    dry_run: bool,
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

#[derive(Serialize, Debug, Clone)]
struct ContextReport {
    cwd: String,
    project_root: Option<String>,
    project_markers: Vec<String>,
    project_types: Vec<String>,
    config_files: ContextConfigFiles,
    env: ContextEnv,
    git: ContextGit,
}

#[derive(Serialize, Debug, Clone)]
struct ContextConfigFiles {
    global_present: Vec<String>,
    project_present: Vec<String>,
}

#[derive(Serialize, Debug, Clone)]
struct ContextEnv {
    env_files_present: Vec<String>,
    vars_present: Vec<String>,
    vars_missing: Vec<String>,
    secret_vars_present: Vec<String>,
    secret_vars_missing: Vec<String>,
}

#[derive(Serialize, Debug, Clone)]
struct ContextGit {
    global_name: Option<String>,
    global_email: Option<String>,
    local_name: Option<String>,
    local_email: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
struct CacheEntry {
    label: String,
    path: String,
    bytes: u64,
    scope: String,
    kind: String,
}

#[derive(Serialize, Debug, Clone)]
struct BinaryCandidate {
    name: String,
    profile: String,
    path: String,
    bytes: u64,
    installed_path: String,
    installed_exists: bool,
}

struct App {
    menu_index: usize,
    message: String,
    report: Option<DoctorReport>,
    pending_confirm: Option<usize>,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            menu_index: 0,
            message: "Press Enter to run an action. q to quit.".to_string(),
            report: None,
            pending_confirm: None,
            should_quit: false,
        }
    }
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Some(Commands::Doctor(args)) => run_doctor(args),
        Some(Commands::Ctx(args)) => run_context(args),
        Some(Commands::Up) => run_up(),
        Some(Commands::InstallLocal(args)) => run_install_local(args),
        Some(Commands::Update(args)) => run_update(args),
        Some(Commands::GitConfig(args)) => run_git_config(args),
        Some(Commands::Cache(args)) => run_cache(args),
        Some(Commands::Secret(args)) => run_secret(args),
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

fn run_context(args: ContextArgs) -> Result<(), String> {
    let report = collect_context_report()?;
    if args.json {
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        println!("{json}");
        return Ok(());
    }

    println!("pj ctx");
    println!("  cwd: {}", report.cwd);
    println!(
        "  project root: {}",
        report.project_root.as_deref().unwrap_or("(none)")
    );
    if !report.project_types.is_empty() {
        println!("  project types: {}", report.project_types.join(", "));
    }
    if !report.project_markers.is_empty() {
        println!("  markers: {}", report.project_markers.join(", "));
    }
    if !report.config_files.global_present.is_empty() {
        println!(
            "  global config: {}",
            report.config_files.global_present.join(", ")
        );
    }
    if !report.config_files.project_present.is_empty() {
        println!(
            "  project config: {}",
            report.config_files.project_present.join(", ")
        );
    }
    if !report.env.env_files_present.is_empty() {
        println!("  env files: {}", report.env.env_files_present.join(", "));
    }
    println!(
        "  vars present: {}",
        if report.env.vars_present.is_empty() {
            "(none)".to_string()
        } else {
            report.env.vars_present.join(", ")
        }
    );
    println!(
        "  secret vars present: {}",
        if report.env.secret_vars_present.is_empty() {
            "(none)".to_string()
        } else {
            report.env.secret_vars_present.join(", ")
        }
    );
    println!(
        "  git global: {} <{}>",
        report.git.global_name.as_deref().unwrap_or("unset"),
        report.git.global_email.as_deref().unwrap_or("unset")
    );
    println!(
        "  git local: {} <{}>",
        report.git.local_name.as_deref().unwrap_or("unset"),
        report.git.local_email.as_deref().unwrap_or("unset")
    );
    Ok(())
}

fn collect_context_report() -> Result<ContextReport, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("failed to read cwd: {e}"))?;
    let root = detect_project_root(&cwd);

    let marker_names = [
        ".git",
        ".mise.toml",
        "mise.toml",
        "Cargo.toml",
        "go.mod",
        "pyproject.toml",
        "package.json",
        "bun.lock",
        "bun.lockb",
        "deno.json",
        "justfile",
        "Justfile",
        "Makefile",
        "Tiltfile",
        "docker-compose.yml",
        "docker-compose.yaml",
        "config/stow-packages.txt",
        ".chezmoi.toml",
    ];

    let mut project_markers = Vec::new();
    let mut project_types = Vec::new();
    let mut project_present = Vec::new();
    let mut env_files_present = Vec::new();

    if let Some(project_root) = &root {
        for rel in marker_names {
            if project_root.join(rel).exists() {
                project_markers.push(rel.to_string());
                project_present.push(rel.to_string());
            }
        }

        if project_root.join("Cargo.toml").is_file() {
            project_types.push("rust".to_string());
        }
        if project_root.join("go.mod").is_file() {
            project_types.push("go".to_string());
        }
        if project_root.join("pyproject.toml").is_file()
            || project_root.join("requirements.txt").is_file()
        {
            project_types.push("python".to_string());
        }
        if project_root.join("package.json").is_file()
            || project_root.join("bun.lock").is_file()
            || project_root.join("bun.lockb").is_file()
        {
            project_types.push("node/bun".to_string());
        }
        if project_root.join("Tiltfile").is_file()
            || project_root.join("docker-compose.yml").is_file()
            || project_root.join("docker-compose.yaml").is_file()
            || project_root.join("k8s").is_dir()
            || project_root.join("helm").is_dir()
        {
            project_types.push("containers/k8s".to_string());
        }
        if project_root
            .join("config")
            .join("stow-packages.txt")
            .is_file()
            || project_root.join(".chezmoi.toml").is_file()
        {
            project_types.push("dotfiles".to_string());
        }

        let env_files = [
            ".env",
            ".env.local",
            ".envrc",
            "mise.local.toml",
            "secrets/.env.json",
            "secrets/.env.sops.json",
            "secrets/bootstrap.env.sops",
        ];
        for rel in env_files {
            if project_root.join(rel).exists() {
                env_files_present.push(rel.to_string());
            }
        }
    }

    let global_candidates = vec![
        expand_home("~/.gitconfig"),
        expand_home("~/.config/mise/config.toml"),
        expand_home("~/.config/mise/mise.toml"),
        expand_home("~/.zshrc"),
        expand_home("~/.zshrc.local"),
        expand_home("~/.config/fish/config.fish"),
        expand_home("~/.config/dev-bootstrap/secrets.env"),
    ];
    let mut global_present = Vec::new();
    for (label, path) in [
        ("~/.gitconfig", &global_candidates[0]),
        ("~/.config/mise/config.toml", &global_candidates[1]),
        ("~/.config/mise/mise.toml", &global_candidates[2]),
        ("~/.zshrc", &global_candidates[3]),
        ("~/.zshrc.local", &global_candidates[4]),
        ("~/.config/fish/config.fish", &global_candidates[5]),
        ("~/.config/dev-bootstrap/secrets.env", &global_candidates[6]),
    ] {
        if path.exists() {
            global_present.push(label.to_string());
        }
    }

    let vars = [
        "PJ_DOTFILES_DIR",
        "MISE_SOPS_AGE_KEY_FILE",
        "XDG_CONFIG_HOME",
        "SHELL",
        "EDITOR",
    ];
    let secret_vars = [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "DATABASE_URL",
        "REDIS_URL",
    ];

    let mut vars_present = Vec::new();
    let mut vars_missing = Vec::new();
    for key in vars {
        if std::env::var_os(key).is_some() {
            vars_present.push(key.to_string());
        } else {
            vars_missing.push(key.to_string());
        }
    }

    let mut secret_vars_present = Vec::new();
    let mut secret_vars_missing = Vec::new();
    for key in secret_vars {
        if std::env::var_os(key).is_some() {
            secret_vars_present.push(key.to_string());
        } else {
            secret_vars_missing.push(key.to_string());
        }
    }

    let git = ContextGit {
        global_name: git_config_get("user.name"),
        global_email: git_config_get("user.email"),
        local_name: git_config_local_get(root.as_deref(), "user.name"),
        local_email: git_config_local_get(root.as_deref(), "user.email"),
    };

    Ok(ContextReport {
        cwd: cwd.display().to_string(),
        project_root: root.as_ref().map(|p| p.display().to_string()),
        project_markers,
        project_types,
        config_files: ContextConfigFiles {
            global_present,
            project_present,
        },
        env: ContextEnv {
            env_files_present,
            vars_present,
            vars_missing,
            secret_vars_present,
            secret_vars_missing,
        },
        git,
    })
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

fn run_install_local(args: InstallLocalArgs) -> Result<(), String> {
    if which("cargo").is_none() {
        return Err("cargo not found".to_string());
    }

    let cwd = std::env::current_dir().map_err(|e| format!("failed to read cwd: {e}"))?;
    let source = if let Some(s) = args.source {
        PathBuf::from(s)
    } else if cwd.join("Cargo.toml").is_file() {
        cwd
    } else if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join("dev/pj");
        if p.join("Cargo.toml").is_file() {
            p
        } else {
            return Err("no Cargo.toml in cwd and ~/dev/pj not found; use --source".to_string());
        }
    } else {
        return Err("unable to resolve source path; use --source".to_string());
    };

    let root = if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local")
    } else {
        PathBuf::from(".local")
    };

    let mut args_vec = vec![
        "install".to_string(),
        "--path".to_string(),
        source.display().to_string(),
        "--root".to_string(),
        root.display().to_string(),
        "--force".to_string(),
    ];
    if args.debug {
        args_vec.push("--debug".to_string());
    }
    let refs: Vec<&str> = args_vec.iter().map(|s| s.as_str()).collect();
    run_cmd("cargo", &refs)?;
    println!("installed pj to {}", root.join("bin/pj").display());

    // Also keep ~/.cargo/bin/pj in sync when it exists or shadows ~/.local/bin in PATH.
    let home = std::env::var("HOME").unwrap_or_default();
    let cargo_pj = PathBuf::from(&home).join(".cargo/bin/pj");
    let local_pj = root.join("bin/pj");
    let should_sync_cargo = cargo_pj.exists()
        || path_precedence_index(&cargo_pj) < path_precedence_index(&local_pj)
        || which("pj")
            .map(|p| {
                p.starts_with(
                    &PathBuf::from(&home)
                        .join(".cargo/bin")
                        .display()
                        .to_string(),
                )
            })
            .unwrap_or(false);
    if should_sync_cargo {
        let mut sync_args = vec![
            "install".to_string(),
            "--path".to_string(),
            source.display().to_string(),
            "--force".to_string(),
        ];
        if args.debug {
            sync_args.push("--debug".to_string());
        }
        let sync_refs: Vec<&str> = sync_args.iter().map(|s| s.as_str()).collect();
        run_cmd("cargo", &sync_refs)?;
        println!("synced pj to {}", cargo_pj.display());
    }
    Ok(())
}

fn run_update(args: UpdateArgs) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("failed to read cwd: {e}"))?;
    let source = if let Some(s) = args.source.clone() {
        PathBuf::from(s)
    } else if cwd.join("Cargo.toml").is_file() {
        cwd
    } else if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join("dev/pj");
        if p.join("Cargo.toml").is_file() {
            p
        } else {
            return Err("no Cargo.toml in cwd and ~/dev/pj not found; use --source".to_string());
        }
    } else {
        return Err("unable to resolve source path; use --source".to_string());
    };

    if args.pull {
        if !source.join(".git").exists() {
            return Err(format!(
                "--pull requested but {} is not a git repo",
                source.display()
            ));
        }
        run_cmd_in_dir(&source, "git", &["pull", "--ff-only"])?;
    }

    run_install_local(InstallLocalArgs {
        source: Some(source.display().to_string()),
        debug: args.debug,
    })
}

fn path_precedence_index(bin_path: &Path) -> usize {
    let Some(parent) = bin_path.parent() else {
        return usize::MAX;
    };
    let Some(path_os) = std::env::var_os("PATH") else {
        return usize::MAX;
    };
    for (idx, dir) in std::env::split_paths(&path_os).enumerate() {
        if dir == parent {
            return idx;
        }
    }
    usize::MAX
}

fn run_cache(args: CacheArgs) -> Result<(), String> {
    match args.command {
        CacheCommand::Status(args) => run_cache_status(args),
        CacheCommand::Clean(args) => run_cache_clean(args),
    }
}

fn run_cache_status(args: CacheStatusArgs) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("failed to read cwd: {e}"))?;
    let root = detect_project_root(&cwd).unwrap_or(cwd);
    let mut entries = detect_cache_entries(&root, args.global);
    entries.retain(|e| Path::new(&e.path).exists());
    entries.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    let binaries = if args.binaries {
        detect_promotable_binaries(&root)
    } else {
        Vec::new()
    };

    if args.json {
        let payload = serde_json::json!({
            "project_root": root.display().to_string(),
            "caches": entries,
            "binaries": binaries,
        });
        let json = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
        println!("{json}");
        return Ok(());
    }

    println!("pj cache status");
    println!("  project root: {}", root.display());
    if entries.is_empty() {
        println!("  no known cache/build directories found");
        return Ok(());
    }

    let total: u64 = entries.iter().map(|e| e.bytes).sum();
    println!("  total: {}", human_size(total));
    for e in entries {
        println!(
            "  - {:<18} {:>8}  [{}:{}] {}",
            e.label,
            human_size(e.bytes),
            e.scope,
            e.kind,
            e.path
        );
    }
    if args.binaries {
        if binaries.is_empty() {
            println!("  promotable binaries: none detected");
        } else {
            println!("  promotable binaries:");
            for b in binaries {
                println!(
                    "  - {:<12} {:>8}  [{}] {} -> {}{}",
                    b.name,
                    human_size(b.bytes),
                    b.profile,
                    b.path,
                    b.installed_path,
                    if b.installed_exists { " (exists)" } else { "" }
                );
            }
        }
    }
    Ok(())
}

fn run_cache_clean(args: CacheCleanArgs) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("failed to read cwd: {e}"))?;
    let root = detect_project_root(&cwd).unwrap_or(cwd);
    let all = detect_cache_entries(&root, args.global);

    let mut selected = Vec::new();
    for e in all {
        let is_rust_debug = e.kind == "rust-debug";
        let is_project_other = e.scope == "project" && e.kind != "rust-debug";
        let is_global = e.scope == "global";
        if is_rust_debug || (args.all_project && is_project_other) || (args.global && is_global) {
            selected.push(e);
        }
    }

    selected.retain(|e| Path::new(&e.path).exists());
    selected.sort_by(|a, b| b.bytes.cmp(&a.bytes));

    if selected.is_empty() {
        println!("nothing to clean");
        return Ok(());
    }

    let reclaim: u64 = selected.iter().map(|e| e.bytes).sum();
    println!("pj cache clean");
    println!("  project root: {}", root.display());
    println!(
        "  reclaimable: {} across {} path(s)",
        human_size(reclaim),
        selected.len()
    );
    for e in &selected {
        println!("  - {:<18} {:>8} {}", e.label, human_size(e.bytes), e.path);
    }

    if args.promote_binaries {
        maybe_promote_binaries(&root, args.yes, args.dry_run)?;
    }

    if args.dry_run {
        println!("dry-run enabled; no files removed");
        return Ok(());
    }

    if !args.yes && atty_stdin() {
        let answer = prompt("Proceed with cache cleanup? [y/N]")?;
        if !matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("aborted");
            return Ok(());
        }
    }

    let mut removed = 0usize;
    let mut failed = Vec::new();
    for e in selected {
        let p = PathBuf::from(&e.path);
        let meta = match fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(err) => {
                failed.push(format!("{} ({err})", p.display()));
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            failed.push(format!("{} (symlink skipped)", p.display()));
            continue;
        }
        let res = if meta.is_dir() {
            fs::remove_dir_all(&p)
        } else {
            fs::remove_file(&p)
        };
        match res {
            Ok(_) => removed += 1,
            Err(err) => failed.push(format!("{} ({err})", p.display())),
        }
    }

    println!("removed {} path(s)", removed);
    if !failed.is_empty() {
        for f in failed {
            eprintln!("failed: {f}");
        }
        return Err("some cache paths could not be removed".to_string());
    }
    Ok(())
}

fn detect_cache_entries(root: &Path, include_global: bool) -> Vec<CacheEntry> {
    let mut out = Vec::new();
    let candidates_project: [(&str, &str, &str); 10] = [
        ("Rust target debug", "target/debug", "rust-debug"),
        ("Rust incremental", "target/incremental", "rust-debug"),
        ("Rust fingerprint", "target/.fingerprint", "rust-debug"),
        ("Node cache", "node_modules/.cache", "project-cache"),
        ("Next cache", ".next/cache", "project-cache"),
        ("Turborepo cache", ".turbo", "project-cache"),
        ("Pytest cache", ".pytest_cache", "project-cache"),
        ("Mypy cache", ".mypy_cache", "project-cache"),
        ("Ruff cache", ".ruff_cache", "project-cache"),
        ("Project .cache", ".cache", "project-cache"),
    ];

    for (label, rel, kind) in candidates_project {
        let p = root.join(rel);
        if p.exists() {
            out.push(CacheEntry {
                label: label.to_string(),
                path: p.display().to_string(),
                bytes: dir_size_or_zero(&p),
                scope: "project".to_string(),
                kind: kind.to_string(),
            });
        }
    }

    if include_global {
        for (label, path) in global_cache_candidates() {
            if path.exists() {
                out.push(CacheEntry {
                    label,
                    path: path.display().to_string(),
                    bytes: dir_size_or_zero(&path),
                    scope: "global".to_string(),
                    kind: "global-cache".to_string(),
                });
            }
        }
    }

    out
}

fn global_cache_candidates() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => return out,
    };
    let pairs = vec![
        ("Cargo registry cache", home.join(".cargo/registry/cache")),
        ("Cargo git db", home.join(".cargo/git/db")),
        ("Cargo git checkouts", home.join(".cargo/git/checkouts")),
        ("NPM cache", home.join(".npm/_cacache")),
        ("Bun cache", home.join(".bun/install/cache")),
        ("Pip cache", home.join(".cache/pip")),
        ("Home cache", home.join(".cache")),
        ("Go build cache", home.join("Library/Caches/go-build")),
        ("Pip macOS cache", home.join("Library/Caches/pip")),
    ];
    for (label, path) in pairs {
        out.push((label.to_string(), path));
    }
    out
}

fn dir_size_or_zero(path: &Path) -> u64 {
    dir_size(path).unwrap_or(0)
}

fn dir_size(path: &Path) -> Result<u64, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        return Ok(0);
    }
    if meta.is_file() {
        return Ok(meta.len());
    }
    if !meta.is_dir() {
        return Ok(0);
    }
    let mut size = 0u64;
    let entries = fs::read_dir(path).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let child = entry.path();
        size = size.saturating_add(dir_size(&child)?);
    }
    Ok(size)
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut val = bytes as f64;
    let mut idx = 0usize;
    while val >= 1024.0 && idx < UNITS.len() - 1 {
        val /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", bytes, UNITS[idx])
    } else {
        format!("{val:.1} {}", UNITS[idx])
    }
}

fn detect_promotable_binaries(root: &Path) -> Vec<BinaryCandidate> {
    let mut names = Vec::new();
    if let Some(name) = cargo_package_name(root) {
        names.push(name);
    }
    let src_bin = root.join("src/bin");
    if src_bin.is_dir()
        && let Ok(entries) = fs::read_dir(src_bin)
    {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("rs")
                && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
            {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    names.dedup();

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let local_bin = PathBuf::from(home).join(".local/bin");
    let mut out = Vec::new();
    for name in names {
        for profile in ["release", "debug"] {
            let p = root.join("target").join(profile).join(&name);
            if !is_executable(&p) {
                continue;
            }
            let bytes = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            let installed = local_bin.join(&name);
            out.push(BinaryCandidate {
                name: name.clone(),
                profile: profile.to_string(),
                path: p.display().to_string(),
                bytes,
                installed_path: installed.display().to_string(),
                installed_exists: installed.exists(),
            });
        }
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    out
}

fn maybe_promote_binaries(root: &Path, yes: bool, dry_run: bool) -> Result<(), String> {
    let bins = detect_promotable_binaries(root);
    if bins.is_empty() {
        return Ok(());
    }
    println!("  detected promotable binaries:");
    for b in &bins {
        println!(
            "  - {} [{}] {} -> {}",
            b.name, b.profile, b.path, b.installed_path
        );
    }
    if dry_run {
        println!("  dry-run: skipped binary promotion");
        return Ok(());
    }
    let do_install = if yes {
        true
    } else if atty_stdin() {
        let answer = prompt("Install/update binary into ~/.local/bin before cleanup? [y/N]")?;
        matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes")
    } else {
        false
    };
    if !do_install {
        return Ok(());
    }
    run_install_local(InstallLocalArgs {
        source: Some(root.display().to_string()),
        debug: false,
    })
}

fn cargo_package_name(root: &Path) -> Option<String> {
    let cargo_toml = root.join("Cargo.toml");
    let content = fs::read_to_string(cargo_toml).ok()?;
    let mut in_package = false;
    for line in content.lines() {
        let l = line.trim();
        if l.starts_with('[') && l.ends_with(']') {
            in_package = l == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = l.strip_prefix("name")
            && let Some(eq_idx) = rest.find('=')
        {
            let value = rest[(eq_idx + 1)..].trim().trim_matches('"');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[derive(Clone)]
struct SecretPattern {
    re: Regex,
    label: &'static str,
}

fn secret_patterns() -> &'static Vec<SecretPattern> {
    static PATTERNS: OnceLock<Vec<SecretPattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let defs: [(&str, &str); 15] = [
            (r"\b(?:A3T[A-Z0-9]|AKIA|ABIA|ACCA|AGPA|AIDA|AIPA|ANPA|ANVA|APKA|AROA|ASCA|ASIA)[A-Z0-9]{16}\b", "AWS-KEY"),
            (r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,}\b", "GITHUB-TOKEN"),
            (r"\bgithub_pat_[A-Za-z0-9]{22}_[A-Za-z0-9]{59}\b", "GITHUB-TOKEN"),
            (r"\bglpat-[A-Za-z0-9_-]{20,}\b", "GITLAB-TOKEN"),
            (r"\b(?:sk|rk)_(?:test|live)_[A-Za-z0-9]{24,}\b", "STRIPE-SECRET"),
            (r"\bAIza[0-9A-Za-z_-]{35}\b", "GOOGLE-API"),
            (r"\bxox[baprs]-[A-Za-z0-9-]{20,}\b", "SLACK-TOKEN"),
            (r"https://hooks\.slack\.com/services/T[A-Z0-9]{8,}/B[A-Z0-9]{8,}/[A-Za-z0-9]{24}", "SLACK-WEBHOOK"),
            (r"postgres(?:ql)?://[^:\s]+:[^@\s]+@[^/\s]+/\w+", "DB-POSTGRES"),
            (r"mysql://[^:\s]+:[^@\s]+@[^/\s]+/\w+", "DB-MYSQL"),
            (r"mongodb(?:\+srv)?://[^:\s]+:[^@\s]+@[^/\s]+", "DB-MONGODB"),
            (r"redis://[^:\s]+:[^@\s]+@[^/\s]+", "DB-REDIS"),
            (r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY(?: BLOCK)?-----", "PRIVATE-KEY"),
            (
                r#"\b(?:OPENAI_API_KEY|ANTHROPIC_API_KEY|AWS_SECRET_ACCESS_KEY|GITHUB_TOKEN|GH_TOKEN)\s*=\s*['"]?[^\s'"`]+"#,
                "ENV-SECRET",
            ),
            (r#"(?:password|passwd|pwd|secret_key|auth_key|private_key|encryption_key|token|api[_-]?key)\s*[=:]\s*["']?[^\s"']{8,}["']?"#, "SECRET"),
        ];

        defs.iter()
            .map(|(pat, label)| SecretPattern {
                re: Regex::new(pat).expect("valid secret regex"),
                label,
            })
            .collect()
    })
}

fn redact_text(input: &str) -> String {
    let mut out = input.to_string();
    for p in secret_patterns() {
        let token = format!("[REDACTED-{}]", p.label);
        out = p.re.replace_all(&out, token.as_str()).into_owned();
    }
    out
}

fn scan_text_for_secrets(input: &str) -> Vec<String> {
    let mut findings = Vec::new();
    for p in secret_patterns() {
        if p.re.is_match(input) {
            findings.push(p.label.to_string());
        }
    }
    findings.sort();
    findings.dedup();
    findings
}

fn run_secret(args: SecretArgs) -> Result<(), String> {
    match args.command {
        SecretCommand::Redact(args) => run_secret_redact(args),
        SecretCommand::Scan(args) => run_secret_scan(args),
        SecretCommand::InstallHooks => run_secret_install_hooks(),
    }
}

fn run_secret_redact(args: SecretRedactArgs) -> Result<(), String> {
    let input = if let Some(text) = args.text {
        text
    } else {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("failed to read stdin: {e}"))?;
        buf
    };
    print!("{}", redact_text(&input));
    Ok(())
}

fn run_secret_scan(args: SecretScanArgs) -> Result<(), String> {
    let source = if args.staged {
        let out = Command::new("git")
            .args(["diff", "--cached", "--text", "--unified=0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to run git diff --cached: {e}"))?;
        if !out.status.success() {
            return Err("git staged diff scan failed; ensure you are in a git repo".to_string());
        }
        String::from_utf8_lossy(&out.stdout).to_string()
    } else {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("failed to read stdin: {e}"))?;
        buf
    };

    let findings = scan_text_for_secrets(&source);
    if findings.is_empty() {
        println!("secret scan passed");
        return Ok(());
    }

    eprintln!("potential secrets detected: {}", findings.join(", "));
    let preview = redact_text(&source);
    for line in preview.lines().take(20) {
        if line.starts_with('+') || line.starts_with('-') {
            eprintln!("{line}");
        }
    }
    Err("secret scan failed".to_string())
}

fn run_secret_install_hooks() -> Result<(), String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let hooks_dir = PathBuf::from(home).join(".config/git/hooks");
    fs::create_dir_all(&hooks_dir)
        .map_err(|e| format!("failed to create hooks dir {}: {e}", hooks_dir.display()))?;

    let pre_commit = hooks_dir.join("pre-commit");
    let pre_push = hooks_dir.join("pre-push");
    let script = r#"#!/usr/bin/env bash
set -euo pipefail
if ! command -v pj >/dev/null 2>&1; then
  echo "[pj-secret] pj not found; skipping secret scan."
  exit 0
fi
pj secret scan --staged
"#;
    fs::write(&pre_commit, script)
        .map_err(|e| format!("failed to write {}: {e}", pre_commit.display()))?;
    fs::write(&pre_push, script)
        .map_err(|e| format!("failed to write {}: {e}", pre_push.display()))?;
    set_executable(&pre_commit)?;
    set_executable(&pre_push)?;

    run_cmd(
        "git",
        &[
            "config",
            "--global",
            "core.hooksPath",
            &hooks_dir.display().to_string(),
        ],
    )?;

    println!("installed git hooks in {}", hooks_dir.display());
    Ok(())
}

fn run_dot(args: DotArgs) -> Result<(), String> {
    ensure_dotfiles_dir()?;
    match args.command {
        DotCommand::Info => run_dot_info(),
        DotCommand::Apply => run_dot_apply(),
        DotCommand::Adopt(adopt_args) => run_dot_adopt(adopt_args),
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
        DotCommand::Install => run_dot_install(),
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
                run_cmd_in_dir_with_env(&dot, "./install.sh", &[], &[("PJ_DOTFILES_RUNNER", "pj")])
            } else {
                Err("stow layout detected but install.sh missing".to_string())
            }
        }
        DotManager::Unknown => {
            Err("no stow/chezmoi layout detected in dotfiles repository".to_string())
        }
    }
}

#[derive(Debug, Clone)]
struct StowConflict {
    package: String,
    target_rel: PathBuf,
    reason: String,
}

fn run_dot_install() -> Result<(), String> {
    let dot = dotfiles_dir();
    ensure_dotfiles_dir()?;

    if matches!(detect_dot_manager(&dot), DotManager::Stow) {
        run_dot_adopt(DotAdoptArgs {
            yes: false,
            dry_run: false,
        })?;
    }

    run_dot_task_with_env("install", &[("PJ_DOTFILES_RUNNER", "pj")])
}

fn run_dot_adopt(args: DotAdoptArgs) -> Result<(), String> {
    let dot = dotfiles_dir();
    ensure_dotfiles_dir()?;

    if !matches!(detect_dot_manager(&dot), DotManager::Stow) {
        println!("stow layout not detected; nothing to adopt.");
        return Ok(());
    }
    if which("stow").is_none() {
        return Err("stow layout detected but `stow` command not found".to_string());
    }

    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME not set".to_string())?);
    let packages = stow_packages(&dot)?;
    if packages.is_empty() {
        println!("no stow packages found.");
        return Ok(());
    }

    let mut all_conflicts = Vec::new();
    for package in packages {
        let mut conflicts = stow_conflicts_for_package(&dot, &home, &package)?;
        all_conflicts.append(&mut conflicts);
    }

    if all_conflicts.is_empty() {
        println!("no stow conflicts detected.");
        return Ok(());
    }

    println!("detected {} stow conflict(s):", all_conflicts.len());
    for conflict in &all_conflicts {
        println!(
            "  - [{}] {} ({})",
            conflict.package,
            conflict.target_rel.display(),
            conflict.reason
        );
    }
    if args.dry_run {
        return Ok(());
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs()
        .to_string();
    let backup_root = home.join(".dotfiles-backups").join(timestamp);

    let mut adopted_targets: HashSet<PathBuf> = HashSet::new();
    let mut accepted_by_package: HashSet<String> = HashSet::new();

    for conflict in &all_conflicts {
        let approve = if args.yes {
            true
        } else if !atty_stdin() {
            false
        } else {
            let answer = prompt(&format!(
                "Adopt [{}] {} ? [y/N]",
                conflict.package,
                conflict.target_rel.display()
            ))?;
            matches!(answer.to_lowercase().as_str(), "y" | "yes")
        };

        if !approve {
            println!(
                "skipping [{}] {}",
                conflict.package,
                conflict.target_rel.display()
            );
            continue;
        }

        let abs_target = home.join(&conflict.target_rel);
        let meta = fs::symlink_metadata(&abs_target);
        if meta.is_err() {
            println!(
                "target missing; skipping backup [{}] {}",
                conflict.package,
                conflict.target_rel.display()
            );
            accepted_by_package.insert(conflict.package.clone());
            continue;
        }

        let backup_path = backup_root.join(&conflict.target_rel);
        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed creating backup dir {}: {e}", parent.display()))?;
        }

        fs::rename(&abs_target, &backup_path).map_err(|e| {
            format!(
                "failed to move {} to {}: {e}",
                abs_target.display(),
                backup_path.display()
            )
        })?;
        println!(
            "backed up {} -> {}",
            abs_target.display(),
            backup_path.display()
        );
        adopted_targets.insert(conflict.target_rel.clone());
        accepted_by_package.insert(conflict.package.clone());
    }

    if adopted_targets.is_empty() {
        println!("no conflicts adopted.");
        return Ok(());
    }

    println!("applying stow packages after adoption...");
    let packages = stow_packages(&dot)?;
    for package in packages {
        if !accepted_by_package.contains(&package) {
            continue;
        }
        run_cmd_in_dir(
            &dot,
            "stow",
            &["-d", ".", "-t", &home.display().to_string(), "-R", &package],
        )?;
    }

    println!(
        "adoption complete. backups saved under {}",
        backup_root.display()
    );
    Ok(())
}

fn stow_packages(dot: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    parse_stow_packages_file(&dot.join("config").join("stow-packages.txt"), &mut out)?;
    parse_stow_packages_file(
        &dot.join("config").join("stow-packages.local.txt"),
        &mut out,
    )?;
    Ok(out)
}

fn parse_stow_packages_file(path: &Path, out: &mut Vec<String>) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let content =
        fs::read_to_string(path).map_err(|e| format!("failed reading {}: {e}", path.display()))?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        out.push(trimmed.to_string());
    }
    Ok(())
}

fn stow_conflicts_for_package(
    dot: &Path,
    home: &Path,
    package: &str,
) -> Result<Vec<StowConflict>, String> {
    let out = Command::new("stow")
        .current_dir(dot)
        .args(["-d", ".", "-t", &home.display().to_string(), "-n", package])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run stow dry-run for {package}: {e}"))?;

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let mut conflicts = Vec::new();
    for raw in combined.lines() {
        let line = raw.trim();
        if !line.contains("cannot stow") && !line.contains("existing target is not owned by stow:")
        {
            continue;
        }
        let target = if let Some(idx) = line.find("existing target is not owned by stow:") {
            let rel = line[(idx + "existing target is not owned by stow:".len())..].trim();
            PathBuf::from(rel)
        } else if let Some(idx) = line.find("existing target ") {
            let rest = &line[(idx + "existing target ".len())..];
            let rel = rest.split(" since ").next().unwrap_or(rest).trim();
            PathBuf::from(rel)
        } else {
            continue;
        };
        conflicts.push(StowConflict {
            package: package.to_string(),
            target_rel: target,
            reason: "target exists and is unmanaged".to_string(),
        });
    }
    Ok(conflicts)
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
    run_dot_task_with_env(task, &[])
}

fn run_dot_task_with_env(task: &str, envs: &[(&str, &str)]) -> Result<(), String> {
    let dot = dotfiles_dir();
    if !dot.is_dir() {
        return Err(format!(
            "dotfiles directory not found: {} (set PJ_DOTFILES_DIR)",
            dot.display()
        ));
    }

    if which("mise").is_some() {
        return run_cmd_in_dir_with_env(&dot, "mise", &["run", task], envs);
    }
    if which("just").is_some() {
        return run_cmd_in_dir_with_env(&dot, "just", &[task], envs);
    }
    if which("make").is_some() {
        return run_cmd_in_dir_with_env(&dot, "make", &[task], envs);
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
    let menu = [
        "Doctor",
        "Context",
        "Cache Status",
        "Cache + Binaries",
        "Cache Clean (Dry Run)",
        "Secret Scan (Staged)",
        "Install Secret Hooks",
        "Install Local Binary",
        "Dot Info",
        "Dot Tasks",
        "Dot Doctor",
        "Dot Up",
        "Quit",
    ];

    while !app.should_quit {
        terminal
            .draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Length(16),
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
                    app.pending_confirm = None;
                }
                KeyCode::Down => {
                    app.menu_index = (app.menu_index + 1) % menu.len();
                    app.pending_confirm = None;
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
                        app.report = None;
                        match collect_context_report() {
                            Ok(ctx) => {
                                let root = ctx.project_root.unwrap_or_else(|| "(none)".to_string());
                                let kinds = if ctx.project_types.is_empty() {
                                    "none".to_string()
                                } else {
                                    ctx.project_types.join(", ")
                                };
                                let env_files = if ctx.env.env_files_present.is_empty() {
                                    "none".to_string()
                                } else {
                                    ctx.env.env_files_present.join(", ")
                                };
                                app.message = format!(
                                    "Context: root={root} | types={kinds} | env_files={env_files}"
                                );
                            }
                            Err(e) => app.message = format!("Context failed: {e}"),
                        }
                    }
                    2 => {
                        app.report = None;
                        let cwd = std::env::current_dir();
                        match cwd {
                            Ok(cwd) => {
                                let root = detect_project_root(&cwd).unwrap_or(cwd);
                                let mut entries = detect_cache_entries(&root, false);
                                entries.retain(|e| Path::new(&e.path).exists());
                                let total: u64 = entries.iter().map(|e| e.bytes).sum();
                                app.message = format!(
                                    "Cache status: {} across {} path(s).",
                                    human_size(total),
                                    entries.len()
                                );
                            }
                            Err(e) => app.message = format!("Cache status failed: {e}"),
                        }
                    }
                    3 => {
                        app.report = None;
                        let cwd = std::env::current_dir();
                        match cwd {
                            Ok(cwd) => {
                                let root = detect_project_root(&cwd).unwrap_or(cwd);
                                let bins = detect_promotable_binaries(&root);
                                if bins.is_empty() {
                                    app.message =
                                        "No promotable binaries found in target/{debug,release}."
                                            .to_string();
                                } else {
                                    let names: Vec<String> = bins
                                        .iter()
                                        .map(|b| format!("{}:{}", b.name, b.profile))
                                        .collect();
                                    app.message =
                                        format!("Promotable binaries: {}", names.join(", "));
                                }
                            }
                            Err(e) => app.message = format!("Binary scan failed: {e}"),
                        }
                    }
                    4 => {
                        app.report = None;
                        let cwd = std::env::current_dir();
                        match cwd {
                            Ok(cwd) => {
                                let root = detect_project_root(&cwd).unwrap_or(cwd);
                                let all = detect_cache_entries(&root, false);
                                let reclaim: u64 = all
                                    .iter()
                                    .filter(|e| e.kind == "rust-debug")
                                    .map(|e| e.bytes)
                                    .sum();
                                app.message = format!(
                                    "Dry-run reclaim estimate: {}. Run `pj cache clean --dry-run --promote-binaries`.",
                                    human_size(reclaim)
                                );
                            }
                            Err(e) => app.message = format!("Cache dry-run failed: {e}"),
                        }
                    }
                    5 => {
                        app.report = None;
                        let out = Command::new("git")
                            .args(["diff", "--cached", "--text", "--unified=0"])
                            .stdout(Stdio::piped())
                            .stderr(Stdio::null())
                            .output();
                        match out {
                            Ok(out) if out.status.success() => {
                                let text = String::from_utf8_lossy(&out.stdout).to_string();
                                let findings = scan_text_for_secrets(&text);
                                if findings.is_empty() {
                                    app.message = "Secret scan passed (staged diff).".to_string();
                                } else {
                                    app.message =
                                        format!("Secret scan findings: {}", findings.join(", "));
                                }
                            }
                            Ok(_) => {
                                app.message =
                                    "Secret scan skipped (not a git repo or no staged diff)."
                                        .to_string()
                            }
                            Err(e) => app.message = format!("Secret scan failed: {e}"),
                        }
                    }
                    6 => {
                        app.report = None;
                        app.message = match run_secret_install_hooks() {
                            Ok(_) => "Installed global secret hooks.".to_string(),
                            Err(e) => format!("Install hooks failed: {e}"),
                        };
                    }
                    7 => {
                        app.report = None;
                        app.message =
                            "Run `pj install-local` in normal shell to update local binaries."
                                .to_string();
                    }
                    8 => {
                        app.report = None;
                        app.message = match tui_dot_info_summary() {
                            Ok(s) => s,
                            Err(e) => format!("Dot info failed: {e}"),
                        };
                    }
                    9 => {
                        app.report = None;
                        app.message = match run_dot_tasks_capture() {
                            Ok(s) => truncate_for_tui(&s, 700),
                            Err(e) => format!("Dot tasks failed: {e}"),
                        };
                    }
                    10 => {
                        app.report = None;
                        app.message = match run_dot_task_capture("doctor") {
                            Ok(s) => truncate_for_tui(&s, 700),
                            Err(e) => format!("Dot doctor failed: {e}"),
                        };
                    }
                    11 => {
                        app.report = None;
                        if app.pending_confirm != Some(11) {
                            app.pending_confirm = Some(11);
                            app.message = "Confirm Dot Up: press Enter again to proceed (starts runtime/cluster). Use arrows to cancel.".to_string();
                        } else {
                            app.pending_confirm = None;
                            app.message = match run_dot_task_capture("up") {
                                Ok(s) => truncate_for_tui(&s, 700),
                                Err(e) => format!("Dot up failed: {e}"),
                            };
                        }
                    }
                    12 => app.should_quit = true,
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

fn run_cmd_in_dir_capture(dir: &Path, bin: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(bin)
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to execute {bin} in {}: {e}", dir.display()))?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = if stderr.trim().is_empty() {
        stdout.to_string()
    } else if stdout.trim().is_empty() {
        stderr.to_string()
    } else {
        format!("{stdout}\n{stderr}")
    };

    if out.status.success() {
        Ok(combined)
    } else {
        Err(format!(
            "command failed in {}: {} {}\n{}",
            dir.display(),
            bin,
            args.join(" "),
            truncate_for_tui(&combined, 600)
        ))
    }
}

fn run_dot_task_capture(task: &str) -> Result<String, String> {
    let dot = dotfiles_dir();
    if !dot.is_dir() {
        return Err(format!(
            "dotfiles directory not found: {} (set PJ_DOTFILES_DIR)",
            dot.display()
        ));
    }
    if which("mise").is_some() {
        return run_cmd_in_dir_capture(&dot, "mise", &["run", task]);
    }
    if which("just").is_some() {
        return run_cmd_in_dir_capture(&dot, "just", &[task]);
    }
    if which("make").is_some() {
        return run_cmd_in_dir_capture(&dot, "make", &[task]);
    }
    Err("none of mise/just/make found to run dotfiles tasks".to_string())
}

fn run_dot_tasks_capture() -> Result<String, String> {
    let dot = dotfiles_dir();
    if !dot.is_dir() {
        return Err(format!(
            "dotfiles directory not found: {} (set PJ_DOTFILES_DIR)",
            dot.display()
        ));
    }

    let mut sections = Vec::new();
    if which("mise").is_some() {
        let s = run_cmd_in_dir_capture(&dot, "mise", &["tasks", "ls"])?;
        sections.push(format!("== mise tasks ==\n{s}"));
    }
    if which("just").is_some() {
        let s = run_cmd_in_dir_capture(&dot, "just", &["--list"])?;
        sections.push(format!("== just recipes ==\n{s}"));
    }
    if which("make").is_some() {
        if which("rg").is_some() {
            let s = run_cmd_in_dir_capture(&dot, "rg", &["-n", "^([a-zA-Z0-9_-]+):", "Makefile"])?;
            sections.push(format!("== make targets ==\n{s}"));
        } else {
            sections.push("== make targets ==\nrg not found; skipping".to_string());
        }
    }
    Ok(sections.join("\n"))
}

fn tui_dot_info_summary() -> Result<String, String> {
    let dot = dotfiles_dir();
    if !dot.is_dir() {
        return Err(format!(
            "dotfiles directory not found: {} (set PJ_DOTFILES_DIR)",
            dot.display()
        ));
    }
    let manager = detect_dot_manager(&dot);
    let status = run_cmd_in_dir_capture(&dot, "git", &["status", "-sb"]).unwrap_or_default();
    Ok(format!(
        "dotfiles={} manager={} task_runners(mise={},just={},make={})\n{}",
        dot.display(),
        match manager {
            DotManager::Chezmoi => "chezmoi",
            DotManager::Stow => "stow",
            DotManager::Unknown => "unknown",
        },
        if which("mise").is_some() { "yes" } else { "no" },
        if which("just").is_some() { "yes" } else { "no" },
        if which("make").is_some() { "yes" } else { "no" },
        truncate_for_tui(&status, 380)
    ))
}

fn truncate_for_tui(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let out: String = trimmed.chars().take(max_chars).collect();
    format!("{out}\n... (truncated)")
}

fn run_cmd_in_dir(dir: &Path, bin: &str, args: &[&str]) -> Result<(), String> {
    run_cmd_in_dir_with_env(dir, bin, args, &[])
}

fn run_cmd_in_dir_with_env(
    dir: &Path,
    bin: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(), String> {
    let mut cmd = Command::new(bin);
    cmd.current_dir(dir)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for (k, v) in envs {
        cmd.env(k, v);
    }

    let status = cmd
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

fn detect_project_root(start: &Path) -> Option<PathBuf> {
    let markers = [
        ".git",
        ".mise.toml",
        "mise.toml",
        "Cargo.toml",
        "go.mod",
        "pyproject.toml",
        "package.json",
        "Justfile",
        "justfile",
        "Makefile",
        "config/stow-packages.txt",
        ".chezmoi.toml",
    ];

    let mut cur = start.to_path_buf();
    loop {
        if markers.iter().any(|m| cur.join(m).exists()) {
            return Some(cur);
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

fn expand_home(input: &str) -> PathBuf {
    if let Some(stripped) = input.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(stripped);
    }
    PathBuf::from(input)
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

fn set_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(|e| format!("failed to read metadata {}: {e}", path.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)
            .map_err(|e| format!("failed to set executable bit on {}: {e}", path.display()))?;
    }
    Ok(())
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

fn git_config_local_get(dir: Option<&Path>, key: &str) -> Option<String> {
    let dir = dir?;
    let out = Command::new("git")
        .current_dir(dir)
        .args(["config", "--local", "--get", key])
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
