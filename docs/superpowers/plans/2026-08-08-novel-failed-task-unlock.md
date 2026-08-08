# Novel Failed Task Unlock Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 `novel_task` 增加受 Task Engine 终态和 Novel 领域产物保护的 `unlock_failed` 操作，并用它安全释放当前遗留锁。

**Architecture:** `NovelStartWorkflow` 只读查询 `novel-task-{task_id}` 的 Task Engine 状态，`NovelApplicationService` 在 task lock 内执行状态核验、领域转换和 checkpoint/event 原子持久化。CLI facade 严格校验输入并暴露结构化结果，工具 schema 与内置小说工作流技能同时写明故障识别和调用顺序。

**Tech Stack:** Rust 2021、Tokio、Serde/serde_json、rusqlite、现有 Task Engine、Cargo tests。

---

## 文件结构

- 修改 `rust/crates/novel-domain/src/state.rs`：加入无产物失败任务的受保护终结转换。
- 修改 `rust/crates/novel-domain/tests/domain_contract.rs`：覆盖允许、拒绝和终态行为。
- 修改 `rust/crates/novel-workflow/src/start.rs`：加入 Task Engine 运行状态只读查询和稳定的公开状态枚举。
- 修改 `rust/crates/novel-workflow/tests/start_workflow_contract.rs`：覆盖全部 Task Engine 状态和未知运行记录。
- 修改 `rust/crates/novel-application/src/application.rs`：加入解锁 receipt、应用用例和 port 方法。
- 修改 `rust/crates/novel-application/src/lib.rs`：导出 receipt。
- 修改 `rust/crates/novel-application/tests/application_contract.rs`：覆盖原子审计、活动锁释放、拒绝和幂等。
- 修改 `rust/crates/tools/src/lib.rs`：加入 `unlock_failed` schema、详细说明和 schema 合同测试。
- 修改 `rust/crates/ai-brain-cli/src/real_tool_executor.rs`：加入严格输入 DTO、校验、dispatch 和 facade 测试。
- 修改 `rust/crates/brain-main/skills/novel-writing-workflow/SKILL.md`：教主脑识别遗留锁并只在安全条件满足时解锁。

### Task 1: Novel Domain 失败解锁转换

**Files:**
- Modify: `rust/crates/novel-domain/src/state.rs`
- Test: `rust/crates/novel-domain/tests/domain_contract.rs`

- [ ] **Step 1: 写失败测试，证明仅空白初始 drafting checkpoint 可以解锁**

在 `domain_contract.rs` 添加：

```rust
#[test]
fn failed_execution_unlock_cancels_empty_drafting_task() {
    let mut state = NovelTaskState::new(task_request()).unwrap();
    state.begin_drafting().unwrap();

    state.unlock_failed_execution().unwrap();

    assert_eq!(state.phase, NovelTaskPhase::Cancelled);
    assert!(state.phase.is_terminal());
}

#[test]
fn failed_execution_unlock_rejects_task_with_draft() {
    let mut state = draft_ready_state();
    let error = state.unlock_failed_execution().unwrap_err();

    assert!(error.to_string().contains("已有草稿或候选版本"));
    assert_eq!(state.phase, NovelTaskPhase::AwaitingMainReview);
}
```

- [ ] **Step 2: 运行测试并确认 RED**

Run: `cargo test -p novel-domain --test domain_contract failed_execution_unlock -- --nocapture`

Expected: FAIL，提示 `NovelTaskState` 不存在 `unlock_failed_execution`。

- [ ] **Step 3: 实现最小领域转换**

在 `NovelTaskState` 中添加：

```rust
pub fn unlock_failed_execution(&mut self) -> Result<()> {
    if self.phase.is_terminal() {
        return Err(NovelDomainError::InvalidTransition(format!(
            "任务 {} 已是终态，不能执行失败解锁",
            self.request.task_id
        )));
    }
    if matches!(
        self.phase,
        NovelTaskPhase::PublicationPending | NovelTaskPhase::ArtifactSavedMemoryPending
    ) || self.publication_id.is_some()
        || self.artifact.is_some()
        || self.commit_report.is_some()
    {
        return Err(NovelDomainError::InvalidTransition(
            "任务处于发布流程或已有发布产物，拒绝解锁".into(),
        ));
    }
    if self.draft_version != 0
        || self.draft.is_some()
        || self.candidate.is_some()
        || !self.candidate_reviews.is_empty()
        || self.main_review.is_some()
        || self.user_decision.is_some()
    {
        return Err(NovelDomainError::InvalidTransition(
            "任务已有草稿或候选版本，拒绝解锁；请继续 review/decide/publish 流程".into(),
        ));
    }
    self.phase = NovelTaskPhase::Cancelled;
    self.updated_at = now_millis();
    Ok(())
}
```

- [ ] **Step 4: 补齐表驱动拒绝测试**

```rust
for mut state in states_with_draft_review_decision_or_publication() {
    let before = state.checkpoint().unwrap();
    assert!(state.unlock_failed_execution().is_err());
    assert_eq!(state.checkpoint().unwrap(), before);
}
for phase in [
    NovelTaskPhase::Completed,
    NovelTaskPhase::Rejected,
    NovelTaskPhase::Cancelled,
    NovelTaskPhase::Failed,
] {
    let mut state = empty_state_at_phase(phase);
    let before = state.checkpoint().unwrap();
    assert!(state.unlock_failed_execution().is_err());
    assert_eq!(state.checkpoint().unwrap(), before);
}
```

测试 helper 使用现有公开状态转换构造 `NeedsClarification`、`AwaitingMainReview`、`AwaitingUserDecision`、`ApprovedForPublication`、`PublicationPending`、`ArtifactSavedMemoryPending`；若某个中间态没有公开构造入口，在 `state.rs` 的同模块单元测试中直接构造，不能为测试新增生产状态后门。

- [ ] **Step 5: 运行 Domain 测试并确认 GREEN**

Run: `cargo test -p novel-domain --test domain_contract`

Expected: PASS。

- [ ] **Step 6: 提交 Domain 变更**

```powershell
git add rust/crates/novel-domain/src/state.rs rust/crates/novel-domain/tests/domain_contract.rs
git commit -m "feat(novel): guard failed task unlock transition"
```

### Task 2: Novel Workflow 查询 Task Engine 状态

**Files:**
- Modify: `rust/crates/novel-workflow/src/start.rs`
- Test: `rust/crates/novel-workflow/tests/start_workflow_contract.rs`

- [ ] **Step 1: 写状态查询失败测试**

在 workflow 合同测试中用现有 repository fixture 创建 `novel-task-task-1`，分别推进到各状态，并断言：

```rust
assert_eq!(
    service.task_execution_state("task-1").await.unwrap(),
    Some(NovelTaskExecutionState::Failed)
);
assert_eq!(
    service.task_execution_state("missing-task").await.unwrap(),
    None
);
```

表驱动覆盖 `Queued`、`Running`、`PausedBudget`、`NeedsInput`、`Completed`、`Failed`、`Cancelled`。

- [ ] **Step 2: 运行测试并确认 RED**

Run: `cargo test -p novel-workflow --test start_workflow_contract task_execution_state -- --nocapture`

Expected: FAIL，提示缺少公开枚举或方法。

- [ ] **Step 3: 添加稳定的 workflow 状态枚举**

在 `start.rs` 定义；现有 `lib.rs` 的 `pub use start::*` 会自动导出：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelTaskExecutionState {
    Queued,
    Running,
    PausedBudget,
    NeedsInput,
    Completed,
    Failed,
    Cancelled,
}
```

实现 `From<TaskRunState>`，逐一显式映射七个 variant，避免应用层直接依赖 Task Engine 类型。

- [ ] **Step 4: 添加异步只读查询方法**

```rust
pub async fn task_execution_state(
    &self,
    task_id: &str,
) -> Result<Option<NovelTaskExecutionState>, NovelWorkflowPortError> {
    if task_id.trim().is_empty() {
        return Err(NovelWorkflowPortError::InvalidRequest(
            "task id is required for execution state lookup".into(),
        ));
    }
    Ok(load_task_optional(Arc::clone(&self.repository), &task_run_id(task_id))
        .await?
        .map(|task| task.state.into()))
}
```

- [ ] **Step 5: 运行 Workflow 测试并确认 GREEN**

Run: `cargo test -p novel-workflow --test start_workflow_contract`

Expected: PASS。

- [ ] **Step 6: 提交 Workflow 变更**

```powershell
git add rust/crates/novel-workflow/src/start.rs rust/crates/novel-workflow/tests/start_workflow_contract.rs
git commit -m "feat(novel): expose writer execution state"
```

### Task 3: Application 原子解锁用例

**Files:**
- Modify: `rust/crates/novel-application/src/application.rs`
- Modify: `rust/crates/novel-application/src/lib.rs`
- Test: `rust/crates/novel-application/tests/application_contract.rs`

- [ ] **Step 1: 写成功解锁和审计失败测试**

用临时 `novel.db`、临时 Task Engine repository 和不应被调用的 Writer 构建完整 service。保存空 `Drafting` checkpoint，把 `novel-task-task-1` 推进为 Failed，然后断言：

```rust
let receipt = service
    .unlock_failed_task("task-1", "Writer 输出合同解析失败")
    .await
    .unwrap();
assert_eq!(receipt.previous_phase, NovelTaskPhase::Drafting);
assert_eq!(receipt.phase, NovelTaskPhase::Cancelled);
assert_eq!(receipt.execution_state, NovelTaskExecutionState::Failed);
assert!(!receipt.already_unlocked);
assert!(store.active_checkpoint_for_project("project-1").unwrap().is_none());
let events = store.load_task_events("task-1").unwrap();
assert_eq!(events.last().unwrap().summary, "manual_unlock");
assert_eq!(events.last().unwrap().details["reason"], "Writer 输出合同解析失败");
assert_eq!(writer.calls.load(Ordering::SeqCst), 0);
```

- [ ] **Step 2: 运行测试并确认 RED**

Run: `cargo test -p novel-application --test application_contract unlock_failed -- --nocapture`

Expected: FAIL，提示缺少 receipt、port 方法和 service 方法。

- [ ] **Step 3: 添加 receipt 和 port 契约**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NovelTaskUnlockReceipt {
    pub task_id: String,
    pub project_id: String,
    pub previous_phase: NovelTaskPhase,
    pub phase: NovelTaskPhase,
    pub execution_state: NovelTaskExecutionState,
    pub already_unlocked: bool,
    pub reason: String,
}
```

在 `TaskApplicationPort` 增加：

```rust
async fn unlock_failed_task(
    &self,
    task_id: &str,
    reason: &str,
) -> Result<NovelTaskUnlockReceipt>;
```

并从 `novel-application/src/lib.rs` 导出 receipt。

- [ ] **Step 4: 添加 reason 防御性校验**

```rust
fn validate_unlock_reason(reason: &str) -> Result<String> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(NovelApplicationError::Conflict("解锁原因不能为空".into()));
    }
    if reason.chars().count() > 256 {
        return Err(NovelApplicationError::Conflict(
            "解锁原因不能超过 256 个字符".into(),
        ));
    }
    if reason.chars().any(char::is_control) {
        return Err(NovelApplicationError::Conflict(
            "解锁原因不能包含控制字符".into(),
        ));
    }
    Ok(reason.to_owned())
}
```

- [ ] **Step 5: 实现受锁保护的应用用例**

核心实现：

```rust
pub async fn unlock_failed_task(
    &self,
    task_id: &str,
    reason: &str,
) -> Result<NovelTaskUnlockReceipt> {
    let reason = validate_unlock_reason(reason)?;
    let task_lock = self.task_lock(task_id);
    let _guard = task_lock.lock().await;
    let mut state = self.load_state(task_id)?;
    let workflow = self.workflow()?;
    let execution_state = workflow
        .task_execution_state(task_id)
        .await?
        .ok_or_else(|| NovelApplicationError::NotFound(format!(
            "Task Engine 运行记录 novel-task-{task_id}"
        )))?;
    if !matches!(
        execution_state,
        NovelTaskExecutionState::Failed | NovelTaskExecutionState::Cancelled
    ) {
        return Err(NovelApplicationError::Conflict(format!(
            "Task Engine 任务仍为 {execution_state:?}，拒绝失败解锁"
        )));
    }
    let previous_phase = state.phase;
    if state.phase == NovelTaskPhase::Cancelled {
        return Ok(unlock_receipt(
            &state,
            previous_phase,
            execution_state,
            true,
            reason,
        ));
    }
    state.unlock_failed_execution()?;
    self.persist(
        &state,
        NovelLifecycleActor::System,
        "manual_unlock",
        "manual_unlock",
        serde_json::json!({
            "reason": reason,
            "previous_phase": previous_phase,
            "execution_state": execution_state,
        }),
    )?;
    Ok(unlock_receipt(
        &state,
        previous_phase,
        execution_state,
        false,
        reason,
    ))
}
```

`unlock_receipt` 只复制已验证字段。Task Engine 为其余状态时返回中文 `NovelApplicationError::Conflict`；记录不存在返回上面的 NotFound。因为 domain 仅允许 `draft_version=0` 且无任何产物，合法解锁只对应初次执行 `novel-task-{task_id}`，不会把失败的修订轮次误判为可丢弃任务。

- [ ] **Step 6: 补齐拒绝、幂等和原子性测试**

```rust
for execution_state in unsafe_execution_states() {
    let fixture = unlock_fixture(execution_state);
    let before = fixture.store.load_checkpoint("task-1").unwrap().unwrap();
    let event_count = fixture.store.load_task_events("task-1").unwrap().len();
    assert!(fixture.service.unlock_failed_task("task-1", "恢复测试").await.is_err());
    assert_eq!(fixture.store.load_checkpoint("task-1").unwrap().unwrap(), before);
    assert_eq!(fixture.store.load_task_events("task-1").unwrap().len(), event_count);
}

let second = fixture
    .service
    .unlock_failed_task("task-1", "重复调用")
    .await
    .unwrap();
assert!(second.already_unlocked);
assert_eq!(manual_unlock_events(&fixture.store, "task-1").len(), 1);
```

- [ ] **Step 7: 运行 Application 测试并确认 GREEN**

Run: `cargo test -p novel-application --test application_contract`

Expected: PASS。

- [ ] **Step 8: 提交 Application 变更**

```powershell
git add rust/crates/novel-application/src/application.rs rust/crates/novel-application/src/lib.rs rust/crates/novel-application/tests/application_contract.rs
git commit -m "feat(novel): add audited failed task unlock"
```

### Task 4: `novel_task.unlock_failed` schema 和执行器

**Files:**
- Modify: `rust/crates/tools/src/lib.rs`
- Modify: `rust/crates/ai-brain-cli/src/real_tool_executor.rs`

- [ ] **Step 1: 写精确 schema 和说明测试**

扩展 `novel_workflow_tools_replace_ephemeral_agent_schema`，断言 description 包含：

```rust
for guidance in [
    "project already has active work",
    "先调用 status",
    "Task Engine 为 failed 或 cancelled",
    "unlock_failed 不调用 Writer/LLM",
    "另行显式调用 start",
] {
    assert!(task.description.contains(guidance));
}
```

查找 `unlock_failed` oneOf branch，断言 properties 仅有 `action/task_id/reason`，required 完整，`additionalProperties=false`，`reason.minLength=1`、`reason.maxLength=256`。

- [ ] **Step 2: 写 executor RED 测试**

扩展 `RecordingTaskApplication` 记录解锁调用，并新增测试：合法输入只调用一次 `unlock_failed_task`，不调用 `associate_conversation_source`；空白、257 字符、换行、额外字段均在调用 port 前失败。

- [ ] **Step 3: 运行 schema 和 executor 测试并确认 RED**

Run: `cargo test -p tools novel_workflow_tools_replace_ephemeral_agent_schema -- --nocapture`

Run: `cargo test -p ai-brain-cli real_tool_executor::tests::novel_unlock_failed -- --nocapture`

Expected: FAIL，分别提示 schema branch 和 dispatch 不存在。

- [ ] **Step 4: 添加 schema 和详细工具说明**

新增 oneOf branch：

```rust
{
    "type": "object",
    "properties": {
        "action": { "const": "unlock_failed" },
        "task_id": { "type": "string", "minLength": 1 },
        "reason": {
            "type": "string",
            "minLength": 1,
            "maxLength": 256,
            "description": "人工解锁原因；不得包含换行等控制字符。"
        }
    },
    "required": ["action", "task_id", "reason"],
    "additionalProperties": false
}
```

description 明确锁的用途、识别顺序、允许状态、拒绝状态、不会调用 LLM、成功后显式 start。

- [ ] **Step 5: 添加严格 DTO、校验和 dispatch**

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NovelUnlockFailedToolInput {
    task_id: String,
    reason: String,
}
```

在 `validate_novel_action_input` 对 `unlock_failed` 先做 required string，再拒绝超过 256 字符或控制字符，最后用 DTO 反序列化以拒绝额外字段。dispatch 调用 `application.unlock_failed_task(&input.task_id, &input.reason)`。会话来源关联条件改为仅匹配 `resume | review | decide | publish`，避免解锁操作写入虚假的创作来源。

输入 helper 使用：

```rust
fn validate_unlock_reason_input(reason: &str) -> Result<(), String> {
    if reason.chars().count() > 256 {
        return Err("novel_task unlock_failed 的 reason 不能超过 256 个字符".into());
    }
    if reason.chars().any(char::is_control) {
        return Err("novel_task unlock_failed 的 reason 不能包含控制字符".into());
    }
    Ok(())
}
```

- [ ] **Step 6: 运行工具测试并确认 GREEN**

Run: `cargo test -p tools novel_workflow_tools_replace_ephemeral_agent_schema`

Run: `cargo test -p ai-brain-cli real_tool_executor::tests::novel_unlock_failed -- --nocapture`

Expected: PASS。

- [ ] **Step 7: 提交工具变更**

```powershell
git add rust/crates/tools/src/lib.rs rust/crates/ai-brain-cli/src/real_tool_executor.rs
git commit -m "feat(tools): expose safe novel task unlock"
```

### Task 5: 更新主脑小说工作流说明

**Files:**
- Modify: `rust/crates/brain-main/skills/novel-writing-workflow/SKILL.md`
- Test: `rust/crates/tools/src/lib.rs`

- [ ] **Step 1: 写内置 skill 内容合同测试**

在 tools inline tests 中读取 `include_str!("../../brain-main/skills/novel-writing-workflow/SKILL.md")`，断言包含 `unlock_failed`、`project already has active work`、`failed`、`cancelled`、`不得自动重试 LLM` 和 `显式调用 start`。

- [ ] **Step 2: 运行测试并确认 RED**

Run: `cargo test -p tools novel_workflow_skill_documents_failed_unlock -- --nocapture`

Expected: FAIL，说明内置 skill 尚未描述解锁操作。

- [ ] **Step 3: 更新 skill 工作流**

把入口更新为 `novel_task(action=start|resume|review|decide|publish|status|unlock_failed)`，并加入：

```markdown
### 失败锁恢复

当 `start` 返回 `project already has active work` 时，先调用 `status` 找到旧 task，并检查最新工作流错误。只有 Task Engine 已为 `failed` 或 `cancelled`、旧 checkpoint 仍为非终态且没有草稿、候选、审核决定或发布物时，才调用 `novel_task(action=unlock_failed)`，提交准确 `task_id` 和简短 reason。

`unlock_failed` 不调用 Writer/LLM，也不删除历史。若工具因 running、paused_budget、needs_input、completed、未知状态或已有产物而拒绝，直接向用户说明，禁止循环解锁或自动重试 LLM。解锁成功后，仅在用户仍要求继续创作时另行显式调用 `start`。
```

- [ ] **Step 4: 运行测试并确认 GREEN**

Run: `cargo test -p tools novel_workflow_skill_documents_failed_unlock`

Expected: PASS。

- [ ] **Step 5: 提交 skill 变更**

```powershell
git add rust/crates/brain-main/skills/novel-writing-workflow/SKILL.md rust/crates/tools/src/lib.rs
git commit -m "docs(novel): teach main brain failed lock recovery"
```

### Task 6: 全面验证、部署和安全释放现有锁

**Files:**
- Verify only; no new production source files.

- [ ] **Step 1: 运行格式和定向测试**

Run:

```powershell
cargo fmt --check
cargo test -p novel-domain -p novel-workflow -p novel-application -p tools
cargo test -p ai-brain-cli real_tool_executor::tests
```

Expected: 全部 PASS。

- [ ] **Step 2: 运行仓库要求的完整验证**

Run:

```powershell
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: 新增代码无 warning，新增/相关测试 PASS；如仍命中已知历史基线失败，记录精确命令、首个失败位置并证明与本次 diff 无关。

- [ ] **Step 3: 构建 Release 并记录哈希**

Run:

```powershell
cargo build --release -p ai-brain-cli
Get-FileHash target/release/ai-brain.exe -Algorithm SHA256
```

Expected: build 成功并输出新 SHA-256。

- [ ] **Step 4: 精确停止旧智脑进程并记录操作前状态**

先只读确认 PID、命令行和监听端口精确指向 `D:\rustObject\AI-brain\rust\target\release\ai-brain.exe web --addr 127.0.0.1:8080`，记录旧 checkpoint、Task Engine state、`manual_unlock` 事件数和 LLM usage/log 尾部，然后只停止该单一 PID。不得终止其他进程。

- [ ] **Step 5: 通过正式工具 facade 处理生产遗留任务**

在 `real_tool_executor.rs` 的 tests 模块临时加入 ignored operator test：用不初始化/调用 Writer 的 fixture 构建 `NovelApplicationService`，指向现有 Novel DB 与 Task Engine DB，并调用同一个 `execute_application_novel_tool`：

```rust
let output = execute_application_novel_tool(
    &service,
    "novel_task",
    json!({
        "action": "unlock_failed",
        "task_id": "wupo-guize-ch1-body-001",
        "reason": "Writer 旧版输出缺少 content，Task Engine 已失败但 Novel checkpoint 仍停留在 drafting"
    }),
)
.await
.unwrap();
assert!(output.contains("\"phase\": \"cancelled\""));
assert_eq!(writer.calls.load(Ordering::SeqCst), 0);
```

用精确测试名和显式环境变量执行一次。该 harness 必须经过正式 DTO 校验、dispatch、Application service、Domain 转换和原子持久化，不直接执行 SQL。运行后立即用 `apply_patch` 从工作树移除临时测试；生产路径和任务 ID不得提交到源代码。

- [ ] **Step 6: 启动新 Release 并只读验证审计、锁和 LLM 调用数**

启动新 Release 并验证 `http://127.0.0.1:8080/` 返回 200。读取 Novel DB，确认 checkpoint `phase=cancelled`、`terminal=1`、`archived=0`，最后一条相关事件为 `manual_unlock`，project `wupo-guize` 无 active checkpoint。读取 Task Engine DB，确认旧运行仍为 Failed；比较操作前后日志/usage 记录，确认没有新增 Writer/LLM 调用。不得调用 `novel_task.start`。

- [ ] **Step 7: 检查最终 diff 并提交收尾**

Run:

```powershell
git diff --check
git status --short
```

只暂存本计划涉及文件，保留用户的小说上下文、大纲和既有未跟踪文件。若格式化产生收尾变更：

```powershell
git add rust/crates/novel-domain rust/crates/novel-workflow rust/crates/novel-application rust/crates/tools/src/lib.rs rust/crates/ai-brain-cli/src/real_tool_executor.rs rust/crates/brain-main/skills/novel-writing-workflow/SKILL.md
git commit -m "fix(novel): finalize failed task unlock"
```
