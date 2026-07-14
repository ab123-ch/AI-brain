# Novel Brain Delegation And Review Refactor

## Goal

Give the Novel sub-agent a low-cost, project-scoped read path while keeping the main brain responsible for task analysis, file discovery, persistence, and Canon commits. Add self-review inside the Novel result, independent review by the main brain, and disable the general EvalBrain by default.

## Constraints

- Preserve the project snapshot as the authoritative Canon source; the graph remains a derived index.
- Novel tools are read-only and limited to explicitly delegated project/files.
- The main brain supplies exact relevant paths and a structured task contract so the Novel brain does not explore from scratch.
- Do not touch unrelated remote-access or cockpit work already present in the worktree.

## Phases

### Phase 1: Runtime Boundaries

**Status:** complete

Inspect runtime/tool boundaries and settle the smallest compatible protocol.

### Phase 2: Scoped Delegation

**Status:** complete

Add structured Novel delegation, scoped file access, and project-memory lookup.

### Phase 3: Review Flow

**Status:** complete

Add structured Novel self-review and main-brain review/revision flow.

### Phase 4: Eval Opt-In

**Status:** complete

Disable the general EvalBrain by default while retaining opt-in configuration.

### Phase 5: Verification

**Status:** complete

Add focused tests, format, and run package-level regression checks.

## Decisions

- Extend the existing `Agent` tool rather than create a second agent runtime.
- Keep existing free-form `prompt` compatibility for non-Novel agents.
- Prefer explicit paths and project identifiers over directory crawling.
- Make review outputs machine-detectable so the main brain can reliably act on them.
- Pass an `AgentRuntimeContext` from `RealToolExecutor` instead of exposing memory paths in the public tool schema.
- Reject background execution for Novel agents because main-brain review is part of the same writing transaction.
- Keep EvalBrain opt-in: `enabled = true` retains the existing `on_file_edit` mode unless explicitly changed.

## Errors Encountered

| Error | Attempt | Resolution |
|---|---|---|
| Borrow after moving normalized Novel type into manifest | First `cargo check -p tools` | Compute `requires_main_review` before moving the type string. |
| Two prompt tests expected wording removed by the new protocol | First targeted test run | Update assertions to the new Web-research gate and tagged content contract. |
| Combined prompt-test patch had an invalid empty hunk | First patch attempt | Reissued a syntactically valid multi-file patch. |
| Serde-default patch first matched the global Hook switch | Review immediately after patch | Restored global `enabled=true` default and applied `false` only to `eval_gate.enabled`. |
| Full `tools` package test run completed 46/50 | Re-ran each failure individually | Confirmed four pre-existing expectation/runtime failures outside this diff: `file_tools_cover_read_write_and_edit_behaviors`, `tool_search_supports_keyword_and_select_queries`, `provider_runtime_client_creates_with_default_config`, and `skill_loads_local_skill_prompt`. Novel-focused tests pass. |
| Workspace-wide Clippy with `-D warnings` failed | Isolated the changed packages | Fixed the one new `items_after_test_module` warning in `brain-hooks::config`; remaining strict failures are pre-existing lints in `brain-hooks::runner`, `brain-mcp`, `brain-dispatch`, `brain-sensory`, and `runtime`. |
| Final review found Novel graph reads were domain-scoped but not fully project-scoped | Inspect graph IDs, node props, and Catalog matching | Add explicit project ownership validation/filtering for Catalog/detail/trace and a cross-project regression test. |
| Offline `tools` regression appeared hung after 60 seconds | Waited for integration tests rather than rerunning | Real provider/sub-agent tests completed at 115.95s; all 47 non-baseline-failure tests passed. |
| Strict `tools` Clippy still failed | Separated existing warnings from new code | Existing warnings remain in unrelated graph indexing/provider/runtime sections. Refactor the new Novel graph dispatcher and borrowed runtime context; do not broaden into baseline cleanup. |
