# Member Mention Handoff Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让实例回复中的显式 `@实例` 原子唤醒目标实例，并为目标持久冻结发送实例回复、原始用户消息和发送实例本轮变更文件。

**Architecture:** `CollaborationRepository::complete_item` 在追加实例回复的同一 SQLite 事务中解析同房间成员显示名、恢复 sleeping/sleep_after_current，并为实际目标创建 Direct Inbox。Claim 从现有事件、对话根事件和 `collaboration_run_changed_files` 派生不可变 `MemberHandoffContext`；runtime 对这类 Claim 创建 `collaboration-task-v5`，将根用户引用和变更文件 Artifact 放入 required context，并在恢复时严格校验。

**Tech Stack:** Rust、rusqlite、Tokio、knowledge-core ContextBuilder、task-engine、Cargo tests。

---

### Task 1: 事务化解析、恢复与 Direct Inbox

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration.rs`

- [ ] **Step 1: 把旧“不唤醒”合同改为关键失败测试**

用一个真实仓储测试建立 A、active B、sleeping C、archived D。用户只唤醒 A，A 完成时回复：

```rust
"@智脑 B @智脑 B 请复核；@智脑 C 请读取文件；@智脑 D 忽略；@智脑 A 自己不处理"
```

断言：

```rust
assert_eq!(reply.recipients, vec![b.clone(), c.clone()]);
assert_eq!(inbox_count(&repository, &reply.event_id, &b), 1);
assert_eq!(inbox_count(&repository, &reply.event_id, &c), 1);
assert_eq!(inbox_count(&repository, &reply.event_id, &d), 0);
assert_eq!(repository.member("room-1", &c).unwrap().availability,
           MemberAvailability::Active);
assert_eq!(repository.member("room-1", &d).unwrap().availability,
           MemberAvailability::Archived);
```

测试同时把 C 先置为 `sleep_after_current` 或另设一个最小断言，确认该状态也被取消为 active；只保留一个测试函数，避免展开排列组合。

- [ ] **Step 2: 运行测试并确认 RED**

Run: `cargo test -p ai-brain-cli member_reply_mentions_wake_active_and_sleeping_members --lib -- --nocapture`

Working directory: `rust/`

Expected: 断言失败，当前回复 `recipients` 为空且 B/C 均无 Inbox。

- [ ] **Step 3: 实现边界匹配和目标选择**

增加私有 helper，输入正文、发送者 ID 和同房间成员，输出按 member_id 稳定去重的目标。匹配规则为：显示名长度降序；`@` 前后必须是正文边界；self/archived 忽略。边界 helper 只把 Unicode 字母、数字、下划线和 `@` 视为名称连续字符，防止邮箱和长名称截断。

在 `append_member_reply_event` 后调用事务内派发 helper：

- 先查询对话根范围内总回复数、目标回复数和 pending 容量。
- active 直接入队；sleeping/sleep_after_current 仅在允许入队时更新为 active、`last_woken_at=now`、`version=version+1`。
- archived/self/超深度/超回复数/超容量跳过。
- 写 `room_event_recipients`、Direct `member_inbox_items` 和 queued `room_event_deliveries`。
- 把实际 member IDs 写回返回的 `RoomEventView.recipients`。

使用 `member-handoff:<reply_event_id>:<member_id>` 作为 Inbox 幂等键，沿用 `(member_id, source_event_id)` 唯一约束。

- [ ] **Step 4: 运行仓储模块确认 GREEN**

Run:

```powershell
cargo test -p ai-brain-cli member_reply_mentions_wake_active_and_sleeping_members --lib -- --nocapture
cargo test -p ai-brain-cli web::collaboration::tests --lib -- --nocapture
```

Working directory: `rust/`

Expected: 关键测试通过；协作仓储模块无回归。

### Task 2: Claim member handoff 与 v5 冻结上下文

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs`

- [ ] **Step 1: 写持久上下文关键失败测试**

构造用户根消息 → A 运行；用 `replace_run_changed_files` 为 A 的 `run_id` 写入一个 added 和一个 modified 绝对路径；A 回复 `@B`。租约 B 后断言 wished-for API：

```rust
let handoff = claim.member_handoff.as_ref().unwrap();
assert_eq!(handoff.source_member.member_id, a);
assert_eq!(handoff.root_user_reference.event_id, root.event.event_id);
assert_eq!(handoff.changed_files, expected_changed_files);

let context = built_context_snapshot_for_claim(&repository, &claim, &config);
let request = task_request_for_claim(&claim, &config, &model, &context);
assert_eq!(request.config_version, "collaboration-task-v5");
assert!(context.blocks.iter().any(|block| {
    block.kind == ContextBlockKind::ConversationReference
        && block.source_ref.as_ref().is_some_and(|source| source.resource_id == root.event.event_id)
}));
assert!(context.blocks.iter().any(|block| {
    block.kind == ContextBlockKind::Artifact
        && expected_changed_files.iter().all(|file| block.content.contains(&file.path))
}));
```

另断言 optional history 不包含 A 在该根消息之前的一条私有回复。

- [ ] **Step 2: 运行测试并确认 RED**

Run: `cargo test -p ai-brain-cli member_handoff_v5_freezes_root_user_and_changed_files --lib -- --nocapture`

Working directory: `rust/`

Expected: 编译失败，`ClaimedInboxItem` 尚无 `member_handoff`，且任务仍为 v4。

- [ ] **Step 3: 从持久数据构建 Claim handoff**

在 `collaboration.rs` 增加可序列化、可比较类型：

```rust
pub struct MemberHandoffSource {
    pub member_id: String,
    pub member_name: String,
    pub event_id: String,
    pub sequence: u64,
    pub content_hash: String,
    pub execution_working_directory: String,
}

pub struct MemberHandoffContext {
    pub source_member: MemberHandoffSource,
    pub root_user_reference: RoomEventReferenceView,
    pub changed_files: Vec<RoomChangedFileView>,
}
```

`claimed_inbox_from_candidate` 在来源事件为 member 时，在当前连接/事务中读取并验证：

- 根事件存在、同房间、sender_kind=user、未失效。
- 来源 member event 的 sender、正文哈希和冻结目录完整。
- `collaboration_run_changed_files` 按 path 升序读取；每个路径必须为绝对路径，change_kind 必须合法。

普通用户来源返回 `None`。`lease_next` 和 `claim_for_reconciliation` 共用同一 builder，确保新租约与重启恢复一致。

- [ ] **Step 4: 构建 v5 required blocks 和任务快照**

在 runtime：

- 增加 `COLLABORATION_TASK_V5`。
- `CurrentInput` 内容仍以 A 完整回复为主，并标注 source member。
- 为 handoff 根用户添加 required `ConversationReference`。
- changed_files 非空时添加 required `Artifact`，每行格式为 `added|modified<TAB>绝对路径`，并绑定来源回复事件的 SourceRef/hash。
- 调整 ContextBudget 的 required item 数。
- `task_request_for_claim` 对有 handoff 的 Claim 写 v5 和 `member_handoff`；普通 Claim仍写 v4。
- v3/v4 读取兼容不变；v5 校验 resolved_config handoff、根引用 block、artifact block、正文 hash、路径排序和绝对性。

- [ ] **Step 5: 运行关键和模块测试确认 GREEN**

Run:

```powershell
cargo test -p ai-brain-cli member_handoff_v5_freezes_root_user_and_changed_files --lib -- --nocapture
cargo test -p ai-brain-cli collaboration_task_v4 --lib -- --nocapture
cargo test -p ai-brain-cli web::collaboration_runtime::tests --lib -- --nocapture
```

Working directory: `rust/`

Expected: 新 v5 测试通过；原 v4 和 runtime 模块保持通过。

### Task 3: durable 恢复与篡改拒绝

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs`

- [ ] **Step 1: 写恢复关键失败测试**

在同一个测试中创建并持久化 B 的 v5 Task，重开 `CollaborationRepository`/`TaskRepository`，从 `claim_for_reconciliation` 重建 Claim：

```rust
let recovered = reopened.claim_for_reconciliation(
    &claim.inbox_item_id,
    &claim.task_run_id,
    &claim.run_id,
).unwrap().unwrap();
assert_eq!(validated_task_context(&task, &recovered).unwrap(), context);
```

随后各复制一次 task 并篡改 `root_user_reference.content`、`changed_files[0].path`，断言 `validated_task_context` 返回错误。只测试这两个关键身份面，不展开每个字段的矩阵。

- [ ] **Step 2: 运行测试并确认 RED**

Run: `cargo test -p ai-brain-cli collaboration_task_v5_reopens_and_rejects_tampered_handoff --lib -- --nocapture`

Working directory: `rust/`

Expected: 新测试在 v5 allowlist/严格 handoff 校验缺失处失败。

- [ ] **Step 3: 完成严格验证与兼容恢复**

让 `validate_task_claim_identity`、`context_snapshot_from_task`、`validated_task_context` 显式支持 v5；v5 先验证 handoff 自身正文哈希和绝对路径，再与恢复 Claim 完整相等，最后比较 required reference/artifact blocks。v3 仍跳过新增字段，v4 保持原 reply-reference 严格校验，未知版本仍拒绝。

- [ ] **Step 4: 运行受影响回归与门禁**

Run:

```powershell
cargo test -p ai-brain-cli collaboration_task_v5 --lib -- --nocapture
cargo test -p ai-brain-cli web::collaboration::tests --lib -- --nocapture
cargo test -p ai-brain-cli web::collaboration_runtime::tests --lib -- --nocapture
cargo fmt --all -- --check
git diff --check
```

Working directory for Cargo: `rust/`; Git command from repository root.

Expected: 定向与模块测试全部通过，格式和 whitespace 检查 exit 0。

- [ ] **Step 5: 提交实例交接变更**

仅暂存两个生产/测试共址 Rust 文件：

```powershell
git add -- rust/crates/ai-brain-cli/src/web/collaboration.rs rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs
git diff --cached --check
git commit -m "feat(web): wake mentioned brain members"
```

