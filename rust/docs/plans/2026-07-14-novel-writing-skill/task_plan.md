# Novel Writing Skill Refactor

## Goal

Move the detailed novel-writing workflow out of oversized system prompts into a discoverable skill. Keep only security boundaries and non-bypassable review rules in system prompts. Make the main brain load the skill before writing tasks and make the Novel brain follow the same workflow without redundant exploration.

## Constraints

- Preserve the structured `novel_context`, project/file isolation, and dual-review gate.
- Use the repository's actual SkillCatalog conventions and default loading mechanism.
- Keep the skill concise and reusable; do not duplicate its full body in prompts.
- Preserve unrelated remote-access and root planning changes.

## Phases

### Phase 1: Skill Runtime Discovery

**Status:** complete

Trace skill roots, metadata parsing, bootstrap injection, and explicit Skill tool behavior.

### Phase 2: Skill Design

**Status:** complete

Define the main-brain writing workflow, Novel handoff contract, and minimal prompt invariants.

### Phase 3: Implementation

**Status:** complete

Create and register the skill, then slim and align main/Novel prompts.

### Phase 4: Verification

**Status:** complete

Validate skill metadata, loading/trigger behavior, prompt tests, and package regressions.

## Decisions

- Treat this as an application-owned skill, not a user-global Codex skill, because it must ship with and be loaded by AI Brain.
- Do not remove runtime-enforced file/project isolation from code even if the skill documents it.
- Prefer catalog discovery plus explicit `Skill` loading over bootstrap injection, so non-writing queries pay only the metadata cost.
- Reuse one skill body for both brains: explicit load in the main brain and runtime injection in the Novel brain.
- Keep the skill in `crates/brain-main/skills/novel-writing-workflow`; embed/seed it for installed runtime discovery and compile the same source into the Novel agent prompt.

## Errors Encountered

| Error | Attempt | Resolution |
|---|---|---|
| `init_skill.py` rejected a 24-character `short_description` after creating `SKILL.md` | First initialization | Keep the generated template and use `generate_openai_yaml.py` with a 25-64 character description; do not rerun initialization over the existing directory. |
| Main prompt test lost the explicit `Agent(subagent_type='Novel')` route after slimming | First prompt test | Restore the stable tool-routing form in the minimal system rule while keeping workflow details in the skill. |
| Final parallel check wrapper had a JavaScript parse error before executing commands | First final-check attempt | Split status/stat and simplify the search command; rerun each check as an independent tool call. |
| `wait_agent` was first called with an invalid 1-second timeout | Forward-test polling | Retry with the documented 10-second minimum; both forward tests later completed successfully. |
