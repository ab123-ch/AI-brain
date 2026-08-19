# 🦞 Claw Code — Rust Implementation

A high-performance Rust rewrite of the Claw Code CLI agent harness. Built for speed, safety, and native tool execution.

## Quick Start

```bash
# Build
cd rust/
cargo build --release

# Run interactive REPL
./target/release/claw

# One-shot prompt
./target/release/claw prompt "explain this codebase"

# With specific model
./target/release/claw --model sonnet prompt "fix the bug in main.rs"
```

## Private Remote Access (Windows Host)

Detailed Windows setup, native testing, phone verification, and Codex handoff:
[docs/windows-remote-access.md](docs/windows-remote-access.md).

AI Brain can expose its Web UI to your phone or another computer through a
private Tailscale network. The backend remains bound to `127.0.0.1`; it is not
published to the public internet.

1. On the Windows host, install Tailscale for Windows and sign in:
   <https://tailscale.com/download/windows>
2. Install Tailscale on the phone and sign it into the same Tailnet.
3. Enable Tailscale unattended mode if the PC should remain in the Tailnet at
   the Windows sign-in screen.
4. Start the normal AI Brain Web service from the workspace it should control:

```powershell
cargo build --release -p ai-brain-cli
.\target\release\ai-brain.exe web
```

Normal `web` startup launches the installed Tailscale app/service, restores its
persistent private Serve proxy, and prints a stable
`https://<device>.<tailnet>.ts.net` address. The first successful setup writes
that address to `~/.ai-brain/config.toml`; open it from any device signed into
the same Tailnet. Local loopback access remains available, while non-local
requests require Tailscale identity and a matching HTTPS origin.

Existing installations without this section use these code defaults:

```toml
[remote_access]
enabled = true
port = 8080
# Filled automatically after the first successful connection:
# url = "https://home-brain.example.ts.net"
```

Set `enabled = false` to keep `web` local-only. `web --addr 127.0.0.1:9090`
overrides the configured port and updates it after a successful remote setup.
The explicit `ai-brain.exe remote --port 8080` command remains available as a
strict remote-only diagnostic mode.

Run Tailscale on the Windows host, not in a second WSL 2 installation. Keep the
`ai-brain.exe web` process running while remote control is needed. Windows may
be locked, but signing out, restarting, or sleeping stops an interactively
started process. Starting AI Brain itself after reboot remains the
responsibility of its existing shortcut, scheduled task, or service launcher.

Tailscale Serve persists its proxy configuration. Disable it when no longer
needed:

```powershell
tailscale serve off
```

Do not replace this setup with a `0.0.0.0` bind or router port forwarding: the
Web UI can execute tools and edit files in the current workspace.

## AI Brain Command Execution on Windows

On native Windows, AI Brain's generic `bash` tool defaults to WSL Bash. The Web
server, SQLite database, file tools, MCP services, and Tailscale remain native
Windows processes. macOS and Linux continue to default to host `sh -lc`.

The backend can be selected per user or workspace with `.claw` JSON settings:

```json
{
  "commandExecution": {
    "backend": "auto",
    "wsl": {
      "distribution": "Ubuntu-24.04",
      "user": "brain"
    }
  }
}
```

Supported backend values are `auto`, `wsl`, `powershell`, and `sh`. `auto`
selects WSL on Windows and host `sh` elsewhere; it does not fall back after an
execution failure. New Web member tasks freeze the resolved backend, and
delegated agents inherit it. The explicit `PowerShell` tool remains independent.

See [docs/command-execution.md](docs/command-execution.md) for configuration
precedence and lifecycle details, and
[docs/windows-remote-access.md](docs/windows-remote-access.md) for native Windows
installation and acceptance checks.

## Configuration

Set your API credentials:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
# Or use a proxy
export ANTHROPIC_BASE_URL="https://your-proxy.com"
```

Or authenticate via OAuth:

```bash
claw login
```

## Features

| Feature | Status |
|---------|--------|
| Anthropic API + streaming | ✅ |
| OAuth login/logout | ✅ |
| Interactive REPL (rustyline) | ✅ |
| Tool system (bash, read, write, edit, grep, glob) | ✅ |
| Web tools (search, fetch) | ✅ |
| Sub-agent orchestration | ✅ |
| Todo tracking | ✅ |
| Notebook editing | ✅ |
| CLAUDE.md / project memory | ✅ |
| Config file hierarchy (.claude.json) | ✅ |
| Permission system | ✅ |
| MCP server lifecycle | ✅ |
| Session persistence + resume | ✅ |
| Extended thinking (thinking blocks) | ✅ |
| Cost tracking + usage display | ✅ |
| Git integration | ✅ |
| Markdown terminal rendering (ANSI) | ✅ |
| Model aliases (opus/sonnet/haiku) | ✅ |
| Slash commands (/status, /compact, /clear, etc.) | ✅ |
| Hooks (PreToolUse/PostToolUse) | 🔧 Config only |
| Plugin system | 📋 Planned |
| Skills registry | 📋 Planned |

## Model Aliases

Short names resolve to the latest model versions:

| Alias | Resolves To |
|-------|------------|
| `opus` | `claude-opus-4-6` |
| `sonnet` | `claude-sonnet-4-6` |
| `haiku` | `claude-haiku-4-5-20251213` |

## CLI Flags

```
claw [OPTIONS] [COMMAND]

Options:
  --model MODEL                    Set the model (alias or full name)
  --dangerously-skip-permissions   Skip all permission checks
  --permission-mode MODE           Set read-only, workspace-write, or danger-full-access
  --allowedTools TOOLS             Restrict enabled tools
  --output-format FORMAT           Output format (text or json)
  --version, -V                    Print version info

Commands:
  prompt <text>      One-shot prompt (non-interactive)
  login              Authenticate via OAuth
  logout             Clear stored credentials
  init               Initialize project config
  doctor             Check environment health
  self-update        Update to latest version
```

## Slash Commands (REPL)

Tab completion now expands not just slash command names, but also common workflow arguments like model aliases, permission modes, and recent session IDs.

| Command | Description |
|---------|-------------|
| `/help` | Show help |
| `/status` | Show session status (model, tokens, cost) |
| `/cost` | Show cost breakdown |
| `/compact` | Compact conversation history |
| `/clear` | Clear conversation |
| `/model [name]` | Show or switch model |
| `/permissions` | Show or switch permission mode |
| `/config [section]` | Show config (env, hooks, model) |
| `/memory` | Show CLAUDE.md contents |
| `/diff` | Show git diff |
| `/export [path]` | Export conversation |
| `/session [id]` | Resume a previous session |
| `/version` | Show version |

## Workspace Layout

```
rust/
├── Cargo.toml              # Workspace root
├── Cargo.lock
└── crates/
    ├── api/                # Anthropic API client + SSE streaming
    ├── commands/           # Shared slash-command registry
    ├── compat-harness/     # TS manifest extraction harness
    ├── runtime/            # Session, config, permissions, MCP, prompts
    ├── rusty-claude-cli/   # Main CLI binary (`claw`)
    └── tools/              # Built-in tool implementations
```

### Crate Responsibilities

- **api** — HTTP client, SSE stream parser, request/response types, auth (API key + OAuth bearer)
- **commands** — Slash command definitions and help text generation
- **compat-harness** — Extracts tool/prompt manifests from upstream TS source
- **runtime** — `ConversationRuntime` agentic loop, `ConfigLoader` hierarchy, `Session` persistence, permission policy, MCP client, system prompt assembly, usage tracking
- **rusty-claude-cli** — REPL, one-shot prompt, streaming display, tool call rendering, CLI argument parsing
- **tools** — Tool specs + execution: Bash, ReadFile, WriteFile, EditFile, GlobSearch, GrepSearch, Agent, TodoWrite, NotebookEdit, Skill, ToolSearch, REPL runtimes. Legacy WebSearch/WebFetch implementations remain internal but are not advertised by AI Brain.

## Stats

- **~20K lines** of Rust
- **6 crates** in workspace
- **Binary name:** `claw`
- **Default model:** `claude-opus-4-6`
- **Default permissions:** `danger-full-access`

## License

See repository root.
