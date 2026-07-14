# Findings

- The previous refactor deliberately placed the full writing workflow in main and Novel system prompts; the user now prefers skill-based progressive disclosure.
- `SkillCatalog` already supports discovery, explicit `Skill` tool loading, and bootstrap content injection into `MainBrain`.
- Existing repository skills are currently evaluator-specific under `crates/brain-eval/skills`; the main-brain skill roots and pack conventions still need inspection.
- Direct skills are scanned from each configured root and summarized into the main system prompt; `Skill` then loads the body on demand.
- Bootstrap would inject the entire workflow for every query, which conflicts with progressive disclosure and the requested "writing task first calls the skill" behavior.
- The current built-in roots cover workspace/user/plugin directories but no application-owned main-brain skills directory.
- Novel sub-agents use `SubagentToolExecutor`, not `RealToolExecutor`, so merely adding `Skill` to their allowlist would not give them the main SkillCatalog. The shared skill body should instead be injected into Novel runtime context by `RealToolExecutor`.
- The robust target design is: embed/seed an application-owned `novel-writing-workflow` skill into a built-in catalog root; main brain explicitly calls it on writing tasks; Novel agents receive that same body automatically from runtime context.
- The application already has `tempfile` for CLI tests, so built-in skill seeding can be verified without touching the real home directory.
- `MainBrain` appends only the skill summary at startup and loads bodies through the `Skill` tool, which matches the desired progressive-disclosure behavior.
- The existing Novel system prompt duplicates nearly the full new workflow and Delta schema; it can be reduced to identity, hard permission boundaries, the injected-skill requirement, and the machine-validated output protocol.
- Independent main-role forward testing loaded the skill first, then correctly ordered project lookup, exact-file reads, Canon recall, consistency checking, synchronous Novel delegation, independent review, save, and Delta commit.
- Independent Novel-role forward testing respected the delegated file/project/Web boundaries and reproduced all six checks, three output sections, and the rule that Novel pass still requires main review.
