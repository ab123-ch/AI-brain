# 群会话目录与快照终审加固 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复目录成功误判、旧快照回滚和目录权限预检缺口，使完整功能分支达到最终审查可合并状态。

**Architecture:** 目录更新使用当前连接专属的 command ACK，而不是前端比较 canonical 字符串；协作房间新增独立 `state_revision` 排序完整快照；目录设置与执行前复用无副作用的可用性/UTF-8 校验。测试只覆盖四个关键合同，不增加重复边界矩阵。

**Tech Stack:** Rust、Tokio、Axum WebSocket、rusqlite、原生 JavaScript、Node test runner。

---

### Task 1: 目录命令 ACK 与精确结算

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/progress_adapter.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/ws_handler.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/static/room_reply.js`
- Modify: `rust/crates/ai-brain-cli/src/web/static/room_reply.test.js`
- Modify: `rust/crates/ai-brain-cli/src/web/static/app.js`

- [ ] **Step 1: 写两个关键失败测试**

Rust handler 测试使用真实 repository，以相对 startup 路径更新目录，要求成功结果为当前连接事件：

```rust
assert!(matches!(events.as_slice(), [
    WebProgressEvent::RoomWorkingDirectoryAccepted {
        room_id,
        command_id,
        snapshot,
    }
] if room_id == "active-room"
    && command_id == "directory-command-1"
    && Path::new(&snapshot.room.working_directory) == canonical_target));
```

Node reducer 测试要求相同 room/command 的 accepted 才结算；相对请求值与 canonical snapshot 不再参与匹配。

- [ ] **Step 2: 运行 RED**

```bash
cd rust
cargo test -p ai-brain-cli room_working_directory_websocket -- --nocapture
cd crates/ai-brain-cli/src/web/static
node --test room_reply.test.js
```

Expected: Rust 缺少 `command_id`/accepted variant；Node 仍由 snapshot 路径比较结算。

- [ ] **Step 3: 实现最小协议**

`ClientMessage::UpdateRoomWorkingDirectory` 增加 `#[serde(default)] command_id: String`。空 ID 仅为旧客户端生成服务端 ID。成功分支返回：

```rust
WebProgressEvent::RoomWorkingDirectoryAccepted {
    room_id: active_room_id,
    command_id,
    snapshot,
}
```

`RoomOperationErrorIdentity::Directory` 改用 `room_id + command_id`；错误 JSON 同样携带这两个字段。

前端提交时生成 command ID 并放入 pending/payload。`settleRoomOperation` 的 directory 分支只识别 `room_working_directory_accepted`；匹配后清 pending、关闭 modal，并调用 `applyRoomSnapshot(event.snapshot)`。删除 `matchesPendingRoomDirectorySnapshot` 及路径字符串猜测。

- [ ] **Step 4: 运行 GREEN 与回归**

```bash
cd rust
cargo test -p ai-brain-cli room_working_directory_websocket -- --nocapture
cargo test -p ai-brain-cli web::ws_handler::tests -- --nocapture
cd crates/ai-brain-cli/src/web/static
node --test
```

Expected: 关键测试与现有 WebSocket/Node 测试全部 PASS。

- [ ] **Step 5: 提交 Task 1**

```bash
git add rust/crates/ai-brain-cli/src/web/progress_adapter.rs \
        rust/crates/ai-brain-cli/src/web/ws_handler.rs \
        rust/crates/ai-brain-cli/src/web/static/app.js \
        rust/crates/ai-brain-cli/src/web/static/room_reply.js \
        rust/crates/ai-brain-cli/src/web/static/room_reply.test.js
git commit -m "fix(web): acknowledge canonical room directories"
```

### Task 2: 完整快照单调 revision

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/static/room_reply.js`
- Modify: `rust/crates/ai-brain-cli/src/web/static/room_reply.test.js`
- Modify: `rust/crates/ai-brain-cli/src/web/static/app.js`

- [ ] **Step 1: 写两个关键失败测试**

仓储测试记录 post 后的 room version、event sequence、state revision，再执行 `lease_next` 并读取新的完整 snapshot：

```rust
let _claim = repository.lease_next("worker-1", 30).await?.unwrap();
let after_lease = repository.snapshot(&room_id, 100).await?;
assert_eq!(after_lease.room.version, before.room.version);
assert_eq!(after_lease.room.latest_event_seq, before.room.latest_event_seq);
assert!(after_lease.room.state_revision > before.room.state_revision);
```

Node 测试要求同房间 snapshot 的 `(state_revision, room.version, latest_event_seq)` 任一回退或三者完全重复时拒绝整体替换；同一测试再断言低版本 Inbox/Member 事件不覆盖高版本。

- [ ] **Step 2: 运行 RED**

```bash
cd rust
cargo test -p ai-brain-cli collaboration_snapshot_state_revision -- --nocapture
cd crates/ai-brain-cli/src/web/static
node --test room_reply.test.js
```

Expected: Rust 缺少 schema/DTO 字段；Node 当前接受相等 room/event 水位。

- [ ] **Step 3: 实现 schema v8 与原子 revision**

将 `SCHEMA_VERSION` 升为 8。新 DDL 为 `collaboration_rooms` 增加 `state_revision INTEGER NOT NULL DEFAULT 0`；v7→v8 在 Immediate transaction 中添加列、将已有房间初始化为 1、最后写 schema version。

`enqueue_room_changed` 获取 outbox INSERT 的 affected rows；仅插入新 outbox 时执行：

```sql
UPDATE collaboration_rooms
SET state_revision = state_revision + 1
WHERE room_id = ?1
```

`CollaborationRoomView` 增加 `#[serde(default)] state_revision: u64`，`room_from_connection` 同步读取。

- [ ] **Step 4: 实现前端单调门禁**

room-local 状态增加 `authoritativeRoomStateRevision`。同房间完整 snapshot 仅在 state revision、room version、event sequence 都不回退且至少一项增长时应用。新增纯函数按 `member_id`/`inbox_item_id` 比较 `version`；`mergeMember`、`mergeInboxItem` 对重复或更低版本直接返回且不重绘。

- [ ] **Step 5: 运行 GREEN 与迁移回归**

```bash
cd rust
cargo test -p ai-brain-cli collaboration_snapshot_state_revision -- --nocapture
cargo test -p ai-brain-cli web::collaboration::tests -- --nocapture
cd crates/ai-brain-cli/src/web/static
node --test
```

Expected: 新关键测试、schema v2-v7 迁移与现有 Node 测试全部 PASS。

- [ ] **Step 6: 提交 Task 2**

```bash
git add rust/crates/ai-brain-cli/src/web/collaboration.rs \
        rust/crates/ai-brain-cli/src/web/static/app.js \
        rust/crates/ai-brain-cli/src/web/static/room_reply.js \
        rust/crates/ai-brain-cli/src/web/static/room_reply.test.js
git commit -m "fix(web): order collaboration snapshots monotonically"
```

### Task 3: 目录访问与 UTF-8 持久化边界

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs`

- [ ] **Step 1: 写最小平台关键测试**

在 `#[cfg(unix)]` 下创建临时目录并移除所有 execute/search 位，发布一条冻结到该目录的消息；启动 runtime 后断言 Inbox/Task 失败且 provider query 计数为零。另用 `OsStringExt::from_vec` 构造非 UTF-8 目录名，断言 repository 更新返回明确 UTF-8 配置错误且房间目录未改变。

- [ ] **Step 2: 运行 RED**

```bash
cd rust
cargo test -p ai-brain-cli inaccessible_frozen_directory_stops_before_provider -- --nocapture
cargo test -p ai-brain-cli non_utf8_room_directory_is_rejected -- --nocapture
```

Expected on Unix: 权限用例仍进入 provider；非 UTF-8 路径当前被 `display()` 有损保存。Windows 编译但按 cfg 不运行 Unix fixture。

- [ ] **Step 3: 实现共享验证**

在 `collaboration.rs` 增加 crate 内共享 helper：

```rust
pub(crate) fn validate_working_directory_access(path: &Path) -> Result<()> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_dir() { /* 返回中文 Config */ }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o111 == 0 { /* 返回中文 Config */ }
    std::fs::read_dir(path).map_err(/* 中文 Config */)?;
    Ok(())
}

fn path_to_storage_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| CollaborationError::Config("工作目录不是有效 UTF-8 路径".into()))
}
```

启动目录与更新目录在持久化前调用二者；runtime `tool_execution_context_for_claim` 在任何 Task/context/provider 工作前调用 access helper，但仍返回 Claim 原路径构造 `ToolExecutionContext`。

- [ ] **Step 4: 运行 GREEN 与关键回归**

```bash
cd rust
cargo test -p ai-brain-cli inaccessible_frozen_directory_stops_before_provider -- --nocapture
cargo test -p ai-brain-cli non_utf8_room_directory_is_rejected -- --nocapture
cargo test -p ai-brain-cli missing_frozen_directory_stops_before_provider -- --nocapture
cargo test -p ai-brain-cli web::collaboration_runtime::tests -- --nocapture
```

Expected: Unix 关键测试通过；Windows 至少编译通过；既有目录缺失和 runtime 模块无回归。

- [ ] **Step 5: 提交 Task 3**

```bash
git add rust/crates/ai-brain-cli/src/web/collaboration.rs \
        rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs
git commit -m "fix(web): reject inaccessible room directories early"
```

### Task 4: 最终验证与整分支复审

**Files:**
- Review only: `b1c15a93..HEAD`

- [ ] **Step 1: 运行功能定向回归**

```bash
cd rust
cargo test -p knowledge-core -p brain-core -p brain-main -p task-engine
cargo test -p ai-brain-cli web::collaboration::tests
cargo test -p ai-brain-cli web::collaboration_runtime::tests
cargo test -p ai-brain-cli web::ws_handler::tests
cd crates/ai-brain-cli/src/web/static
node --test
cd ../../../../../..
python -m pytest tests/test_porting_workspace.py -q
```

Expected: 全部 PASS。

- [ ] **Step 2: 运行仓库门禁**

```bash
cd rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: 格式通过。既有 Windows Unix API、integration API 漂移和旧 lint 若仍失败，逐条保留未改基线证据，不把功能分支误报为全绿。

- [ ] **Step 3: 检查范围并请求 fresh review**

```bash
git diff --check b1c15a93..HEAD
git status --short
git log --oneline b1c15a93..HEAD
```

Expected: tracked 工作树干净，仅保留 `.port_sessions/`、`target-spec-review/`；最终 reviewer 为 Critical 0 / Important 0 后才进入交付。
