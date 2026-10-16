# AGENTS.md

## What this repo is

**mishapi** is an *agentic first* REST API client written in Rust. Anything a person can do in the app, an AI agent can do too, and the person sees, approves or undoes every change. For the full feature list see `README.md` (es) / `README.en.md` (en).

Two binaries share one library:

- `mishapi` (`src/main.rs`): desktop app built with egui/eframe.
- `mishapi-cli` (`src/bin/mishapi-cli.rs`): command-line tool for CI and for agents working from the terminal.

## Layout

`src/lib.rs` defines the layers, from the inside out. **Each layer only uses the ones inside it**, never the other way around:

`domain` → `engine` → `infra` / `import` → `ui`

| Module | Contents |
|---|---|
| `domain/` | Model (requests, collections, environments), actions, `{{name}}` variables, change overlay. No I/O. |
| `engine/` | Collection runner and JS scripts (`pm.*`) on Boa, no Node. |
| `infra/` | HTTP (reqwest), auth (Bearer, Basic, API Key, OAuth 2.0/PKCE), cookies, disk storage, git repo sync, agent integration (Claude Code `claude -p` or Ollama). |
| `import/` | OpenAPI 3 / Swagger 2, Postman v2 and cURL importers. |
| `ui/` | egui panels and windows (sidebar, editor, response, agent panel, runner, etc.). |
| `i18n.rs` | UI strings in Spanish (default) and English. |
| `packaging/` | Installers (`install.ps1`, `install.sh`). |

## Commands

```sh
cargo build            # build
cargo test             # tests (they live in the lib; the GUI bin has test = false)
cargo build --release  # binaries in target/release/
```

Requires Rust 1.85+ (edition 2024).

## Rules for agents

- **All code is in English**: identifiers, comments, doc comments, test names, logs, internal errors and the prompts sent to the LLM.
- **Only the UI is in Spanish** (default language, with English as the alternative). Every user-visible string goes through `t("español", "english")` / `tr!(...)` from `i18n.rs`; never hardcode a visible string outside those helpers.
- Respect the layer direction; never import `ui` or `infra` from `domain`/`engine`.
- Never send to the agent or commit variable values, tokens or the data files (`collections.json`, `environments.json`, `settings.json`, `agents.json`, `cookies.json`, `repos/`); they are in `.gitignore`.
- Destructive or sensitive agent actions (deleting, generating load, writing scripts, changing auth, new hosts) must keep asking for approval.
