# Last User Message Persistent Retry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore a persistent retry action for only the last user message in a brain-v2 collaboration room, invalidating every later result and replaying the retained message without creating a duplicate.

**Architecture:** Keep the collaboration event log append-oriented by marking superseded events invalid instead of deleting audit data. A repository transaction validates the retained user event, invalidates later room events, cancels derived inbox work, and creates a fresh inbox delivery for the retained event; the runtime cancels active workers before committing the retry and broadcasts the authoritative room snapshot. The Web client renders retry only on the latest visible user event and replaces local state from the returned snapshot.

**Tech Stack:** Rust, Tokio, Axum WebSocket, rusqlite/SQLite, vanilla JavaScript, Cargo tests.

---

## File Structure

- Modify `crates/ai-brain-cli/src/web/collaboration.rs`
  - Add the persistent event-invalidation migration.
  - Add repository-level retry transaction and snapshot filtering.
  - Add repository regression tests.
- Modify `crates/ai-brain-cli/src/web/collaboration_runtime.rs`
  - Cancel active room work, invoke the repository retry, reschedule the retained event, and broadcast a snapshot.
  - Add runtime retry tests.
- Modify `crates/ai-brain-cli/src/web/ws_handler.rs`
  - Route `retry_last_user_message` through collaboration runtime when the active session is a room.
  - Keep legacy-session fallback isolated.
  - Add protocol and handler tests.
- Modify `crates/ai-brain-cli/src/web/static/app.js`
  - Render retry only for the last visible user room event.
  - Clear stale client state and request the server retry.
- Modify `crates/ai-brain-cli/src/web/static/style.css`
  - Reuse the existing message action visual language for the room timeline retry button.

### Task 1: Persistently invalidate collaboration results after the retained user event

**Files:**
- Modify: `crates/ai-brain-cli/src/web/collaboration.rs`
- Test: `crates/ai-brain-cli/src/web/collaboration.rs`

- [ ] **Step 1: Write the failing repository tests**

Add tests alongside the existing collaboration repository tests:

```rust
#[test]
fn retry_last_user_event_invalidates_later_room_results_persistently() {
    let (_temp, repository) = repository();
    let initial = ensure(&repository);
    let member = initial.members[0].clone();
    let posted = repository
        .post_message_checked(
            "room-1",
            &[MemberAddress {
                member_id: member.member_id.clone(),
                expected_version: member.version,
            }],
            "请重新回答".into(),
            RoomInputMode::Chat,
            DEFAULT_THREAD_KEY.into(),
            initial.room.version,
            "user-message-1".into(),
        )
        .unwrap();
    let claim = repository.claim_next(&member.member_id).unwrap().unwrap();
    repository
        .reconcile_completed_item(
            &posted.inbox_items[0].inbox_item_id,
            &claim.run_id,
            "旧回答",
        )
        .unwrap();

    let retried = repository
        .retry_last_user_event("room-1", &posted.event.event_id, "retry-1")
        .unwrap();

    assert_eq!(retried.retained_event.event_id, posted.event.event_id);
    assert_eq!(retried.inbox_items.len(), 1);
    assert!(retried
        .snapshot
        .events
        .iter()
        .all(|event| event.content != "旧回答"));
    drop(repository);

    let reopened = CollaborationRepository::open(_temp.path().join("collaboration.db")).unwrap();
    assert!(reopened
        .snapshot("room-1")
        .unwrap()
        .events
        .iter()
        .all(|event| event.content != "旧回答"));
}

#[test]
fn retry_rejects_non_last_user_event_without_mutating_room() {
    let (_temp, repository) = repository();
    let initial = ensure(&repository);
    let member = initial.members[0].clone();
    let first = repository
        .post_message(
            "room-1",
            std::slice::from_ref(&member.member_id),
            "第一条",
            RoomInputMode::Chat,
            "first",
        )
        .unwrap();
    let next_snapshot = repository.snapshot("room-1").unwrap();
    repository
        .post_message(
            "room-1",
            std::slice::from_ref(&next_snapshot.room.default_member_id),
            "第二条",
            RoomInputMode::Chat,
            "second",
        )
        .unwrap();

    let before = repository.snapshot("room-1").unwrap();
    let error = repository
        .retry_last_user_event("room-1", &first.event.event_id, "retry-first")
        .unwrap_err();
    let after = repository.snapshot("room-1").unwrap();

    assert!(error.to_string().contains("最后一条用户消息"));
    assert_eq!(before.events.len(), after.events.len());
}
```

- [ ] **Step 2: Run the repository tests and verify RED**

Run:

```powershell
cargo test -p ai-brain-cli --lib retry_last_user_event -- --nocapture
```

Expected: compilation fails because `retry_last_user_event` and its result type do not exist.

- [ ] **Step 3: Add the invalidation migration and retry result**

Extend the schema migration in `CollaborationRepository::open`:

```rust
if !table_has_column(&connection, "room_events", "invalidated_at")? {
    connection.execute(
        "ALTER TABLE room_events ADD COLUMN invalidated_at TEXT",
        [],
    )?;
}
connection.execute(
    "CREATE INDEX IF NOT EXISTS room_events_visible_sequence_idx
     ON room_events(room_id, invalidated_at, sequence)",
    [],
)?;
```

Define the repository return type near `PostMessageResult`:

```rust
#[derive(Debug, Clone)]
pub struct RetryMessageResult {
    pub retained_event: RoomEventView,
    pub inbox_items: Vec<InboxItemView>,
    pub snapshot: RoomSnapshot,
}
```

- [ ] **Step 4: Implement the atomic repository retry**

Add `CollaborationRepository::retry_last_user_event` with this contract:

```rust
pub fn retry_last_user_event(
    &self,
    room_id: &str,
    event_id: &str,
    retry_id: &str,
) -> Result<RetryMessageResult> {
    let mut connection = self.connect()?;
    let transaction =
        connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let retained = events_from_connection(&transaction, room_id, 300)?
        .into_iter()
        .find(|event| event.event_id == event_id)
        .ok_or_else(|| CollaborationError::EventNotFound(event_id.into()))?;
    if retained.room_id != room_id || retained.sender_kind != "user" {
        return Err(CollaborationError::Config(
            "只能重试当前房间的用户消息".into(),
        ));
    }
    let last_user_id: String = transaction.query_row(
        "SELECT event_id FROM room_events
         WHERE room_id = ?1 AND sender_kind = 'user' AND invalidated_at IS NULL
         ORDER BY sequence DESC LIMIT 1",
        [room_id],
        |row| row.get(0),
    )?;
    if last_user_id != retained.event_id {
        return Err(CollaborationError::Config(
            "只能重试最后一条用户消息".into(),
        ));
    }

    let now = Utc::now().to_rfc3339();
    transaction.execute(
        "UPDATE room_events SET invalidated_at = ?1
         WHERE room_id = ?2 AND sequence > ?3 AND invalidated_at IS NULL",
        params![now, room_id, retained.sequence],
    )?;
    transaction.execute(
        "UPDATE member_inbox_items
         SET state = 'cancelled', cancel_requested = 1, version = version + 1,
             completed_at = COALESCE(completed_at, ?1)
         WHERE source_event_id IN (
             SELECT event_id FROM room_events
             WHERE room_id = ?2 AND invalidated_at IS NOT NULL
         ) AND state IN ('pending', 'leased', 'running')",
        params![now, room_id],
    )?;

    let inbox_items = enqueue_retry_for_existing_event(
        &transaction,
        &retained,
        retry_id,
    )?;
    enqueue_room_changed(
        &transaction,
        room_id,
        "event",
        &retained.event_id,
        &format!("room-retry:{retry_id}"),
    )?;
    transaction.commit()?;
    Ok(RetryMessageResult {
        retained_event: retained,
        inbox_items,
        snapshot: self.snapshot(room_id)?,
    })
}
```

Extract the existing inbox/delivery insertion used by `post_message_checked` into
`enqueue_retry_for_existing_event`. It must create new inbox IDs and idempotency keys derived
from `retry_id`, while retaining the original user event and content.

- [ ] **Step 5: Filter invalid events from every room read path**

Update event and dependent-view queries:

```sql
WHERE e.room_id = ?1
  AND e.invalidated_at IS NULL
```

Apply the same visibility condition through joins in:

- `events_from_connection`
- `inbox_from_connection`
- `deliveries_from_connection`
- `member_history`
- any context-building query that reads `room_events`

Late member output must be rejected when its source event or conversation root has
`invalidated_at IS NOT NULL`.

- [ ] **Step 6: Run repository tests and commit**

Run:

```powershell
cargo test -p ai-brain-cli --lib web::collaboration -- --nocapture
```

Expected: all collaboration repository tests pass.

Commit:

```powershell
git add crates/ai-brain-cli/src/web/collaboration.rs
git commit -m "feat(web): persist collaboration retry cutoffs"
```

### Task 2: Cancel active work and replay the retained event

**Files:**
- Modify: `crates/ai-brain-cli/src/web/collaboration_runtime.rs`
- Test: `crates/ai-brain-cli/src/web/collaboration_runtime.rs`

- [ ] **Step 1: Write the failing runtime test**

```rust
#[tokio::test]
async fn retry_last_user_message_cancels_active_run_and_requeues_retained_event() {
    let fixture = runtime_fixture().await;
    let posted = fixture.post_default("重新分析这个问题").await;
    let running = fixture.wait_for_running(&posted.event.event_id).await;

    let snapshot = fixture
        .runtime
        .retry_last_user_message(
            fixture.room_id.clone(),
            posted.event.event_id.clone(),
        )
        .await
        .unwrap();

    assert!(snapshot
        .events
        .iter()
        .any(|event| event.event_id == posted.event.event_id));
    assert_eq!(
        snapshot
            .inbox
            .iter()
            .filter(|item| item.source_event_id == posted.event.event_id)
            .filter(|item| item.state == InboxState::Pending)
            .count(),
        1
    );
    assert!(fixture.task_is_cancelled(&running.task_run_id).await);
}
```

- [ ] **Step 2: Run the runtime test and verify RED**

Run:

```powershell
cargo test -p ai-brain-cli --lib retry_last_user_message_cancels -- --nocapture
```

Expected: compilation fails because `CollaborationRuntime::retry_last_user_message` is missing.

- [ ] **Step 3: Implement runtime cancellation and retry**

Add:

```rust
pub async fn retry_last_user_message(
    &self,
    room_id: String,
    event_id: String,
) -> Result<RoomSnapshot, String> {
    let before = self.snapshot(room_id.clone()).await?;
    for item in before.inbox.iter().filter(|item| {
        matches!(item.state, InboxState::Leased | InboxState::Running)
    }) {
        if let Some(run_id) = item.run_id.clone() {
            self.interrupt_run_checked(
                room_id.clone(),
                item.member_id.clone(),
                run_id,
                item.version,
            )
            .await?;
        }
    }

    let repository = Arc::clone(&self.repository);
    let retry_id = uuid::Uuid::new_v4().to_string();
    let result = tokio::task::spawn_blocking(move || {
        repository.retry_last_user_event(&room_id, &event_id, &retry_id)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;

    self.dispatcher_notify.notify_waiters();
    self.broadcast(WebProgressEvent::RoomSnapshot {
        snapshot: result.snapshot.clone(),
    });
    Ok(result.snapshot)
}
```

Use the runtime’s existing cancellation registry and worker notification methods rather than
creating a second scheduling path.

- [ ] **Step 4: Run runtime tests and commit**

Run:

```powershell
cargo test -p ai-brain-cli --lib web::collaboration_runtime -- --nocapture
```

Expected: all collaboration runtime tests pass.

Commit:

```powershell
git add crates/ai-brain-cli/src/web/collaboration_runtime.rs
git commit -m "feat(web): replay retained room messages"
```

### Task 3: Route WebSocket retry to the collaboration runtime

**Files:**
- Modify: `crates/ai-brain-cli/src/web/ws_handler.rs`
- Test: `crates/ai-brain-cli/src/web/ws_handler.rs`

- [ ] **Step 1: Write failing routing tests**

Add a pure validation helper test:

```rust
#[test]
fn retry_room_message_requires_stable_event_id() {
    assert!(validate_retry_message_id("").is_err());
    assert!(validate_retry_message_id("event-user-1").is_ok());
}
```

Add a handler fixture assertion that `RetryLastUserMessage` calls collaboration retry when
the active session has a room snapshot and does not call `prepare_conversation_fork`.

- [ ] **Step 2: Run handler tests and verify RED**

Run:

```powershell
cargo test -p ai-brain-cli --lib retry_room_message -- --nocapture
```

Expected: compilation fails because `validate_retry_message_id` and the room retry routing are
missing.

- [ ] **Step 3: Add room-aware retry routing**

Keep the wire format stable:

```rust
RetryLastUserMessage { message_id: String },
```

Before constructing a legacy `QueryAction`, route room retries:

```rust
ClientMessage::RetryLastUserMessage { message_id } => {
    if let Err(message) = validate_retry_message_id(&message_id) {
        send_event(&mut sender, WebProgressEvent::Error { message })
            .await
            .ok();
    } else {
        match state
            .collaboration
            .retry_last_user_message(active_session_id(&state).await, message_id)
            .await
        {
            Ok(snapshot) => {
                send_event(
                    &mut sender,
                    WebProgressEvent::RoomSnapshot { snapshot },
                )
                .await
                .ok();
            }
            Err(message) => {
                send_event(&mut sender, WebProgressEvent::Error { message })
                    .await
                    .ok();
            }
        }
    }
    None
}
```

Before the room retry is scheduled, synchronize the legacy `SessionManager` by calling its
existing `retry_last_user_message` with the session manager's own final user-message ID. Do not
pass the room event ID into `SessionManager`; the room event ID and session message ID belong to
different identity domains. If legacy synchronization fails, return an error before scheduling
the retained room event.

- [ ] **Step 4: Run handler tests and commit**

Run:

```powershell
cargo test -p ai-brain-cli --lib web::ws_handler -- --nocapture
```

Expected: all WebSocket handler tests pass.

Commit:

```powershell
git add crates/ai-brain-cli/src/web/ws_handler.rs
git commit -m "feat(web): route last-message retry through rooms"
```

### Task 4: Restore the retry action in the brain-v2 room timeline

**Files:**
- Modify: `crates/ai-brain-cli/src/web/static/app.js`
- Modify: `crates/ai-brain-cli/src/web/static/style.css`

- [ ] **Step 1: Add a failing static-contract test**

Add a Rust static asset test in `ws_handler.rs` or the existing Web static test module:

```rust
#[test]
fn room_timeline_exposes_retry_only_for_last_user_event() {
    let script = include_str!("static/app.js");
    assert!(script.contains("lastVisibleUserEventId"));
    assert!(script.contains("retry_last_user_message"));
    assert!(script.contains("event.event_id === lastVisibleUserEventId"));
}
```

- [ ] **Step 2: Run the static-contract test and verify RED**

Run:

```powershell
cargo test -p ai-brain-cli --lib room_timeline_exposes_retry -- --nocapture
```

Expected: FAIL because `lastVisibleUserEventId` is absent from the room renderer.

- [ ] **Step 3: Render the retry button on the final user event**

In `renderRoomTimeline`, calculate:

```javascript
const visibleUserEvents = roomSnapshot.events.filter(
    (event) => event.sender_kind === 'user',
);
const lastVisibleUserEventId = visibleUserEvents.at(-1)?.event_id || null;
```

For a user timeline event:

```javascript
if (event.event_id === lastVisibleUserEventId) {
    const retry = createMessageAction('rotate-ccw', '重试最后一条消息', () => {
        beginHistoryRegeneration('重试最后一条用户消息');
        roomSnapshot.events = roomSnapshot.events.filter(
            (item) => item.sequence <= event.sequence,
        );
        roomSnapshot.inbox = [];
        renderCollaborationRoom();
        send('retry_last_user_message', { message_id: event.event_id });
    });
    actions.appendChild(retry);
}
```

Do not render edit or delete actions.

- [ ] **Step 4: Style the room retry action**

Reuse `.msg-action` dimensions and focus styles:

```css
.room-event-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.35rem;
    margin-top: 0.4rem;
}

.room-event:hover .room-event-actions,
.room-event:focus-within .room-event-actions {
    opacity: 1;
}
```

- [ ] **Step 5: Run static and Web tests and commit**

Run:

```powershell
cargo test -p ai-brain-cli --lib room_timeline_exposes_retry -- --nocapture
cargo test -p ai-brain-cli --lib web::ws_handler -- --nocapture
```

Expected: both commands pass.

Commit:

```powershell
git add crates/ai-brain-cli/src/web/static/app.js crates/ai-brain-cli/src/web/static/style.css crates/ai-brain-cli/src/web/ws_handler.rs
git commit -m "feat(web): show retry on last room message"
```

### Task 5: End-to-end verification and service restart

**Files:**
- Verify: `crates/ai-brain-cli/src/web/collaboration.rs`
- Verify: `crates/ai-brain-cli/src/web/collaboration_runtime.rs`
- Verify: `crates/ai-brain-cli/src/web/ws_handler.rs`
- Verify: `crates/ai-brain-cli/src/web/static/app.js`

- [ ] **Step 1: Run focused suites**

```powershell
cargo test -p ai-brain-cli --lib web::collaboration -- --nocapture
cargo test -p ai-brain-cli --lib web::collaboration_runtime -- --nocapture
cargo test -p ai-brain-cli --lib web::ws_handler -- --nocapture
```

Expected: all focused tests pass with zero failures.

- [ ] **Step 2: Run workspace verification**

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: formatting passes. Record any pre-existing Windows-only or warning-as-error failures
separately; do not claim full verification if either command exits non-zero.

- [ ] **Step 3: Rebuild and restart the Web service**

Stop the exact running `target\debug\ai-brain.exe web` process, then run:

```powershell
cargo build -p ai-brain-cli --bin ai-brain
Start-Process -FilePath '.\target\debug\ai-brain.exe' -ArgumentList 'web' -WorkingDirectory (Get-Location) -WindowStyle Hidden
```

Expected: the process remains alive and listens on `127.0.0.1:8080`.

- [ ] **Step 4: Verify persistence manually**

1. Send a user message and wait for main/member/tool results.
2. Click retry on the last user message.
3. Confirm old results disappear immediately and the user message is not duplicated.
4. Wait for new results.
5. Refresh the browser.
6. Confirm only the retained user message and new results remain.

- [ ] **Step 5: Commit final verification adjustments**

If verification required no code changes, do not create an empty commit. Otherwise:

```powershell
git add crates/ai-brain-cli/src/web
git commit -m "test(web): verify persistent room retry"
```
