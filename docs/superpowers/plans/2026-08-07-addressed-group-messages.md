# 定向唤醒群消息 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让群聊仅唤醒被显式 `@` 的实例，同时保留顺序公共消息池、隔离的三条用户消息上下文窗口，以及可安全查询群消息的只读工具。

**Architecture:** `room_events` 继续是 SQLite 的追加式公共消息池，`room_event_recipients` 是事件与被唤醒实例的去重关联。新消息只创建 Direct Inbox 项；协作历史查询改为“最近三条用户消息加当前实例自己的回复”。执行时用 Claim 约束的工具执行器包装既有执行器，额外公开 `read_room_messages`，而不改动 ContextSnapshot、压缩或模型运行链。

**Tech Stack:** Rust、rusqlite、Tokio、serde_json、brain-main、brain-core 工具执行器、原生 Web JavaScript、Cargo 测试。

---

## 文件结构

- `rust/crates/ai-brain-cli/src/web/collaboration.rs`：SQLite 迁移、定向入队、消息窗口查询、成员名称约束及仓储测试。
- `rust/crates/ai-brain-cli/src/web/collaboration_tools.rs`：新的 Claim 绑定只读工具执行器及工具单元测试。
- `rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs`：把 Direct Claim 的工具权限、消息池边界和仓储引用传给模型运行。
- `rust/crates/ai-brain-cli/src/orchestrator.rs`、`rust/crates/brain-main/src/main_brain.rs`：允许隔离成员运行替换工具执行器及工具定义。
- `rust/crates/ai-brain-cli/src/web/static/app.js`、`app.test.js`：提及/选择器同步及浏览器外纯函数测试。

### Task 1: 建立仅定向唤醒的仓储契约

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration.rs:5271-5720`

- [ ] **Step 1: 写失败测试：只给明确收件人创建 Direct 项**

~~~rust
#[test]
fn group_message_only_queues_explicit_recipients() {
    let (_dir, repository) = repository();
    let snapshot = ensure(&repository);
    let a = snapshot.room.default_member_id;
    let b = repository.create_member("room-1", "智脑 B", None, None).unwrap().member_id;
    let c = repository.create_member("room-1", "智脑 C", None, None).unwrap().member_id;

    let posted = repository.post_group_message(
        "room-1", &[a.clone(), b.clone()], "@A @B 只唤醒指定实例",
        RoomInputMode::Chat, "addressed-only",
    ).unwrap();

    assert_eq!(posted.event.recipients, vec![a.clone(), b.clone()]);
    assert!(posted.event.audience.is_empty());
    assert_eq!(posted.inbox_items.len(), 2);
    assert!(posted.inbox_items.iter().all(|item| item.purpose == InboxPurpose::Direct));
    assert!(posted.inbox_items.iter().all(|item| item.member_id != c));
    assert!(repository.claim_next().unwrap().is_some());
    assert!(repository.claim_next().unwrap().is_some());
    assert!(repository.claim_next().unwrap().is_none());
}
~~~

- [ ] **Step 2: 验证红灯**

Run: `cargo test -p ai-brain-cli group_message_only_queues_explicit_recipients`

Expected: FAIL；当前实现为非收件人创建 `Participation` 项。

- [ ] **Step 3: 写失败测试：成员回复不再自动唤醒别人**

~~~rust
#[test]
fn direct_member_reply_is_appended_without_waking_other_members() {
    let (_dir, repository) = repository();
    let snapshot = ensure(&repository);
    let a = snapshot.room.default_member_id;
    let b = repository.create_member("room-1", "智脑 B", None, None).unwrap().member_id;
    repository.post_group_message(
        "room-1", std::slice::from_ref(&a), "只问 A",
        RoomInputMode::Chat, "only-a",
    ).unwrap();

    let claim = repository.claim_next().unwrap().unwrap();
    let reply = repository.complete_item(&claim, "A 的答复").unwrap().unwrap();
    assert!(reply.audience.is_empty());
    assert!(reply.recipients.is_empty());
    assert_eq!(repository.claim_next().unwrap(), None);
    assert!(!repository.snapshot("room-1").unwrap().inbox.iter()
        .any(|item| item.member_id == b));
}
~~~

- [ ] **Step 4: 验证红灯并提交测试**

Run: `cargo test -p ai-brain-cli direct_member_reply_is_appended_without_waking_other_members`

Expected: FAIL；`append_member_reply_event` 当前扩散回复。

~~~bash
git add rust/crates/ai-brain-cli/src/web/collaboration.rs
git commit -m "test(web): define addressed-only group delivery"
~~~

### Task 2: 收敛投递与自动辩论链

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration.rs:1804-2139,2597-3146,5271-5720`

- [ ] **Step 1: 只创建 Direct 投递**

在 `post_message_checked_internal` 保留收件人排序、版本验证、`room_event_recipients` 插入。删除 `audience_members` 查询、容量分支以及整个环境参与循环，改为以下唯一写入循环：

~~~rust
let mut inbox_items = Vec::with_capacity(recipient_list.len());
for recipient in &recipient_list {
    let item = insert_inbox_item(
        &transaction, &recipient.member_id, &event_id, thread_key, mode,
        InboxPurpose::Direct, &event_id, recipient.expected_version,
        &format!("{idempotency_key}:{}", recipient.member_id), &now,
    )?;
    insert_delivery(
        &transaction, &event_id, &recipient.member_id, DeliveryKind::Direct,
        DeliveryState::Queued, Some(&item.inbox_item_id), None, &now,
    )?;
    inbox_items.push(item);
}
~~~

`group_enabled` 仍写 `true`，作为群消息窗口历史的标记；事件的 `audience` 固定为空。

- [ ] **Step 2: 关闭成员回复扩散**

将 `append_member_reply_event` 结尾替换为：

~~~rust
let event = RoomEventView {
    // 保留已有事件字段
    recipients: Vec::new(),
    audience: Vec::new(),
    // 其余字段沿用当前插入值
};
Ok(event)
~~~

删除只被新路径使用的 `expand_member_reply_deliveries`、`queue_latest_deferred_participation` 和调用。保留 `complete_participation_item` 及恢复分支，让升级前持久化的参与项仍可收敛。

- [ ] **Step 3: 更新取代旧语义的测试**

移除“环境参与 / 延迟参与 / 自动辩论”断言，改用 Task 1 的两个契约。保留旧 Participation 恢复测试，但 fixture 必须直接构造旧行，不能调用新发帖路径创建它。

- [ ] **Step 4: 运行绿灯并提交**

Run: `cargo test -p ai-brain-cli web::collaboration::tests`

Expected: PASS；新发帖只产生 Direct 项，历史 Participation 仍可完成。

~~~bash
git add rust/crates/ai-brain-cli/src/web/collaboration.rs
git commit -m "feat(web): wake only addressed group members"
~~~

### Task 3: 实现最近三条用户消息的隔离历史投影

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration.rs:2435-2595,5271-5720`
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs:1520-1669,1712-2060`

- [ ] **Step 1: 写失败测试：最近三个用户轮次加自身回复**

~~~rust
#[test]
fn addressed_history_has_three_latest_users_and_own_replies_only() {
    let (_dir, repository) = repository();
    let snapshot = ensure(&repository);
    let a = snapshot.room.default_member_id;
    let b = repository.create_member("room-1", "智脑 B", None, None).unwrap().member_id;
    let claim_a = append_window_fixture_and_claim_a(&repository, &a, &b);
    let history = repository.member_history(&claim_a).unwrap();
    let text = history.iter().map(|message| message.content.as_str())
        .collect::<Vec<_>>().join("\n");

    assert!(!text.contains("U1"));
    assert!(text.contains("U2") && text.contains("U3") && text.contains("U4"));
    assert!(text.contains("A1") && text.contains("A2"));
    assert!(!text.contains("B1"));
    assert!(history.windows(2).all(|pair| pair[0].sequence < pair[1].sequence));
}
~~~

`append_window_fixture_and_claim_a` 用真实的 `post_group_message` 和 `complete_item` 产生 U1/A1/U2/B1/U3/A2/U4，并只将 U4 定向给 A。

- [ ] **Step 2: 验证红灯**

Run: `cargo test -p ai-brain-cli addressed_history_has_three_latest_users_and_own_replies_only`

Expected: FAIL；现有群聊分支按投递记录取历史，非固定三条用户消息窗口。

- [ ] **Step 3: 用 CTE 替换 `claim.group_enabled` 历史查询**

~~~sql
WITH recent_users AS (
    SELECT sequence FROM room_events
    WHERE room_id = ?1 AND sequence <= ?2
      AND sender_kind = 'user' AND kind = 'user_message'
      AND invalidated_at IS NULL
    ORDER BY sequence DESC LIMIT 3
), window_start AS (
    SELECT COALESCE(MIN(sequence), ?2) AS sequence FROM recent_users
)
SELECT e.event_id, e.sequence, e.sender_kind, e.sender_id, e.sender_name, e.content
FROM room_events e, window_start w
WHERE e.room_id = ?1 AND e.sequence >= w.sequence AND e.sequence <= ?2
  AND e.invalidated_at IS NULL
  AND e.kind IN ('user_message', 'member_message')
  AND (e.sender_kind = 'user' OR (e.sender_kind = 'member' AND e.sender_id = ?3))
ORDER BY e.sequence ASC
~~~

用户消息映射为 `user` 并带发送者标签；当前实例消息映射为 `assistant`。移除旧的 DESC/LIMIT/reverse。

- [ ] **Step 4: 证明 ContextSnapshot 链未变**

在 `context_request_for_claim` 测试增加断言：窗口历史成为 `ConversationUser` / `ConversationAssistant` optional blocks；当前输入仍是唯一的 `CurrentInput` required block；`ContextSnapshot::validate()` 成功。

Run: `cargo test -p ai-brain-cli addressed_history_has_three_latest_users_and_own_replies_only context_request_for_claim`

Expected: PASS。

- [ ] **Step 5: 提交**

~~~bash
git add rust/crates/ai-brain-cli/src/web/collaboration.rs rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs
git commit -m "feat(web): scope addressed history to three user turns"
~~~

### Task 4: 增加受 Claim 约束的只读公共消息池工具

**Files:**
- Create: `rust/crates/ai-brain-cli/src/web/collaboration_tools.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/mod.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration.rs:2435-2595`
- Modify: `rust/crates/brain-main/src/main_brain.rs:87-105`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs:1173-1247`
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs:59-65,882-895`

- [ ] **Step 1: 写失败的仓储分页测试**

~~~rust
#[test]
fn read_room_messages_stops_at_claim_context_boundary() {
    let (_dir, repository) = repository();
    let claim = append_messages_and_claim(&repository, 4);
    let page = repository.read_room_messages(&claim, 0, 50, None).unwrap();
    assert!(page.events.iter().all(|event| event.sequence <= claim.context_through_seq));
    assert!(page.events.windows(2).all(|pair| pair[0].sequence < pair[1].sequence));
    assert_eq!(page.events.len(), 4);
}
~~~

- [ ] **Step 2: 实现仓储接口**

添加 `MAX_ROOM_MESSAGE_READ_LIMIT: usize = 100`、`RoomMessagePage`、`RoomMessageRecord` 与：

~~~rust
pub fn read_room_messages(
    &self, claim: &ClaimedInboxItem, after_sequence: u64,
    limit: usize, sender_id: Option<&str>,
) -> Result<RoomMessagePage>
~~~

拒绝 `limit == 0 || limit > 100`。查询强制绑定 `claim.room_id` 和 `claim.context_through_seq`：`sequence > after_sequence AND sequence <= claim.context_through_seq`、非失效、用户/成员消息、升序；可选 `sender_id` 只过滤发送者，调用输入不得提供 room 或边界。

- [ ] **Step 3: 写失败的工具边界测试**

~~~rust
#[tokio::test]
async fn room_reader_never_trusts_room_or_boundary_from_tool_input() {
    let result = executor.execute(&ToolCall {
        tool_name: "read_room_messages".into(),
        input: serde_json::json!({
            "room_id":"other-room", "after_sequence":0, "limit":101
        }),
        validated: false, validation_id: None,
    }).await;
    assert!(result.is_error);
    assert!(result.output.contains("limit"));
}
~~~

- [ ] **Step 4: 实现工具包装器和运行注入**

`ScopedCollaborationToolExecutor` 持有内层 `Arc<dyn ToolExecutor>`、仓储和 `ClaimedInboxItem`。其 `list_tools` 追加：

~~~rust
ToolDescriptor {
    name: "read_room_messages".into(),
    description: "只读查询本次任务开始前在当前群可见的消息。".into(),
    input_schema: serde_json::json!({
        "type":"object",
        "properties":{
            "after_sequence":{"type":"integer","minimum":0},
            "limit":{"type":"integer","minimum":1,"maximum":100},
            "sender_id":{"type":"string"}
        },
        "additionalProperties":false
    }),
}
~~~

`execute` 只截获该工具名并严格反序列化；其它工具委托给内层执行器。向 `MainBrain` 添加 `fork_isolated_with_llm_and_tools(llm, executor, definitions, max_tokens, temperature)`；旧 fork 保持并转调新接口。让 `Orchestrator::query_member_streaming_scoped` 接收请求专属 executor/definitions；`CollaborationRuntime::run_claim` 只为 Direct Claim 构造该包装器，历史 Participation 项仍禁用工具。

- [ ] **Step 5: 验证并提交**

Run: `cargo test -p ai-brain-cli read_room_messages scoped_collaboration_tool`  
Run: `cargo test -p brain-main fork_isolated`

Expected: PASS；工具无副作用、无法越界、既有 fork 仍通过。

~~~bash
git add rust/crates/ai-brain-cli/src/web/collaboration.rs rust/crates/ai-brain-cli/src/web/collaboration_tools.rs rust/crates/ai-brain-cli/src/web/mod.rs rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs rust/crates/ai-brain-cli/src/orchestrator.rs rust/crates/brain-main/src/main_brain.rs
git commit -m "feat(web): add bounded room message reader"
~~~

### Task 5: 同步实例选择器与文本提及

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration.rs:1285-1493,5271-5720`
- Modify: `rust/crates/ai-brain-cli/src/web/static/app.js:575-615,1020-1048,2898-2902`
- Create: `rust/crates/ai-brain-cli/src/web/static/app.test.js`

- [ ] **Step 1: 写失败的唯一名称测试**

~~~rust
#[test]
fn active_member_display_names_are_unique_within_a_room() {
    let (_dir, repository) = repository();
    ensure(&repository);
    repository.create_member("room-1", "分析师", None, None).unwrap();
    assert!(matches!(
        repository.create_member("room-1", "分析师", None, None),
        Err(CollaborationError::DuplicateMemberName { .. })
    ));
}
~~~

- [ ] **Step 2: 迁移与错误映射**

新增 `DuplicateMemberName`，将 `SCHEMA_VERSION` 从 5 升至 6。在 5→6 迁移先探测活跃重复名称并报可识别配置错误，然后创建：

~~~sql
CREATE UNIQUE INDEX IF NOT EXISTS brain_members_active_display_name_idx
ON brain_members(room_id, display_name)
WHERE availability != 'archived';
~~~

在 `create_member_as` 和 `configure_member_as` 事务中预检并将 SQLite unique 违例映射为领域错误。

- [ ] **Step 3: 写前端纯函数红灯测试**

~~~js
test('normalizes repeated mentions by member id', () => {
  const members = [{ member_id: 'a', display_name: 'A' }, { member_id: 'b', display_name: 'B' }];
  assert.deepEqual(mentionedMemberIds('@A @A 请和 @B @A 讨论', members), ['a', 'b']);
});
test('selection changes only its own marker', () => {
  assert.equal(setMentionSelected('请分析 @B', 'A', true), '请分析 @B @A');
  assert.equal(setMentionSelected('请分析 @A @B', 'A', false), '请分析 @B');
});
~~~

- [ ] **Step 4: 实现并连入 DOM**

在 `app.js` 提取 `mentionedMemberIds`、`setMentionSelected`、`normalizeMentionRecipients`。提及匹配必须转义完整显示名，返回首次出现的 member ID。选择器调用 `setMentionSelected` 写入/移除文本；输入事件解析文本并同步 `selectedMemberIds`；提交前规范化文本，然后从 member ID Set 生成唯一 `MemberAddress`。以 `module.exports` 条件导出纯函数供 Node 测试，浏览器仍走原全局逻辑。

- [ ] **Step 5: 运行并提交**

Run: `node --test rust/crates/ai-brain-cli/src/web/static/app.test.js`  
Run: `cargo test -p ai-brain-cli active_member_display_names_are_unique_within_a_room`

Expected: PASS；重复提及只保留一个 ID，选择器/文本双向同步，活跃重名被拒绝。

~~~bash
git add rust/crates/ai-brain-cli/src/web/collaboration.rs rust/crates/ai-brain-cli/src/web/static/app.js rust/crates/ai-brain-cli/src/web/static/app.test.js
git commit -m "feat(web): synchronize addressed member mentions"
~~~

### Task 6: 文档和全量验收

**Files:**
- Modify: `docs/superpowers/specs/2026-08-07-addressed-group-messages-design.md`
- Modify: `rust/findings.md`
- Modify: `rust/progress.md`

- [ ] **Step 1: 更新架构表述**

在已批准设计的“数据模型”“实例回复”和“失败与兼容性”章节追加实现后的迁移事实：历史 Participation 行仅为恢复兼容而保留，新发帖不创建该类行。向 `rust/findings.md` 与 `rust/progress.md` 各追加一条带日期的实现和验证记录；不改写两文件中的历史 Phase 4.5 记录。

- [ ] **Step 2: 格式化、静态检查与全量测试**

Run: `cargo fmt --check`  
Expected: PASS.

Run: `cargo clippy --workspace --all-targets -- -D warnings`  
Expected: PASS；如有历史警告，记录准确包/行号，不能降低 lint 级别。

Run: `cargo test --workspace`  
Expected: PASS.

Run: `node --test rust/crates/ai-brain-cli/src/web/static/app.test.js`  
Expected: PASS.

- [ ] **Step 3: 人工 Web 验收**

启动本地服务，创建 A/B/C，发送 `@A @A @B`：A/B 各一个 Direct 项，C 不入队。A 回复后 C 仍不运行。再 @A，确认默认上下文是最近三条用户消息与 A 回复；调用 `read_room_messages` 后可查看 B 的较早消息，不能查看本 Claim 上界之后的事件。

- [ ] **Step 4: 提交验收文档**

~~~bash
git add docs/superpowers/specs/2026-08-07-addressed-group-messages-design.md rust/findings.md rust/progress.md
git commit -m "docs(web): record addressed group messaging migration"
~~~
