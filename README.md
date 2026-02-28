# pj

Portable Rust CLI for bootstrapping and checking your developer environment.

## Commands

```bash
pj                    # opens ratatui dashboard
pj tui
pj doctor
pj doctor --json
pj up
pj dot install
pj dot doctor
pj dot up
pj dot info
pj dot apply
pj dot tasks
pj dot repo-status
pj dot observe
pj git-config
pj git-config --name "Your Name" --email "you@example.com"
```

- `doctor`: checks core local tools (`git`, `gh`, `mise`, `uv`, `bun`, `docker`, `colima`, `kubectl`, `k3d`).
- `up`: if `~/dotfiles` exists, runs your dotfiles `up` task; otherwise falls back to local `mise/colima/k3d`.
- `dot`: explicit interface to dotfiles tasks and repo management.
  - detects `stow` and/or `chezmoi` layout (`pj dot info`)
  - applies with detected manager (`pj dot apply`)
  - supports repo ops (`repo-status`, `repo-diff`, `repo-log`, `pull`, `push`)
  - supports setup tasks (`install`, `doctor`, `up`, `drift`, `observe`, `observe-k8s`, `observe-logs`, `container-status`)
- `git-config`: sets global git identity from flags, env (`GIT_USER_NAME`, `GIT_USER_EMAIL`), GitHub CLI, or prompt.
- `tui` (or no command): opens a ratatui dashboard for quick status checks.

## Build

```bash
cargo build --release
```

## Install (local)

```bash
cargo install --path .
```
