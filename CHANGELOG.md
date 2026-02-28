# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/89jobrien/pj/compare/v0.1.0...v0.1.1) - 2026-02-28

### Added

- *(secret-scan)* reduce false positives with line-aware heuristics
- *(sync)* add one-shot pj sync command and tui action
- *(pj)* add ctx/cache/secret/dot flows, update command, tui expansion, and release-plz automation
- *(dot)* detect stow/chezmoi and add dotfiles-aware commands
- add ratatui pj CLI with dotfiles interface and release workflows

### Other

- *(ctx)* ignore project-local .ctx handoff directory
