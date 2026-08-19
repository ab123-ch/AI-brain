# Agent Skills

AI Brain discovers instruction-based skills compatible with its native format,
Open Agent Skills, Claude Code, and Codex. Each skill is a directory containing
a `SKILL.md` with YAML `name` and `description` fields.

## Discovery

For direct execution and each Web room, project roots are resolved from that
execution's frozen working directory. AI Brain checks every directory from the
working directory through the nearest Git root, in this order:

1. `.ai-brain/skills`
2. `.agents/skills`
3. `.claude/skills`
4. `.codex/skills`

The global catalog then adds native AI Brain plugin skill roots and these user
or machine locations:

- `~/.ai-brain/skills`
- `~/.agents/skills`
- `$CLAUDE_CONFIG_DIR/skills` and `~/.claude/skills`
- `$CODEX_HOME/skills` and legacy `~/.codex/skills`
- `/etc/codex/skills` on Unix

Skill directories are traversed recursively to a bounded depth. Symlinked
folders are followed, canonical targets are de-duplicated, and earlier roots
win an unqualified name collision. The exact `SKILL.md` path shown in the skill
summary can select a shadowed skill explicitly.

## Loading

Startup and room prompts receive only bounded metadata: skill name,
description, optional usage hint, and canonical path. The summary is capped at
8,000 characters. The full instruction body is read only when the LLM calls
the `Skill` tool, whose result also includes the canonical skill directory so
relative `scripts/`, `references/`, and `assets/` paths resolve correctly.

Only native `.ai-brain/skills` and installed AI Brain plugin roots may opt into
automatic startup injection with `bootstrap: true`. That flag is ignored in
`.agents`, `.claude`, and `.codex` roots. Multiple trusted bootstrap skills are
composed instead of overwriting one another.

Catalogs are cached per canonical working directory for the process lifetime.
Restart AI Brain after adding or changing skills in an already-used directory.

## Network Tools

AI Brain does not advertise its legacy hard-coded `WebFetch` or `WebSearch`
implementations to LLMs. A network-oriented skill can instruct the model to use
an installed CLI through the command tool, including a WSL-installed CLI on
Windows. Alternatively, a future or separately connected MCP integration can
provide model-visible network tools.

The current AI Brain orchestrator reads MCP configuration files but does not
yet connect those servers or register their tool definitions. Merely adding an
MCP configuration does not currently make its tools available in Web rooms.
