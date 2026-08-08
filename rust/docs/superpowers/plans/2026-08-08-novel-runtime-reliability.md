# Novel Runtime Reliability Implementation Plan

> **For Codex:** REQUIRED SUB-SKILL: Use `executing-plans` to implement this plan task-by-task. Every behavior change follows red-green-refactor; do not add cross-provider fallback.

**Goal:** Make Novel Writer output contract-driven and diagnosable, stop deterministic provider-route retries, validate Novel tool recovery inputs before side effects, and make Web/TUI file logging reliable.

**Architecture:** `novel-workflow` remains the single source of truth for the versioned Writer JSON contract and strict parser. `ai-brain-cli` renders that contract into the frozen Writer prompt, permits exactly one same-provider/same-model format-repair request, and returns bounded redacted diagnostics through the existing persisted workflow error chain. Tool validation happens before conversation association or domain calls. Logging installs one composed subscriber per process.

**Tech Stack:** Rust, Tokio, serde/serde_json, tracing/tracing-subscriber, sha2, regex, SQLite-backed Novel/TaskEngine application tests.

---

### Task 1: Make `novel.writer-output.v1` explicit and strict

**Files:**
- Create: `crates/novel-workflow/src/writer_contract.rs`
- Modify: `crates/novel-workflow/src/lib.rs`
- Modify: `crates/novel-workflow/src/response.rs`

**Step 1: Write the failing parser tests**

Add tests to `response.rs` proving that a legal response must declare one of the two outcomes and that legacy tagged text is no longer accepted:

```rust
#[test]
fn rejects_json_without_explicit_outcome() {
    let error = parse_novel_response(
        r#"{"summary":"正文已生成"}"#,
        "task-1",
        "project-1",
        1,
        5,
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("missing or invalid Novel outcome"));
}

#[test]
fn rejects_legacy_tagged_writer_output() {
    let error = parse_novel_response(
        "[NOVEL_CONTENT]正文[/NOVEL_CONTENT]",
        "task-1",
        "project-1",
        1,
        5,
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("valid JSON object"));
}
```

Also add tests for blank draft content and blank clarification questions/reason.

**Step 2: Run the focused tests and confirm RED**

Run:

```powershell
cargo test -p novel-workflow response::tests -- --nocapture
```

Expected: the missing-outcome test fails because the parser defaults to `draft_ready`; the tagged-output assertion fails because tagged fallback is still active.

**Step 3: Implement strict parsing**

In `parse_novel_response`:

- Parse exactly one JSON object, while retaining the harmless outer Markdown-fence removal.
- Require a string `outcome`; never default it.
- Accept only `draft_ready` and `needs_clarification`.
- Require `draft_ready` payload under `draft`.
- Reject blank `content`.
- Reject `needs_clarification` when `questions` is empty, any question is blank, or `reason` is blank.
- Delete `tagged_section` and the legacy tagged-text fallback.
- Preserve frozen project/revision validation and evidence fallback.

The key dispatch must be:

```rust
let value = parse_json_value(raw).map_err(|error| {
    NovelWorkflowError::Invalid(format!("Writer output must be one valid JSON object: {error}"))
})?;
let outcome = value
    .get("outcome")
    .and_then(serde_json::Value::as_str)
    .ok_or_else(|| NovelWorkflowError::Invalid("missing or invalid Novel outcome".into()))?;
```

**Step 4: Write the failing contract-rendering test**

Create `writer_contract.rs` with a test that builds a `NovelTaskRequest` and asserts the rendered contract contains:

- `novel.writer-output.v1`
- both explicit outcomes
- all six self-review check keys
- every `NovelMemoryDelta` list field
- the frozen `project_id`, branch, revision, task type and `source_ref`
- the supplied evidence refs
- an instruction that no additional top-level shapes are accepted

The public function signature is:

```rust
pub fn render_writer_output_contract(
    request: &NovelTaskRequest,
    branch_id: &str,
    evidence_refs: &[String],
) -> Result<String>
```

Export it from `lib.rs` with `pub use writer_contract::render_writer_output_contract;`.

**Step 5: Run the contract test and confirm RED**

Run:

```powershell
cargo test -p novel-workflow writer_contract::tests -- --nocapture
```

Expected: compilation fails because the new module/function does not exist yet.

**Step 6: Implement the versioned contract renderer**

Build a human-readable instruction followed by pretty JSON examples generated with `serde_json::json!`. The `draft_ready` example must show:

```json
{
  "outcome": "draft_ready",
  "draft": {
    "content": "<非空正文>",
    "self_review": {
      "verdict": "pass",
      "checks": {
        "outline_alignment": "pass",
        "canon_consistency": "pass",
        "character_consistency": "pass",
        "timeline_consistency": "pass",
        "plot_and_foreshadowing": "pass",
        "style_and_repetition": "pass"
      },
      "issues": [],
      "unverified_assumptions": [],
      "summary": "<六项检查摘要>"
    },
    "proposed_delta": {
      "project_id": "<frozen project>",
      "branch_id": "<frozen branch>",
      "expected_revision": 0,
      "task_type": "body",
      "source_ref": "<frozen output path>",
      "progress": null,
      "proposed_facts": [],
      "state_changes": [],
      "plot_updates": [],
      "foreshadowing_updates": [],
      "feedback": [],
      "experience_candidates": []
    },
    "evidence_refs": []
  }
}
```

Render dynamic values from the frozen request rather than interpolating user-controlled text into structural instructions. Include a separate `needs_clarification` example with non-empty `questions` and `reason`.

**Step 7: Run focused GREEN tests**

Run:

```powershell
cargo test -p novel-workflow response::tests writer_contract::tests -- --nocapture
```

Expected: all focused contract/parser tests pass.

**Step 8: Commit**

```powershell
git add crates/novel-workflow/src/lib.rs crates/novel-workflow/src/response.rs crates/novel-workflow/src/writer_contract.rs
git commit -m "fix(novel): enforce explicit writer output contract"
```

### Task 2: Add one bounded same-route format repair and safe diagnostics

**Files:**
- Modify: `crates/ai-brain-cli/Cargo.toml`
- Modify: `crates/ai-brain-cli/src/novel_adapters.rs`

**Step 1: Replace the single-response test stub with a response queue**

Use `Mutex<Vec<ChatRequest>>` plus `Mutex<VecDeque<brain_llm::Result<ChatResponse>>>`. The stub records every request and pops exactly one configured response. It never chooses a provider or manufactures fallback behavior.

Add helper constructors for successful responses and full valid `draft_ready` JSON.

**Step 2: Write the failing prompt/repair/usage tests**

Add these async behavior tests:

1. `writer_prompt_contains_frozen_versioned_contract` asserts the system prompt contains all contract fields plus frozen project/revision/task/source values.
2. `writer_repairs_invalid_shape_once_on_same_model_and_sums_usage` queues malformed JSON then a valid response, and asserts:
   - exactly two calls;
   - both calls use the configured model;
   - the second conversation includes the first raw response as an assistant message;
   - second `max_tokens` is the reservation minus the first completion usage;
   - returned `ActualUsage` is the saturating sum of both calls.
3. `writer_does_not_repair_provider_failure` queues an `LlmError::ApiError`, asserts one call only, and asserts the returned error names provider and model.
4. `writer_stops_after_second_invalid_shape_with_redacted_bounded_diagnostic` queues two malformed responses containing `Authorization: Bearer ...`, JSON `api_key`/`token`/`secret` values, an `sk-...` token and control characters. Assert the error contains attempt, route, finish reason and full SHA-256, excludes every secret, and the preview is bounded.

**Step 3: Run the focused tests and confirm RED**

Run:

```powershell
cargo test -p ai-brain-cli novel_adapters::tests::writer_ -- --nocapture
```

Expected: new prompt assertions fail, only one request occurs, diagnostics lack route/hash/redaction, and provider errors lack explicit route.

**Step 4: Add deterministic diagnostic helpers**

Add `regex = "1"` to `ai-brain-cli` dependencies and implement private helpers:

```rust
const WRITER_DIAGNOSTIC_PREVIEW_CHARS: usize = 2_048;

fn response_usage(response: &ChatResponse) -> ActualUsage { /* actual prompt/completion */ }
fn add_usage(left: ActualUsage, right: ActualUsage) -> ActualUsage { /* saturating_add */ }
fn remaining_output_tokens(reserved: u32, used: u64) -> u32 { /* saturating subtraction */ }
fn redact_writer_output(raw: &str) -> String { /* auth/key/token/secret/sk-* */ }
fn escape_control_chars(raw: &str) -> String { /* \n, \r, \t, \u{...} */ }
fn writer_output_diagnostic(...) -> String { /* route, attempt, finish, hash, preview */ }
```

Hash the full unmodified raw response with SHA-256. Redact the full response before taking the preview, then escape controls and take at most 2,048 Unicode scalar values. Never include the full raw response in an error.

**Step 5: Implement the two-attempt state machine**

Build evidence refs before rendering the contract. Compose the first request from the frozen profile, rendered contract, workflow skill, and frozen context.

Execution rules:

- First provider error: return `Novel Writer Provider failed (provider=..., model=..., attempt=1): ...`; do not repair or route elsewhere.
- First parse error: if remaining output tokens are zero, return a bounded attempt-1 diagnostic.
- Otherwise append the first raw response as `assistant`, append a short `user` instruction to correct only the JSON shape against the same embedded contract, and call the same `Arc<dyn LlmProvider>` with the same model once.
- Repair provider error: return the provider route/error plus the redacted first-response diagnostic.
- Second parse error: emit `tracing::warn!` and return the bounded second-response diagnostic including the first parse error summary.
- Success: return the accepted raw output and actual usage summed across calls.
- Never call any other provider/model and never repair a provider/network/status error.

**Step 6: Run focused GREEN tests**

Run:

```powershell
cargo test -p ai-brain-cli novel_adapters::tests::writer_ -- --nocapture
```

Expected: all Writer Adapter tests pass and the response queue is exhausted exactly as asserted.

**Step 7: Commit**

```powershell
git add crates/ai-brain-cli/Cargo.toml crates/ai-brain-cli/src/novel_adapters.rs Cargo.lock
git commit -m "fix(novel): repair malformed writer output once"
```

### Task 3: Stop retrying deterministic model-route failures

**Files:**
- Modify: `crates/brain-llm/src/error.rs`

**Step 1: Write failing retry-classification tests**

Add tests proving:

```rust
#[test]
fn model_route_503_is_not_retryable() { /* model_not_found + No available channel */ }

#[test]
fn ordinary_503_remains_retryable() { /* temporary upstream overload */ }
```

Include both English variants observed in production and a Chinese `没有可用渠道` message.

**Step 2: Run RED test**

```powershell
cargo test -p brain-llm error::tests -- --nocapture
```

Expected: deterministic route 503 cases incorrectly return `true`.

**Step 3: Implement semantic classification**

Before the status-code match, lowercase the API message and reject retry for `model_not_found`, `model not found`, `no available channel`, or `没有可用渠道`. Keep ordinary 408/429/500/502/503/504 behavior unchanged. Do not add provider selection or fallback.

**Step 4: Run GREEN test and commit**

```powershell
cargo test -p brain-llm error::tests -- --nocapture
git add crates/brain-llm/src/error.rs
git commit -m "fix(llm): stop retrying unavailable model routes"
```

### Task 4: Validate Novel recovery actions before side effects

**Files:**
- Modify: `crates/ai-brain-cli/src/real_tool_executor.rs`
- Modify: `crates/tools/src/lib.rs`
- Modify: `crates/brain-main/skills/novel-writing-workflow/SKILL.md`

**Step 1: Write failing executor tests**

Add async tests covering real facade behavior:

1. A `novel_task resume` with an existing task, conversation scope and missing/blank `input` returns a stable Chinese recovery error before `associate_conversation_source` changes the checkpoint/source.
2. A stale `ContextRef` start retains `expected=...` and `actual=...` and appends: reread the resource, use the actual hash, keep the same task ID, retry once.

The resume error must be exactly actionable:

```text
novel_task resume 缺少或无效字段 input；仅当上一次结果为 needs_clarification 时，使用同一 task_id 并提供非空 input 后重试一次
```

**Step 2: Write failing tool-description test**

Extend the existing `tools` Novel schema test to require description text stating:

- `resume` is legal only after `needs_clarification`;
- `input` must be non-empty;
- on hash change, reread and retry once with `actual` hash and the same task ID.

**Step 3: Run RED tests**

```powershell
cargo test -p ai-brain-cli real_tool_executor::tests::novel_ -- --nocapture
cargo test -p tools novel_workflow_tools_replace_ephemeral_agent_schema -- --nocapture
```

Expected: executor error is generic serde text/association occurs first, and tool description lacks recovery guidance.

**Step 4: Implement preflight and typed error mapping**

Before removing `action` or associating conversation source, call a pure `validate_novel_action_input(name, action, &input)` function. For `novel_task/resume`, require non-empty `task_id` and `input` and return the stable message above. Validate required task IDs for other mutating task actions as well.

Add:

```rust
fn map_novel_application_error(error: NovelApplicationError) -> String {
    match error {
        NovelApplicationError::ContextChanged(message) => format!(
            "{message}；请重新读取资源，使用错误中的 actual hash，保持同一 task_id，并仅重试一次"
        ),
        other => other.to_string(),
    }
}
```

Use this mapper for every Novel application call. Only associate a conversation source after preflight succeeds.

Update the tool description and workflow skill with the same finite recovery rules. Do not weaken hash checks or auto-repeat calls.

**Step 5: Run GREEN tests and commit**

```powershell
cargo test -p ai-brain-cli real_tool_executor::tests::novel_ -- --nocapture
cargo test -p tools novel_workflow_tools_replace_ephemeral_agent_schema -- --nocapture
git add crates/ai-brain-cli/src/real_tool_executor.rs crates/tools/src/lib.rs crates/brain-main/skills/novel-writing-workflow/SKILL.md
git commit -m "fix(novel): validate recovery calls before mutation"
```

### Task 5: Install one composed tracing subscriber

**Files:**
- Modify: `crates/ai-brain-cli/src/init.rs`
- Modify: `crates/ai-brain-cli/src/main.rs`

**Step 1: Write the failing local-dispatch tests**

Add tests that do not touch the process-global subscriber:

1. Build a non-TUI dispatch, install it with `tracing::dispatcher::with_default`, emit a unique info event, drop the dispatch, and assert the daily file is non-empty and contains the event.
2. Build a TUI dispatch and assert the same file behavior.
3. Use a path whose `logs` component cannot be created/open and assert initialization returns a Chinese error instead of silently succeeding.

**Step 2: Run RED tests**

```powershell
cargo test -p ai-brain-cli init::tests::logging_ -- --nocapture
```

Expected: no unified/testable dispatch builder exists and current file initializer silently ignores global-install failure.

**Step 3: Implement a unified initializer**

Replace `init_file_logging` and `init_tui_logging` with:

```rust
pub fn build_logging_dispatch(base_dir: &Path, is_tui: bool) -> Result<(tracing::Dispatch, PathBuf), String>
pub fn init_logging(base_dir: &Path, is_tui: bool) -> Result<PathBuf, String>
```

`build_logging_dispatch` must create `logs/brain-YYYY-MM-DD.log` and return:

- TUI: file formatting layer only;
- non-TUI: terminal formatting layer plus the same file layer;
- ANSI disabled for file output;
- environment filter with `info` fallback;
- no ignored result and no sink fallback.

`init_logging` installs the returned dispatch once with `tracing::dispatcher::set_global_default` and returns the path or a Chinese error.

In `main.rs`, after `init_environment`, call `init::init_logging(&base_dir, is_tui)` exactly once. On failure print `初始化日志失败: ...` to stderr and exit non-zero. Remove the direct `.init()` call.

**Step 4: Run GREEN tests and commit**

```powershell
cargo test -p ai-brain-cli init::tests::logging_ -- --nocapture
git add crates/ai-brain-cli/src/init.rs crates/ai-brain-cli/src/main.rs
git commit -m "fix(logging): compose terminal and file subscribers once"
```

### Task 6: Run regression gates and review the branch

**Files:**
- Modify only if a gate exposes a regression; add a failing regression test before each behavioral fix.

**Step 1: Run focused crate suites**

```powershell
cargo test -p novel-workflow
cargo test -p brain-llm
cargo test -p tools
cargo test -p ai-brain-cli
```

Expected: all pass.

**Step 2: Run workspace formatting and lint gates**

From `rust/`:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: exit code 0 for every command. If formatting fails, run `cargo fmt`, inspect the diff, then repeat all three gates.

**Step 3: Inspect the complete diff and commit any verification-only fixes**

```powershell
git status --short
git diff --check
git log --oneline --decorate -8
git diff ea81da9c^..HEAD --stat
```

Confirm there is no provider fallback, no secret/raw-response persistence, no weakened hash validation, no unrelated user-file change, and no placeholders.

**Step 4: Apply `requesting-code-review` and `verification-before-completion`**

Perform an evidence-based self-review against the approved design. Resolve every High/Medium finding with RED/GREEN coverage before proceeding.

### Task 7: Merge, rebuild, restart, and perform one isolated smoke

**Files:**
- No source changes expected.
- Runtime outputs: `rust/target/release/ai-brain.exe`, `C:\Users\16038\.ai-brain\logs\brain-YYYY-MM-DD.log`, redirected Web stdout/stderr files.

**Step 1: Record rollback evidence without modifying data**

Record current branch/HEAD, worktree commits, running PID, executable path, command line, start time, listener owner, current log sizes, and latest Novel task IDs/states. Do not delete or rewrite databases.

**Step 2: Merge the reviewed branch into the approved current branch**

From `D:\rustObject\AI-brain` on `featrue/20260404-nao`, verify the only untracked files are the preserved planning/novel materials, then merge `fix/novel-runtime-reliability` with a normal non-destructive merge. Do not reset, clean, stash, push, or touch unrelated files.

**Step 3: Re-run the full gates in the merged checkout and build release**

From `D:\rustObject\AI-brain\rust`:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p ai-brain-cli
```

Expected: all pass and `target/release/ai-brain.exe` has a new timestamp.

**Step 4: Restart only the verified current service**

Re-resolve the listener owner for `127.0.0.1:8080` immediately before stopping it. Stop only when PID, executable path and command line match the recorded AI Brain Web process. Start the rebuilt executable hidden with explicit `web --addr 127.0.0.1:8080` and redirect stdout/stderr to stable files under `.ai-brain/logs`. Never kill by broad process name.

**Step 5: Verify startup health before paid traffic**

Verify:

- the new PID owns `127.0.0.1:8080`;
- the expected HTTP page/API answers;
- WebSocket handshake succeeds;
- the daily `brain-YYYY-MM-DD.log` becomes non-empty and contains the new startup event;
- redirected stderr has no startup panic/config error.

**Step 6: Run exactly one minimal isolated DeepSeek Novel smoke**

Use a uniquely named temporary Novel project/task and a tiny local context/output path under the workspace. Freeze the actual SHA-256, request a very short draft, and invoke the current explicit DeepSeek route once through the production Novel Writer path. Do not call `decide`, `publish`, or Canon mutation. Do not repeat a failed paid call.

Success criteria:

- one Writer attempt returns `draft_ready` or legitimate `needs_clarification` under the strict contract;
- if the first text is malformed, at most one same-model format repair occurs and actual usage is summed;
- no Gemini/other provider request occurs;
- task events and daily file log contain route/attempt information without secrets or an unbounded full response;
- no production project/task is altered.

**Step 7: Final state report**

Report merged commit(s), full gate outputs, release binary timestamp/hash, old/new PID, HTTP/WS/file-log results, isolated smoke task ID/outcome/call count, and any residual external provider error exactly as returned. Do not claim success without fresh command evidence.

