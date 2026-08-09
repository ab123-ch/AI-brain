# Task Plan: Brain Cockpit View

## Current Task: Phase 7 Cutover/Data Migration and Phase 8 Legacy Novel Runtime Removal (2026-07-25)

### Goal
Complete the final two approved delivery stages after the finished Phase 6 vertical slice: cut production composition and durable Novel data to the extracted Domain/Workflow/Knowledge boundaries without losing identities or approval authority, then remove the superseded resident Novel runtime and compatibility ownership only after the cutover is proven restart-safe.

### Phases
- [x] Recover the exact Phase 7/8 contracts, migration inventory, compatibility gates, and acceptance evidence from the approved design and current worktree.
- [x] Add focused red contracts for migration/idempotency, production routing, restart/replay, and absence of legacy runtime ownership.
- [x] Implement Phase 7 cutover and durable data migration with rollback-safe, repeatable behavior.
- [x] Verify Phase 7 data preservation, production behavior, restart idempotency, and full regressions.
- [x] Remove the Phase 8 special Novel runtime and obsolete compatibility paths without changing Domain/Canon/publication authority.
- [x] Run focused/full/static/live verification and synchronize architecture, findings, and progress records.

### Resume Evidence
- Repository planning records state that Phase 5 and Phase 6 production vertical slices completed on 2026-07-24.
- The two remaining approved stages are explicitly Phase 7 (cutover/data migration) and Phase 8 (remove the special Novel runtime).
- The worktree is intentionally dirty with the uncommitted Phase 1-6 implementation and documentation; all existing changes must be preserved and extended in place.
- Session catchup contained no relevant implementation context, so the repository plan, approved design, current code, and persisted migration state are authoritative.

### Implementation Decision
- Add a `novel-application` composition boundary that owns `novel.db`, legacy read-only import, durable domain/outbox operations, adapter-driven Memory/Graph rebuild, and one high-level `TaskApplicationPort`.
- Keep `novel-domain` free of persistence/runtime dependencies and keep Writer execution in TaskRun-backed `novel-workflow`; do not move actor state into CLI.
- Cut production composition to the application port and verify migration/restart first. Only then remove the resident crate, low-level tool protocol, special model/subagent configuration, and obsolete WebSession compatibility field.

### Delivery Evidence
- Phase 7 and Phase 8 are complete. The approved design now records the implemented application/database/outbox cutover and physical resident-runtime removal; TaskEngine KG v1.5.0 validates 24 provides, 18 components, 4 integration points, and 24 validation rules.
- Full final gates pass: offline workspace check, full rustfmt check, Git whitespace, legacy runtime/tool/config scans, affected strict Clippy boundaries, ordinary remaining Clippy suites, and the focused/full regressions recorded in `progress.md`.
- Live default-root cutover is a verified zero-record migration. Across clean stop and two starts, `novel.db` integrity, all-zero project/checkpoint/event/publication/outbox counts, size, mtime, and SHA-256 remained identical; both starts reported zero imports and zero pending projections.
- The current build remains running at `http://127.0.0.1:8080`; HTTP/static JavaScript and read-only WebSocket room snapshot plus cursor replay are healthy.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| New `novel-application` initially selected `rusqlite 0.32`, conflicting with the workspace's `rusqlite 0.31` native SQLite link | Phase 7 red-contract compile | Align the new crate to the existing `0.31` dependency and rerun offline so the intended missing-API red boundary is observable. |
| First projection compile assigned `event_id` inside `NovelKnowledgeEvent`, whose identity is intentionally carried by outer `KnowledgeSourceEvent` | Phase 7 migration implementation | Keep the deterministic Canon event ID at the registered source-event boundary and leave the Domain payload identity-free. |
| First revision-workflow compile retained one start-path call to the old one-argument `persist_initial` helper | Phase 7 iterative Writer implementation | Pass the original task ID as the initial execution ID; no runtime behavior or persisted data was reached. |
| First application publication fixture supplied only one main-review evidence ref and was rejected by the existing three-source Domain guard | Phase 7 application contract | Fix the fixture to provide requirements, Canon, and immutable Artifact evidence; do not weaken review authority. |
| First CLI cutover check retained two owned-String calls from the old Handle API after `TaskApplicationPort` changed task IDs to borrowed strings | Phase 7 composition cutover | Borrow the parsed task IDs and skip pre-create source association for start; start provenance is already frozen in the request. |
| Strict `novel.db` idempotency rejected replay of a stable completed event ID because its details/timestamp included freshly reconstructed candidate timing | Phase 7 restart route test | Keep strict same-ID/same-content validation; serialize only stable candidate ID/hash and use the frozen initial checkpoint timestamp for the completed event. |
| First Phase 7/8 resume-record patch assumed the template heading `# Progress Log`, but the repository uses `# Progress` | Phase 8 resume synchronization | The multi-file patch was rejected atomically; inspect the real heading and reapply against `# Progress` while recording the mismatch here. |
| New Phase 8 project-facade contract failed on the five intentionally absent `NovelApplicationService` project APIs | Phase 8 application facade red gate | Expected red boundary; implement Novel-owned create/list/recall/consistency/conflict operations with hash-CAS persistence before changing CLI tools. |
| First green project-facade run asserted a nonexistent `ConsistencyReport.is_clean` convenience field | Phase 8 application facade contract | Use the established `issues.is_empty()` contract; no production implementation failed. |
| First Phase 8 package formatting check reported style diffs across the newly edited facades and existing unformatted Phase 7 files | Phase 8 facade routing | Run package-scoped rustfmt before interpreting compile/test output; these are mechanical layout changes, not behavioral failures. |
| First CLI facade suite passed 11/12; the Agent rejection test still expected the old phrase `领域工作流` | Phase 8 facade routing | Update the assertion to the new high-level `novel_task 领域应用` message; command routing and persistence tests were already green. |
| First post-deletion Cargo commands failed because the workspace `crates/*` glob still discovered the empty `crates/brain-novel` directory without a manifest | Phase 8 resident crate removal | Confirm both removed source directories are empty, delete only those empty directories, then rerun dependency and compile checks. |
| First post-removal Novel regression used nonexistent `ToolGrant::is_empty()` in a strengthened absence assertion | Phase 8 regression | Use the established iterator API (`iter().next().is_none()`); production crates compiled before the test-only error. |
| Full tools regression passed 47/48 because unknown `Agent` roles silently fell through to the general-purpose profile after the Novel-specific guard was removed | Phase 8 generic Agent validation | Validate the existing six supported built-in roles centrally before profile construction; unsupported roles now fail before filesystem writes without any Novel branch. |
| Combined strict Clippy stopped on three pre-existing `brain-dispatch/src/bus.rs` style findings unrelated to the removed enum variant | Phase 8 static gate | Preserve the unrelated dispatch API; run strict Clippy for the remaining affected packages and ordinary all-target Clippy plus its 5/5 tests for Dispatch, reporting the known residual debt explicitly. |
| Second combined strict Clippy exposed 12 Phase 7/8 Novel Application future/unused-async findings plus 15 pre-existing tools findings | Phase 8 static gate | Fix the Novel-owned findings by making inherent SQLite-only methods synchronous and boxing continuation futures; exclude the unrelated tools backlog from the strict new-crate proof while retaining its green 48-test suite. |
| Final workspace formatting gate found one rustfmt-only difference in the new generic Agent role validator | Phase 8 final static gate | Apply package-scoped rustfmt to `tools`, then rerun formatting, whitespace, and the affected regression suite. |
| Direct `sqlite3 -readonly` could not open the WAL-configured live `novel.db` without creating a shared-memory handle | Phase 8 live baseline audit | Preserve the database untouched and use SQLite's `file:...?...immutable=1` URI for the pre-start static snapshot; use ordinary read-only SQL only while the owning service is running. |
| First live outbox baseline query guessed nonexistent `memory_published_at`/`graph_published_at` columns | Phase 8 live baseline audit | The statement failed during prepare with no writes; inspect `PRAGMA table_info(novel_outbox)` and query only verified schema names. |
| In-app Browser bootstrap was rejected before navigation because the host omitted the required `sandboxPolicy` metadata | Phase 8 live Web verification | No page or service state was touched; retain the environment limitation and use read-only HTTP/WebSocket protocol probes plus database/runtime logs for the live health proof. |
| The local Node 20 runtime has neither global `WebSocket` nor the optional `ws` package | Phase 8 live Web verification | Do not install a one-off dependency; use an already installed Python WebSocket client or a minimal RFC 6455 handshake for the read-only collaboration probe. |
| First combined KG `jq` assertion lost the root object after piping components into an array | Phase 8 architecture synchronization | The KG parsed and all other checks passed; bind the root as `$root` before comparing aggregate component count and rerun the expanded structural validation. |

## Current Task: Phase 6 Novel Domain and Workflow Extraction (2026-07-24)

### Goal
Move Novel project/task/candidate/review/publication contracts and transition authority out of generic Memory/Graph/runtime ownership, introduce explicit Novel workflows and a registered knowledge adapter over the Phase 5 ports, and retain old `novel_*` tools only as high-level compatibility facades during cutover.

### Phases
- [x] Recover the exact Novel domain types, transition guards, publication authority, workflow roles, adapter events, and compatibility boundaries from the approved design and current code.
- [x] Add focused failing contracts for domain-owned state transitions, TaskRun-backed workflow definitions, evidence-linked knowledge projection, and generic-core dependency neutrality.
- [x] Implement `novel-domain`, `novel-workflow`, and `novel-knowledge-adapter`, then migrate one current Novel task path without introducing a second Canon or publication authority.
- [x] Verify compatibility, recovery/idempotency, rejected-candidate isolation, Memory/Graph evidence, existing Novel behavior, full regressions, and architecture/knowledge records.

### Invariants
- Novel Domain owns project/task/candidate/review/publication types and transition guards; generic Memory, Graph, AgentRuntime, and TaskEngine never branch on Novel semantics.
- Canon/project resources remain the publication source of truth. Workflow, Memory, and Graph may reference accepted revisions but cannot bypass user approval or widen visibility.
- Novel knowledge enters generic Graph only through a registered adapter from committed/approved events, with EvidenceLinks back to Canon, Artifact, Memory, or versioned project resources.
- Every Writer/Reviewer/Extractor execution remains an isolated Task/InstanceRun with one frozen ContextSnapshot, explicit Profile/ToolGrant/OutputContract, budget, and restart-safe Artifact identity.
- Preserve current Novel behavior and data during this phase; no double-write cutover, destructive migration, or removal of the old resident runtime belongs here.

### Cost Follow-Up
- Record a zero-Token deterministic prefilter for ambient participation as a bounded Phase 4.5 optimization; do not let it bypass TaskEngine audit/budget authority or displace Phase 5 Memory/Graph contracts.

### Delivery Status
- Phase 5 production vertical slice completed on 2026-07-24.
- Phase 6 production vertical slice completed on 2026-07-24, including live read-only HTTP/WebSocket replay and unchanged persistence verification.
- Two approved delivery stages remain: Phase 7 (cutover/data migration) and Phase 8 (remove the special Novel runtime).

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| Third final RFC 6455 probe selected the first historical occurrence of the repeated question and therefore matched its pre-fix blank A event | Phase 6 live WebSocket closeout | All concrete Xiaomi display assertions passed before this local assertion. Select the newest matching user event (verified as sequence 28), bind A's reply by later sequence, and separately confirm the frontend suppresses the retained append-only blank historical event. |
| First no-active-work distribution query used `task_runs.status` instead of the schema's state column | Phase 6 live persistence closeout | SQLite returned the valid Inbox distribution and then stopped at read-only prepare; inspect the two TaskEngine table schemas and rerun using their verified lifecycle column names. |
| Second final RFC 6455 probe used an unlabeled hard-coded idle-state assertion after the concrete-model and nonblank-reply assertions | Phase 6 live WebSocket closeout | The join/replay completed and the earlier display assertions did not fail; stop guessing the serialized activity label, prove no active work from authoritative Task/Inbox state, and make the final probe report raw activity with labeled assertions only for required snapshot behavior. |
| First final RFC 6455 assertion assumed the persisted A reply uses RoomEvent kind `message` | Phase 6 live WebSocket closeout | Handshake and read-only `join_room` succeeded, but the local assertion exited before reporting the snapshot; inspect the persisted sequence 28-30 event kinds and rerun with the verified protocol value, still sending no business message. |
| First combined Phase 6 live-record patch included an empty trailing hunk before the second file update | Phase 6 planning synchronization | `apply_patch` rejected the patch atomically and changed neither file; remove the empty hunk and reapply both additions against their exact headings. |
| First Phase 6 room-detail helper query assumed a `collaboration_rooms.status` column that is not present in schema v4 | Phase 6 live snapshot lookup | The read-only statement failed during prepare and changed nothing; inspect `PRAGMA table_info` and query only the verified room/member columns before opening the WebSocket. |
| Final Phase 6 in-app browser bootstrap is still rejected before navigation because the host omits required `sandboxPolicy` metadata | Phase 6 live Web closeout | No page or service state was touched; retain the established environment limitation and complete the same read-only snapshot assertion through the public RFC 6455 protocol plus HTTP/static checks. |
| First final Phase 6 SQLite baseline query used the descriptive count labels as table names (`collaboration_room_events`, `collaboration_inbox`) | Phase 6 live persistence closeout | The statement failed during read-only prepare and changed nothing; inspect the live process's database files and actual SQLite schema, then query only verified tables with a bounded busy timeout. |
| First Phase 6 service replacement used a nohup child that the execution environment reaped after its shell completed | Phase 6 live deployment | Startup reached HTTP and logged concrete Xiaomi model metadata with no panic or data change; restart in a managed long-running PTY session and verify sustained HTTP/WebSocket health plus unchanged durable counts. |
| Phase 6 KG closeout tried array tail slicing on keyed `architecture_decisions` and `integration_points` objects | Phase 6 KG synchronization | The read-only jq query changed nothing; inspect object keys/entries and append named Phase 6 records using the existing keyed shape. |
| Second strict Novel-crate Clippy pass cleared Domain findings and then exposed the Adapter's 114-line projection entrypoint | Phase 6 static quality gate | Extract deterministic Novel node/edge construction from source validation and evidence-batch assembly, then rerun all three crates. |
| First strict all-target Clippy for the three new Novel crates found eight Domain-only pedantic findings | Phase 6 static quality gate | Split Canon validation/fact/supersession work, consume owned review/decision/task values, use allocation-free String formatting, and retain the established `NovelOutcome` Rust API with one documented local enum-size allowance. |
| First Novel-to-Generic-Graph integration test moved a non-Copy `NodeId` inside an `FnMut` predicate | Phase 6 Graph evidence gate | Clone the stable identifier for the comparison and rerun the unchanged store/projection assertions. |
| First RealToolExecutor route-test patch included removal of a duplicate scope injection seen only in a truncated combined read, while the current source already has one injection | Phase 6 CLI route proof | Patch was rejected atomically; verify the live file/diff, keep the correct single injection, and apply the test additions against current local contexts. |
| First Phase 6 closeout findings patch targeted a later bullet instead of the actual current file head | Phase 6 resume record | Patch was rejected atomically; read the current heading and insert against that stable line without changing source files. |
| First crash-window helper refactor attached the Drafting event body and checkpoint return to the wrong private functions | Phase 6 recovery contract gate | Restore the three explicit boundaries (`persist_initial`, `persist_completed`, `append_completed_event`) and rerun package tests plus strict Clippy before interpreting behavior. |
| First CLI adapter-test compile imported `NovelTaskType` from the resident crate root, which intentionally does not re-export it | Phase 6 production adapter contract gate | Import the Domain-owned compatibility type from `brain_memory::novel` alongside lifecycle enums, then rerun the unchanged focused suites. |
| First strict `novel-workflow` Clippy found one 106-line execution method and four serialization-only values passed by ownership | Phase 6 start Workflow quality gate | Split writer result persistence/completion into a bounded helper and borrow serialization inputs/join errors; rerun the full package and strict no-deps lint. |
| The new start-task recovery contract fails on seven intentionally absent Workflow service/port symbols | Phase 6 production vertical-slice red gate | Expected red boundary; implement the stable-ID, frozen-snapshot, TaskArtifact, checkpoint reconciliation service and rerun without weakening restart assertions. |
| Resident Novel behavior passed 7/8 after type extraction, but the no-user-acceptance test observed a generic Domain error variant instead of legacy `InvalidTransition` | Phase 6 resident compatibility tests | Replace blanket Domain wrapping with explicit error-by-error mapping back to the legacy public enum, preserving caller pattern matches while Domain remains authoritative. |
| First resident Novel compile after Domain re-export had one manual `return Err(error)` that bypassed the new Domain-to-runtime conversion and two obsolete validator imports | Phase 6 resident compatibility migration | Convert the explicit fork-cancellation error with `.into()` and remove only the unused imports; all `?` paths already use the compatibility conversion. |
| First Legacy Memory Novel regression removed the `CanonStatus` import even though the retained compatibility Graph projector still checks Superseded lifecycle | Phase 6 Memory migration | Restore only the Domain-owned enum import; do not restore deleted Memory transition helpers, then rerun the focused suite. |
| New Phase 6 Domain/Workflow/Adapter contract tests fail on their intentionally absent public symbols | Phase 6 red-contract gate | Expected red boundary; implement the three independent crate APIs without weakening the tests, then rerun each focused contract. |
| Two post-edit KG summaries applied `.task_id`/`.id` directly to array slices, and a new provides entry initially abbreviated the real `GraphEvidenceLink` symbol | Phase 5 KG synchronization | JSON parsing and uniqueness checks passed; use `map(...)` for every sliced-array projection and update the provides symbol to the exact exported Rust name before final validation. |
| Fresh full CLI run passed 244 deterministic tests but the existing real-provider `test_orchestrator_query` exceeded its hard-coded 30-second timeout twice, including one isolated retry | Phase 5 full regression | Live Web completed the same Xiaomi model path; stop repeating the external call, run the full deterministic suite with only this explicit integration test skipped, and report Provider latency separately from product correctness. |
| Initial KG closeout summary assumed `testing.test_cases` and failed on the existing keyed v1.2 shape; the first domain scan also matched the generic adjective `canonical` as a `Canon` substring | Phase 5 documentation/static audit | Read the complete existing KG and query its actual keys before editing; rerun domain neutrality with exact word/identifier patterns instead of a substring scan. |
| First post-restart read-only WebSocket probe indexed `model_policy_details` as an object, but the protocol serializes it as a list | Phase 5 replay verification | The connection failed before sending `join_room` and changed no durable state; reuse the already-proven list/object resolver and rerun the read-only replay. |
| One external `sqlite3` snapshot-detail query returned `database is locked` while the live service finished outbox publication | Phase 5 live provenance audit | The CLI connection had its default zero busy timeout; rerun the read-only audit with `.timeout 5000`, matching the product's bounded read policy, after all Inbox work is terminal. |
| Phase 5 live-evidence record patch assumed a shortened `findings.md` heading | Live closeout documentation | Patch was rejected atomically; locate the actual `Phase 5 Memory/Graph I/O Startup` heading and reapply against that stable context. |
| Initial Phase 5 closeout audit guessed obsolete aggregate/outbox/event column names and generic Memory/Graph table names | Live terminal-state and store-count audit | Treat the successful state distributions as valid, inspect each live schema with `PRAGMA table_info`/`.tables`, and rerun read-only queries against the actual columns; no database mutation occurred. |
| First `knowledge-core` compile found a self-imported `KnowledgeSourceEvent` and an ambiguous token `sum()` type | Phase 5 Core implementation | Remove the duplicate import and annotate the accumulated token count as `usize`; rerun the focused Core contracts. |
| New optional-history/provenance contracts fail on missing `with_source_metadata`, `with_optional_block`, and RoomEvent identity fields | Phase 5 Collaboration snapshot red tests | Expected red boundary; implement the generic optional-block API and extend all three authorized history projections before production migration. |
| First Orchestrator knowledge-runtime insertion targeted a nonexistent `build_main_brain` boundary | Phase 5 composition root | Patch was rejected atomically; inspect the actual `create_v2_main_brain` ending and insert immediately before `create_sub_brains`. |
| Snapshot-input helper patch assumed imports preceded `resolve_member_reasoning_tokens` | Phase 5 model boundary | Patch was rejected atomically; update the real knowledge import and place the helper after all imports where both snapshot and restore-message types are in scope. |
| First migrated CLI check found the obsolete `member_context: String` still present in the narrowed method signature | Phase 5 model boundary compile | Remove the unused legacy parameter; all content is already derived from the validated snapshot inside the method. |
| CollaborationRuntime test compile found two v2 helper calls without the new snapshot argument | Phase 5 persisted-snapshot tests | Upgrade fixtures to construct valid policy/current-input snapshots, assert v3 metadata, and add restart/config-hash stability coverage. |
| Focused formatting check found two layout-only differences in the new collaboration context test | Phase 5 focused verification | Apply workspace rustfmt, then rerun the actual focused test bodies. |
| Combined strict Clippy found the new `ContextBuilder::build` at 180 lines and the existing `runtime/src/bash.rs` items-after-statements lint | Phase 5 mechanical verification | Refactor Core assembly into bounded private steps, then rerun changed-core Clippy with `--no-deps`; leave unrelated runtime lint debt unchanged. |
| Strict Generic Graph Clippy found a 224-byte writer command variant | Phase 5 mechanical verification | Box GraphMutationBatch at the bounded writer-queue boundary; transaction semantics and public API remain unchanged. |
| First live logical-hash query referenced nonexistent `collaboration_rooms.latest_sequence` and hashed empty output | Phase 5 live baseline | Treat the hash as invalid, inspect the actual v4 table schema, and recompute before stopping the old service. |
| Second live hash command lost SQL empty-string literals through nested shell quoting | Phase 5 live baseline | Treat the partial hash as invalid; use SQLite quote-mode NULL output directly and remove COALESCE/string literals from the piped query. |
| First detached Phase 5 server launch did not bind 8080 and produced no immediate log output | Phase 5 live restart | Inspect process/log state and run the binary in a managed foreground session to capture the real startup result before retrying detachment. |
| First real v3 query failed before model admission because an append-only historical blank reply became an empty optional ContextBlock | Phase 5 live query | Preserve the audit event, filter blank history at the Collaboration-to-Context adapter, add a regression, rebuild, and retry the exact question; no model Token was consumed. |
| Initial Knowledge Graph summary query assumed `components` was an array and failed with `Cannot index array with string \"id\"` | Phase 5 graph audit | Inspect the file's actual object/array shape first, then query its keyed sections without changing the graph. |
| Session recovery again surfaced an unrelated historical GPT-model discussion | Phase 5 startup | Ignore it and use the current repository, completed Phase 4.5 records, approved design, live v4 database, and running service as authoritative. |

## Current Task: Phase 4.5 Group Deliberation And Selective Participation (2026-07-24)

### Goal
Extend CollaborationRoom from addressed task delivery into a shared, durable group conversation: every non-archived room member receives every user/member message for awareness, idle members independently decide whether they should contribute, busy members defer the message and reconsider it at safe execution checkpoints, and members may challenge one another without creating reply storms or a second chat authority.

### Phases
- [x] Recover the exact broadcast, participation-decision, busy/deferred, interruption, and loop-bound contracts from the current room/runtime implementation.
- [x] Add focused failing tests for all-member awareness, self-message exclusion, selective reply, deferred reconsideration, safe interruption, and bounded member-to-member debate.
- [x] Implement the smallest durable participation scheduler and protocol/UI projection over the existing RoomEvent, recipient, Inbox, member/thread lane, and TaskEngine authorities.
- [x] Verify idle/busy mixed rooms, member rebuttals, restart/replay idempotency, bounded termination, concrete model metadata, and non-empty replies; synchronize architecture records.

### Completion Evidence
- Collaboration/runtime contracts pass 34/34; deterministic `ai-brain-cli` passes 241/241 with only the explicit real-provider unit test skipped; TaskEngine passes 14/14 plus strict Clippy; MainBrain passes 24/24; AgentRuntime passes 3/3; workspace check and production-library Clippy succeed.
- Live schema v3-to-v4 migration preserved all legacy counts and both pre-migration logical hashes. Backup `/Users/chenh/.ai-brain/runtime.db.phase45-pre-v4-20260724T180956` passes SQLite integrity with SHA-256 `b2e6f982555c953542a80a07f6afb6eb2a67a9310d0c57bdc9233bed7a356f10`.
- Real A/B Chat proved direct versus ambient delivery, concurrent decisions, silence without RoomEvent, busy deferral/coalescing, member rebuttal and depth-2 response, sender exclusion, terminal bounds, exact replay, and restart idempotency. The Phase 4.5 identity hash stayed `32a4b51a254f094df9fa724b247a87f06152ffef77dc354d585381dfd6cfdf89` across restart.
- `main` still projects as `xiaomi/mimo-v2.5-pro`; all new visible member replies are nonblank. The exact historical blank incident remains append-only for audit but is filtered from the Web timeline, so its obsolete `暂无内容` preview is no longer rendered.
- In-app desktop/mobile Browser verification remains host-blocked by missing `sandboxPolicy`; required bootstrap and troubleshooting calls both fail before navigation, and no external browser-control bypass was used.

### Invariants
- Receiving a room message and being admitted to a model reply are separate states; durable awareness must never imply that every member immediately consumes model/Scheduler/budget capacity.
- Every current non-archived room member can observe user and other-member messages; a member never responds to its own event, and sleeping/busy members retain an unread/deferred cursor without owning a live model connection.
- Explicit mention and user priority may request reconsideration at a safe checkpoint, but ordinary chat cannot hard-cancel an in-flight tool side effect. Interruption remains persisted, cooperative, and idempotent.
- Participation decisions are auditable and bounded by per-event response limits, debate rounds, duplicate/novelty suppression, cooldown, and room/task budgets so autonomous replies cannot self-amplify indefinitely.
- RoomEvent remains the sole durable conversation source of truth; TaskEngine remains the sole model execution/admission authority.
- Preserve the resolved `xiaomi/mimo-v2.5-pro` Web metadata and the non-empty successful-reply invariant.

### Delivery Impact
- This added one deliverable stage before the previously remaining Phase 5-8 work. Phase 4.5 is complete, so four stages now remain.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| Configuration search assumed a root `config.example.toml`, which does not exist | Phase 4.5 configuration audit | Use the implemented `CollaborationConfig`, its two Rust construction sites, and the approved design's embedded TOML as the configuration sources. |
| Four new group-deliberation contracts fail to compile with 29 missing methods/types/fields | Expected red-test phase | Implement schema v4 delivery/lineage/purpose state, group post/completion APIs, and snapshot/claim projections before rerunning. |
| First schema/repository compile left one recovery initializer incomplete and moved a service-event ID before cloning it | First implementation compile | Fill the compatibility initializer and clone the service event ID at construction; rerun into behavioral tests. |
| Cargo accepts only one positional test filter, so a command with separate repository/runtime filters was rejected | Combined focused compile | Use their shared `web::collaboration` prefix or run the two filters as separate commands. |
| A warning-cleanup patch renamed the first matching `b` fixture instead of the later unused binding | Group-history test compile | Restore the used concurrency fixture binding and rename only the debate-limit fixture with exact surrounding context. |
| First parallel static-gate orchestration script had a JavaScript parenthesis parse error before running commands | Static verification | Remove the Markdown-fence command from that batch, rerun the three code gates, and validate the document separately. |
| Workspace rustfmt check reported layout-only changes in the new collaboration/runtime code | Static verification | Run `cargo fmt --all`, then rerun the formatting check and focused tests. |
| Session recovery surfaced an unrelated historical GPT-model discussion | Phase 4.5 closeout resume | Ignore it and use the current Phase 4.5 plan, filesystem implementation, test evidence, and live database as authoritative. |
| Focused test compile found three stale `Option<String>::map(parse_datetime)` call signatures after timestamp parsing was changed to borrow | Warning-cleanup regression | Change those three optional timestamp mappings to closures that borrow their owned string, format, and rerun the focused suite. |
| Post-restart static probe requested unimplemented `/static/app.js` and received HTTP 404 | Live frontend verification | Read the served index asset path and retry against the actual bundled route; database/recovery checks already passed independently. |
| In-app Browser bootstrap and its required troubleshooting read are both rejected before execution because host metadata lacks `sandboxPolicy` | Desktop/mobile visual verification | Per the Browser skill, do not bypass with standalone Playwright; retain visual QA as environment-blocked and rely on live HTTP/WebSocket/SQLite plus static responsive checks. |
| Planning helper reports historical `0/17` against the accumulated multi-task plan | Final completion check | Its parser treats repeated completed historical task sections as one task; use the current top Phase 4.5 section (4/4) plus explicit gate/live evidence as authoritative. |

## Current Task: Phase 4 CollaborationRoom Completion (2026-07-24)

### Goal
Continue the approved migration by completing Phase 4 as a delta over the existing collaboration vertical slice: make room/member identity, authorization, lifecycle, recipient/inbox delivery, replay, recovery, and legacy migration production-complete while continuing to execute admitted member work through the durable Phase 3 TaskEngine.

### Required Regressions
- Web member metadata must keep exposing the resolved provider/model (`xiaomi / mimo-v2.5-pro` for the current `main` compatibility policy), never display the route alias `main` as the model.
- A successful member run must persist and replay a non-empty assistant reply; streaming, Artifact content, RoomEvent content, and preview output must agree.
- Create/Wake/configuration-only commands must not call a model or consume Scheduler/Worker/budget capacity.

### Phases
- [x] Close the remaining Phase 3 mechanical gates and establish the exact Phase 4 implementation-gap baseline.
- [x] Add focused failing contracts for the uncovered Phase 4 lifecycle, authorization, delivery, migration, and recovery behavior.
- [x] Implement the missing CollaborationRoom/InstanceDirectory/WebSocket production delta without creating a second execution or chat authority.
- [x] Verify member-lane concurrency, queued delivery, restart/cursor idempotency, compatibility routing, real Web behavior, and update architecture records.

### Phase 4 Closeout Checklist
- [x] Audit outbox emission on zero-row lease release, cancelled reconciliation, replay match arms, and legacy Query completion compatibility.
- [x] Build the current Web binary and migrate the live `runtime.db` to collaboration schema v3 without duplicating existing durable state.
- [x] Verify live snapshot metadata, sequence replay, stale-version rejection, outbox recovery, and restart/reconnect idempotency.
- [x] Synchronize the plan/findings/progress, architecture status, and the existing TaskEngine knowledge graph; run all final mechanical gates.

### Completion Evidence
- Collaboration repository 20/20, WebSocket 6/6, deterministic CLI 230/230, TaskEngine 14/14 plus strict Clippy, MainBrain 24/24, AgentRuntime 3/3, and `cargo check --workspace` all pass.
- Live `runtime.db` is schema v3; the v2 backup is retained, old Room/Member/Event/Inbox/Task/Run/Artifact identities are unchanged, Inbox refs are deterministically backfilled, and restart leaves zero active work/reservation and zero pending outbox.
- Live `/ws` resolves `main` as `xiaomi/mimo-v2.5-pro`, replays exact sequence ranges, rejects stale versions before sequence allocation, and reconnects without duplicate state. In-app desktop/mobile visual QA remains environment-blocked by missing host `sandboxPolicy`; no external browser bypass was used.

### Invariants
- `RoomEvent` plus explicit recipients is the sole chat source of truth; message append, recipient expansion, Inbox insertion, and runtime outbox publication share one short transaction.
- Durable `BrainMember` identity is separate from ephemeral `InstanceRun`; Sleeping/Archived/WaitingDependency/Queued members own no model connection, Worker permit, or Token reservation.
- Every command is authorized against current room membership/capabilities; Wake never trusts stale scopes and never invokes the model by itself.
- A member/thread lane serializes its own work without blocking independently admissible members; model/tool execution occurs outside SQLite transactions.
- Cursor replay and crash recovery are idempotent and do not duplicate RoomEvents, Inbox work, TaskRuns, Artifacts, or completed replies.
- Preserve unrelated dirty-worktree changes and all Phase 1-3 behavior.

### Current Baseline
- The existing vertical slice already has durable rooms, members, ordered events, Inbox claim leases, per-member execution lanes, WebSocket replay, group UI, restart recovery, and TaskEngine-backed model execution.
- The Phase 4 audit must therefore focus on missing design contracts rather than rebuilding those paths. Initial likely gaps are principal membership/capability persistence, explicit event-recipient/outbox ownership, summary cursors, complete member lifecycle/configuration/template commands, and legacy import fidelity; each will be confirmed against code and tests before editing.
- Phase 3 closeout gates pass: `cargo fmt --all -- --check`, Web JavaScript syntax, TaskEngine knowledge-graph JSON parsing, `git diff --check`, temporary smoke-script cleanup, and live root/listener verification on PID 67467.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| Session catchup returned an unrelated historical model discussion | Phase 4 startup | Ignore it and use the current plan, implementation, live database, and approved design as authoritative. |
| Initial combined Phase 4 planning patch assumed the wrong `findings.md` heading | Planning synchronization | The patch was rejected atomically; inspect the three real headings and apply exact, smaller patches. |
| Final service probe used an unimplemented `/health` route and received HTTP 404 | Phase 3 closeout | Keep the four successful code gates and temporary-file check; verify liveness through the served root URL and actual port listener instead. |
| Six new Phase 4 repository contracts fail with 41 missing-symbol/field errors | Expected red-test phase | Implement the tested v3 schema, authorization/version, outbox, replay/cursor, template, and thread-lane contracts before rerunning. |
| Independent-thread lease test was rejected by the fixture's two-item member backlog cap | First v3 repository run | Raise only the test fixture cap to three so the test reaches the thread-lane admission behavior; production defaults remain unchanged. |
| Focused v2 migration command used `--exact` without the module-qualified test name and ran zero tests | Migration verification | Treat the result as invalid and rerun with the unique test-name substring so the migration body actually executes. |
| Combined legacy Query handler patch assumed rustfmt line wrapping that did not match the file | Transport migration | The patch was rejected atomically; split Query/Cancel, completion, and disconnect cleanup into exact smaller patches. |
| Session recovery surfaced an unrelated historical GPT model discussion again | Phase 4 closeout resume | Ignore it; the current repository, Phase 4 records, live database, and implementation handoff remain authoritative. |
| SQLite CLI `-readonly` could not open the live database without sidecar access | Live v2 baseline audit | Use the explicit `file:...?...mode=ro&immutable=1` URI, which reads the stable database file without creating WAL/SHM files. |
| The first aggregate baseline query referenced nonexistent generic table `artifacts` | Live v2 baseline audit | Preserve the valid counts returned before the error; inspect `sqlite_master` and query the actual TaskEngine table names next. |
| URI `mode=ro` also returned `SQLITE_CANTOPEN` against the running migrated database | Live v3 audit | Stop trying read-only open modes; the database has no WAL/SHM, so use the previously proven normal SQLite CLI with SELECT/PRAGMA statements only. |
| First v3 metadata query used nonexistent `model_allowlist_json` | Live v3 audit | Core no-duplication counts were valid before the error; inspect table metadata and use the implemented names `allowed_model_policies_json` / `allowed_reasoning_depths_json`. |
| Combined Inbox task-reference patch missed rustfmt's exact `inbox_for_event` wrapping | Task-reference completion | Patch was rejected atomically; split schema/backfill, insertion, projection, and tests into exact smaller edits. |
| In-app Browser bootstrap is still rejected because host request metadata lacks `sandboxPolicy` | Final live visual verification | Per the required Browser skill, do not use standalone Playwright or another browser-control surface; retain visual QA as environment-blocked and finish HTTP/WebSocket/SQLite verification. |
| Planning helper reports historical `0/16` against the accumulated multi-task plan | Final completion check | Its parser does not understand repeated historical task sections; use the current top Phase 4 section (4/4 phases and 4/4 closeout items) plus explicit gate evidence. |

## Current Task: Phase 3 Durable TaskEngine And Unified Admission (2026-07-24)

### Goal
Continue the approved generic multi-instance migration with a production vertical slice of Phase 3: persist `TaskRun`/`TaskNode` DAG state, coordinate ready work through one admission boundary, and reserve/settle model budgets atomically without regressing the Phase 1/2 collaboration paths.

### Required Follow-Up From Earlier Phases
- Preserve the corrected Web model display: member metadata must expose the resolved provider/model (currently `xiaomi / mimo-v2.5-pro`), never the compatibility route alias `main`.
- Preserve the empty-response invariant: a successful member run cannot persist an empty assistant reply.

### Phases
- [x] Recover the exact Phase 3 state-machine, persistence, scheduling, budget, and recovery contracts; cache them in the feature knowledge graph.
- [x] Add focused failing tests for durable DAG/CAS transitions, atomic budget reserve/settle, unified admission, and restart recovery.
- [x] Implement the smallest production TaskEngine/Scheduler vertical slice and migrate one collaboration task path.
- [x] Verify focused behavior, restart/idempotency, Phase 1/2 regressions, workspace checks, and update records.

### Invariants
- `TaskRun` is the durable root for scheduling, budget, cancellation, audit, and the resolved configuration snapshot/hash; `TaskNode` is durable DAG state.
- State transitions use short transactions plus expected-version CAS; model calls never run while a database transaction or file lock is held.
- A single Scheduler admission decision covers global/provider/profile/task concurrency before work occupies a Worker; Workers do not nest semaphore acquisition.
- Budget is reserved before model execution and settled from authoritative usage afterward; failed/cancelled pre-execution work releases its reservation exactly once.
- Recovery never re-executes completed nodes and never treats an expired lease as proof that an external side effect is safe to repeat.
- Preserve unrelated dirty-worktree changes and existing Phase 1/2 behavior.

### Completion Evidence
- `task-engine` passes 14/14 contract tests and strict all-target Clippy; deterministic `ai-brain-cli` passes 218/218, `brain-main` 24/24, `agent-runtime` 3/3, and the workspace compiles.
- Live `runtime.db` migrated collaboration v1 to v2 and created the TaskEngine schema. Run `run-9aca677d-e054-4cd7-923f-67c3a64bd449` persisted a nonblank reply through TaskRun/Node/Instance/Artifact/Event state with concrete `xiaomi / mimo-v2.5-pro` metadata.
- The live reservation settled from Provider usage `15375 / 635`; task reserved counters returned to zero and consumed counters match exactly.
- A real service restart left one Instance, five Task events, one room reply, and zero Ready/Running nodes for the completed task. The read-only WebSocket replay still returned the nonblank reply and concrete model metadata.
- Desktop/mobile in-app visual verification remains environment-blocked before navigation because the host does not supply the browser plugin's required `sandboxPolicy` metadata. Protocol, persistence, static syntax, and responsive source checks remain available; no external browser bypass was used.

### Remaining Breadth Beyond This Vertical Slice
- Generic Workflow registration/UI for authoring arbitrary fan-out Reviewer/Synthesizer graphs and policy-driven configuration degradation remains subsequent incremental scope. The durable DAG, composite admission, budget, cancellation, recovery, event, and collaboration execution foundations are now production-backed.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| New post-execution budget tests fail because `TaskRepository::fail_node_after_execution` does not exist | Expected red-test phase | Implement the tested atomic failure settlement path while retaining `fail_node` for confirmed pre-execution release. |
| Package formatting check reported rustfmt-only layout changes in the new error-accounting code | Post-implementation format gate | Run package-scoped formatting, then re-run the check without touching unrelated crates. |
| In-app browser connection is rejected before page access because host metadata lacks required `sandboxPolicy` | Desktop/mobile live verification | Do not bypass the mandated browser surface with external Playwright; complete HTTP/WebSocket/SQLite verification and retain visual QA as an explicit environment-blocked item. |
| First live accounting audit referenced config hash on `task_runs` and parallel SQLite CLI readers observed a short write lock | Live database verification | Join `task_config_snapshots`, use SQLite CLI `.timeout 5000`, and run the audit serially; the corrected audit completed immediately. |
| Multi-call usage regression expected completion total 8 but current `MainBrainOutput` returned only the final call's 5 | Final accounting audit | Aggregate corrected usage and call counts across every tool-loop response and context-overflow re-entry before constructing `MainBrainOutput`. |
| Strict `brain-main` Clippy reaches 20+ existing lints in legacy conversation/tool-loop code | Final quality gate | Remove the two local compiler warnings in the touched function, keep unrelated lint cleanup out of scope, and rely on 24/24 tests plus workspace check; `task-engine` remains strict-Clippy clean. |
| First final-record `rg` command had an unmatched shell quote | Documentation audit | Correct the quoting and rerun the same read-only search successfully. |

## Current Task: Fix Empty Collaboration Member Reply (2026-07-24)

### Goal
Fix the Web collaboration path where asking `你能替我干什么？` completes a member run but renders `智脑 A 回复 / 暂无内容`; preserve the actual model answer from provider streaming through persistence, WebSocket replay, and preview rendering.

### Phases
- [x] Reproduce the defect and capture the authoritative provider/run/room-event state.
- [x] Trace the backend-to-frontend output contract and add a focused failing regression test.
- [x] Implement the smallest ownership-correct fix without inventing fallback answer text.
- [x] Run focused regressions and live HTTP/WebSocket verification; update records.

### Invariants
- A successful run must persist and replay the actual final answer; empty successful assistant events are invalid.
- Streaming deltas and the final persisted artifact/event must agree without duplicating content.
- Preserve member isolation, cancellation semantics, event ordering, and unrelated dirty-worktree changes.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| Session catchup returned an unrelated historical model discussion | Bug-fix startup | Ignore it and use the live service, persisted runtime database, and current repository as authoritative. |
| Member diagnostic query referenced a nonexistent `activity` column | Persisted-state inspection | Read the actual `brain_members` schema and rerun only with owned columns. |
| Initial exact timestamp/run-id log search returned no matches | Runtime trace inspection | Inspect the current log tail/format before choosing a new focused search pattern. |
| Combined planning patch contained an invalid empty hunk separator | Investigation record update | The patch failed atomically; remove the stray hunk marker and reapply the same scoped additions. |
| Two new tool-loop blank-response tests fail on the current behavior | Expected red-test phase | The failures prove the first blank response is returned as `Ok("")` and retry exhaustion is not enforced; implement the tested bounded recovery next. |
| Formatting check found two multiline match arms in the new retry reason mapping | Post-implementation format gate | Run package-scoped rustfmt, then rerun the check before tests. |
| In-app browser bootstrap is rejected before page access because `sandboxPolicy` metadata is missing | Live visual verification | Do not bypass the required browser surface; rely on the authoritative WebSocket event, SQLite row, HTTP response, and frontend contract, and record the environment limitation. |

## Current Task: Phase 2 AgentRuntime Extraction (2026-07-24)

### Goal
Continue the approved generic multi-instance migration by completing Phase 2: extract a reusable, bounded `AgentRuntime` from the current temporary/sub-agent and isolated member execution paths without changing authoritative workflow or domain ownership.

### Required Follow-Up From Phase 1
- The Web member roster/timeline currently displays the compatibility model policy as `main` instead of the actual resolved provider/model. Preserve this as an explicit Phase 1 defect and correct it during the model/profile metadata integration after the Phase 2 runtime boundary is established.

### Phases
- [x] Recover the exact Phase 2 contracts, current implementation baseline, and dirty-worktree ownership.
- [x] Define the smallest extraction boundary and add focused failing contract/runtime tests.
- [x] Implement `AgentRuntime` and migrate the applicable execution paths without broad domain rewrites.
- [x] Correct the Web model display to use server-resolved model metadata rather than the `main` route alias.
- [x] Run focused and regression verification; update the architecture/development records.

### Invariants
- Preserve all Phase 1 collaboration behavior and compatibility Web query behavior.
- Keep durable `BrainMember` identity separate from ephemeral runtime/model connections.
- Keep model calls outside database/file locks and execute each run from an immutable context snapshot.
- Do not revert or absorb unrelated dirty-worktree changes.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| Session catchup returned an unrelated historical model discussion | Resume startup | Ignore it and use the current repository, approved design, and existing planning records as authoritative. |
| Combined planning-file patch assumed the wrong `progress.md` heading | Resume planning write | The patch was rejected atomically; inspect the exact heading and update each planning file separately. |
| New `agent-runtime` contract tests failed with unresolved imports | Expected red test phase | The failure is limited to the intentionally empty new crate; implement the tested contracts next. |
| First implementation compile used a nonexistent `ConversationMessage::text_content()` in the new test | Contract test implementation | Inspect the public message shape and assert against `ContentBlock::Text`; product code already compiled. |
| First `tools` test compile referenced removed thread/mpsc injection seams | Adapter test migration | Replace them with preparation/profile, ArtifactSink persistence, and real mock `AgentRuntime` tests for Explore/Plan/Verification. |
| Migrated provider tests referenced `max_tokens_for_model` without the nested-module qualifier | Adapter test compile | Use the explicit `api::max_tokens_for_model` path. |
| Strict multi-package Clippy stopped in pre-existing `runtime/src/bash.rs` (`items_after_statements`) | Quality gate | Preserve unrelated code and rerun strict Clippy with `--no-deps` for the directly changed packages. |
| Strict `tools` Clippy surfaced 15 existing package lints plus one migrated-test lint | Quality gate | Fix the one Phase 2 `manual_let_else`; validate `agent-runtime`/`brain-llm` strictly and report the existing `tools` lint debt without unrelated cleanup. |
| Resume progress patch assumed the wrong Phase 1 follow-up heading | Final verification record | The patch failed atomically; inspect the exact Phase 2 section boundary and apply a smaller contextual patch. |
| In-app browser bootstrap again reports missing `sandboxPolicy` metadata | Desktop/mobile Web verification | Record the environment block, do not bypass the mandated browser surface, and complete the available HTTP/WebSocket/static checks. |
| Local Node 20 cannot resolve the `ws` package for the first WebSocket smoke approach | Live protocol verification | Do not retry that client; discover and use an already installed WebSocket client or a repository-native test path. |
| No `wscat`/`websocat` command or Python `websocket` module is installed | Live protocol verification | Keep the environment unchanged and search existing project/global JavaScript dependencies before falling back to focused Rust protocol coverage. |

## Current Task: Implement Collaboration Runtime Vertical Slice (2026-07-23)

### Goal
Implement the first production migration slice of the approved architecture: durable addressable BrainMember records, append-only room events, per-member inbox/claim semantics, bounded member execution, and a Web group-chat protocol/UI that permits independent members to work without the legacy session-wide query lock.

### Scope And Invariants
- Preserve current query behavior through a compatibility default room/member; do not attempt the entire Novel/Memory/Graph migration in one unsafe rewrite.
- Separate durable member identity from one execution; Create/Wake/Sleep/Archive must not create an LLM session or consume a worker.
- Persist room/member/event/inbox state server-side with idempotent commands and per-room ordering.
- Queue repeated work for the same member while allowing different members to execute independently under configured hard limits.
- Never mutate an in-flight member context with a later message; queued items start a new query context after the current run ends.
- Keep the existing dirty worktree and unrelated files intact.

### Phases
- [x] Map Web/Orchestrator concurrency, persistence, configuration, protocol, and frontend ownership; establish focused baseline tests.
- [x] Add collaboration domain contracts, persistence, member/inbox state transitions, capacity policy, and deterministic tests.
- [x] Integrate per-member workers and compatibility routing with the existing Orchestrator query path.
- [x] Extend WebSocket commands/events and implement the group-chat member roster/composer/timeline behavior.
- [x] Verify Rust, JavaScript, persistence/recovery, concurrent-member behavior, live WebSocket replay, and static responsive constraints.
- [ ] Complete desktop/mobile in-app browser visual verification when the environment supplies the required `sandboxPolicy` metadata.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| Session catchup returned an unrelated historical model discussion | Startup | Ignore it and use the approved design, current code, and dirty-worktree baseline as authoritative. |
| Initial focused Cargo test yielded a continuing PTY session after compilation output | Baseline verification | Poll the returned session with the command-session API; all 32 Web tests completed successfully. |
| First collaboration-store compile reported three identical end-of-block rusqlite statement borrow errors | Persistence implementation | Collect each mapped row iterator into a local Vec before returning so the iterator drops before its prepared statement. |
| `cargo fmt --all -- --check` reported style-only diffs in the new Orchestrator/collaboration code | Resume baseline | Run the workspace formatter before compilation, then re-run the check. |
| Resume notes referred to `web/api_server.rs`, but that file does not exist in the current tree | Web integration audit | Locate the current server composition module from `web/mod.rs` and `rg --files` before wiring state. |
| Combined protocol-test patch assumed the wrong end context for the recovery test | Contract tests | Patch progress, WebSocket, and repository tests separately at their exact current function boundaries; the failed patch was atomic and changed nothing. |
| In-app browser bootstrap failed because the tool request omitted required `sandboxPolicy` metadata | Live browser verification | Follow the browser skill troubleshooting path once; do not bypass it with external Playwright. Continue HTTP/WebSocket/static verification and report visual verification separately if metadata remains unavailable. |
| First command-line WebSocket smoke script assumed a Node global `WebSocket`, but this machine runs Node 20 without it | Live protocol smoke | Use an already installed WebSocket CLI/library if available; the failed script connected nowhere and changed no state. |
| Parallel full CLI/brain-main tests caused the pre-existing 1-second remote child-process test to time out (211/212 CLI tests still passed) | Full regression | Re-run the failing test alone, then run the deterministic CLI suite serially without competing Cargo processes. |

## Current Task: Addressable Instance Pool And Group Collaboration (2026-07-23)

### Goal
Extend the generic multi-instance development design with a Web group-collaboration model: users can create, mention, queue work for, sleep, and wake durable logical brain members while bounded ephemeral runs execute through the existing TaskEngine/Scheduler and collaborate through artifacts and dependency gates.

### Non-Negotiable Requirements
- Separate durable user-facing `BrainMember` identity from ephemeral `InstanceRun` and retry `InstanceAttempt`; sleeping members hold no model connection, worker, lock, or Token budget.
- Separate `InstanceDirectory` capacity from `WorkerPool` concurrency so many members may exist while only configured slots run.
- A group timeline is a server-authoritative event log, not automatically shared mutable LLM history; only addressed and policy-selected events enter an instance ContextSnapshot.
- `@member` routes to a durable per-member inbox. New input while a member is running queues by default and cannot mutate the current immutable snapshot; explicit interrupt is a separate command.
- Model cross-validation as dependency-gated, blind Reviewer runs over sealed artifacts; model unrelated parallel work as independent TaskNodes/TaskRuns followed by an explicit Synthesizer/Adjudicator.
- Persist room/member/task/run memory scopes distinctly so wake restores approved summaries and references without exposing other members' private state or hidden reasoning.
- Manual create/wake/model/depth controls remain subject to authorization, configured member/concurrency limits, and task/workspace Token budgets.
- Extend backend contracts, storage, WebSocket protocol, UI states, migration, recovery, tests, and acceptance criteria; do not present this as a frontend-only change.

### Phases
- [x] Compare the current temporary Agent/Web session implementation with the requested group collaboration behavior.
- [x] Define durable member, worker pool, room event, inbox, addressing, memory, and wake contracts.
- [x] Integrate the design across architecture, configuration, storage, API, migration, tests, and acceptance criteria.
- [x] Validate the revised document and preserve unrelated worktree changes.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| Session catchup returned an unrelated historical model discussion | Startup | Ignore it; use the current request, repository evidence, and existing target design as authoritative. |

## Current Task: Generalize Memory And Graph Design (2026-07-23)

### Goal
Refine the multi-instance architecture so Memory and Graph are fully domain-neutral platform capabilities. Novel must be only one consumer/adapter and must not appear in core memory/graph types, schemas, storage layout, query contracts, configuration, or lifecycle logic.

### Non-Negotiable Requirements
- Generic Memory/Graph core cannot depend on `novel-domain`, Novel workflow, Canon, project revisions, or publication types.
- Replace closed domain enums and hard-coded `project_id` fields with namespaced, schema-registered generic references.
- Preserve strong domain ownership: domain facts remain in each domain repository; Memory is advisory and Graph is a derived relationship/read model.
- Make cross-session and multi-instance isolation generic across user, workspace, conversation, task, instance, and custom domain scopes.
- Domain adapters translate committed events into generic memory/graph records without teaching the core about the domain.
- Novel, code, video, support, and other accepted knowledge must actually enter the shared Graph as schema-registered entities and relations; generic means the engine is domain-neutral, not that domain data stays out.
- Graph must maintain independently updatable evidence/location links from every entity or relation to the MemoryEntry, Artifact, Domain resource, ContextSnapshot, Conversation turn, or versioned workspace file where it is supported.
- Keep all prior concurrency, outbox, token-budget, and immutable-context guarantees.

### Phases
- [x] Audit the target document and current code for Novel-specific Memory/Graph coupling.
- [x] Define domain-neutral namespace, scope, schema, source-reference, query, and projection contracts.
- [x] Revise storage ownership, adapter boundaries, migration phases, tests, and acceptance criteria.
- [x] Correct Graph positioning as the first-class cross-domain entity/relation and evidence-location index.
- [x] Validate the updated development design and preserve previous review documents.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| Session catchup returned an unrelated historical model discussion | Startup | Ignore it and use the current request, repository, and completed target design as authoritative. |
| Initial read-only validation script contained Markdown fence backticks inside the tool's JavaScript template literal | Final validation | No files changed; rebuild the command without literal backticks and rerun once. |

## Current Task: Generic Multi-Instance Brain Development Design (2026-07-22)

### Goal
Design an implementable replacement for the special resident `NovelBrain`: a generic multi-instance agent runtime in which task-scoped instances load configurable profiles, prompts, context, tools, models, and reasoning depth, then collaborate or independently validate results while deterministic workflow and domain layers own state and authorization.

### Non-Negotiable Requirements
- Remove `NovelBrain` as a special runtime type without removing Novel project, Canon, candidate, review, approval, publication, and validation capabilities.
- Support both task decomposition and independent same-task candidate/verification modes.
- Keep instance contexts isolated; a user session may originate multiple instances but cannot be their shared mutable execution state.
- Design memory recall/save and graph query/projection with explicit authority, consistency, and provenance rules.
- Prevent parallel instances from holding database/file locks during model calls or causing unsafe concurrent project writes.
- Control token growth through configuration of instance count, role/model assignment, reasoning depth, context/output budgets, workflow budget, and degradation policy without architecture changes.
- Keep user approval and authoritative domain writes outside probabilistic agent decisions.

### Phases
- [x] Map reusable current runtime, memory, graph, configuration, and persistence boundaries.
- [x] Define target layers, ownership, task/instance/artifact contracts, and execution modes.
- [x] Design concurrency, memory, graph, transaction, consistency, and failure semantics.
- [x] Design hierarchical configuration, token/model budgets, scheduling, and observability.
- [x] Define migration phases, verification matrix, and export the development design document.

### Design Rules
- `AgentInstance` performs bounded computation; `TaskCoordinator` owns execution; domain services own facts and mutations.
- All model inputs are immutable context snapshots; all model outputs are immutable artifacts or structured findings.
- Recall and graph queries use snapshot/read-model semantics; writes occur through short deterministic commits, queues, or project-scoped serialization.
- Configuration changes capacity and policy, not component topology or data contracts.
- Cross-validation requires independence, evidence, and adjudication; it is not unrestricted agent-to-agent chat or majority voting.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| Session catchup returned an unrelated historical model discussion | Startup | Ignore it; preserve the worktree and use the current request plus repository state as authoritative. |
| Combined document refinement patch missed a Markdown list prefix | Consistency refinement | The patch was rejected atomically with no file changes; inspect exact contexts and apply smaller patches. |

## Current Task: Novel Brain Architecture Design Review (2026-07-22)

### Goal
Evaluate the NovelBrain architecture as a design, independent of implementation bugs: test whether its abstractions, responsibility boundaries, state ownership, collaboration model, aggregate boundaries, and lifecycle fit the novel-writing domain, then export a separate design critique and target model.

### Scope
- Preserve the implementation-risk audit as a separate follow-up document.
- Judge architectural concepts and dependency direction, not isolated code defects or line-level failure cases.
- Compare the intended resident design with the implemented component topology only to identify deliberate design choices.
- Answer what NovelBrain should be, whether it should be resident, what MainBrain/MemoryBrain should own, and how project/session/task/review/publication should compose.

### Phases
- [x] Separate the implementation audit from the requested design review and read the intended resident architecture.
- [x] Map responsibilities, state owners, aggregates, and collaboration boundaries.
- [x] Evaluate alternative architecture shapes and select a recommended target.
- [x] Export a design-focused architecture review without bug severity framing.

### Review Rules
- Organize conclusions by abstraction and responsibility, not by bug severity.
- Distinguish useful product metaphors from executable architecture boundaries.
- Prefer a smaller number of coherent components over adding more named brains.
- Preserve sound existing concepts when their ownership can be clarified.

### Errors Encountered
| Error | Attempt | Resolution |
|---|---|---|
| Initial legacy BrainAgent read used nonexistent `brain-core/src/brain.rs` | System architecture comparison | Use the located `brain-core/src/agent.rs`; the remaining parallel reads completed and no files changed. |

## Current Task: Novel Brain Architecture Problem Audit (2026-07-22)

### Goal
Audit the currently implemented AI-brain/NovelBrain architecture from live Rust code and tests, then export a severity-ranked design-problem report with evidence, impact, root cause, and remediation direction.

### Scope
- Read-only product-code review; do not modify runtime behavior.
- Treat executable code and tests as authoritative, using design documents only for intended contracts.
- Cover lifecycle, routing, state ownership, persistence/Canon boundaries, review/publication workflow, concurrency/revision safety, observability, configuration, and migration residue.

### Phases
- [x] Inventory current crates, design records, and NovelBrain entrypoints.
- [x] Trace end-to-end runtime, state machine, persistence, and publication paths.
- [x] Validate architectural claims against call sites, tests, and failure/concurrency behavior.
- [x] Export a severity-ranked architecture issue report and concise recommended target shape.

### Review Rules
- Distinguish confirmed defects from risks and intentional tradeoffs.
- Every high-confidence finding must cite a concrete file/line or missing enforcement point.
- Prioritize structural problems over style, naming, or speculative abstraction preferences.

### Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| Session catchup returned an unrelated historical model discussion | Startup | Ignore it; use the repository and the user's current request as authoritative. |
| Existing root planning files contain multiple completed tasks and are untracked | Startup | Preserve all content and prepend only a new task section for this audit. |
| Initial combined planning patch assumed the wrong `findings.md` heading | Planning write | No files changed; inspect exact headings and apply a corrected patch. |

## Current Task: Web Message Edit and Retry (2026-07-16)

### Goal
Add server-authoritative Web controls to edit any user message and retry only the final user message. Editing must fork the conversation at that turn, discard all later visible/runtime context and conversation-derived memory, then regenerate from the edited message. Retry must reuse the existing final user message without duplicating it.

### Phases
- [x] Trace Web session persistence, MainBrain history restoration, in-flight query ownership, and MemoryBrain writes.
- [x] Define the typed fork/retry protocol and scoped memory invalidation boundary.
- [x] Implement and test session/history/memory truncation plus regeneration.
- [x] Add inline edit and final-user retry controls to the Web UI.
- [x] Run focused Rust/JavaScript checks plus live HTTP/WebSocket protocol verification.
- [ ] Complete desktop/mobile in-app browser visual verification when browser sandbox metadata is available.

### Decisions
- Treat edit and retry as backend operations; the browser never mutates authoritative history on its own.
- An edit keeps the selected user turn with new content and removes everything after it before regeneration.
- A retry is valid only for the last visible user turn and reuses that stored turn rather than appending another user message.
- Reject edit/retry while generation is active unless the request first reaches a well-defined cancellation boundary.
- Invalidate only conversation-derived memory at and after the affected turn; preserve unrelated project Canon and durable novel artifacts.
- Assign every Web user turn a server-owned memory generation. Superseded L1 generations move to `l1-invalidated`; L2/L3/L4, profile, evaluation, pending-analysis, and graph-derived injection remain disabled until a revision-safe rebuild.
- A post-invalidation concentration is a clean rebuild from active L1 only: old L2/L3/L4/profile/evaluation values are never included in model prompts, and an empty active L1 clears every derived layer before staleness is released.
- Commit rebuilt graph projections and clear derived-memory staleness under the same invalidation lock; a newer edit revision prevents both operations.
- Preserve the edited/retried user message ID but issue a new memory generation ID, so the UI retains stable identity while delayed old-branch results cannot attach to the new branch.
- Cancel resident NovelBrain tasks sourced from invalidated generations only while they are still unpublished; completed tasks, confirmed Canon, and published artifacts remain durable.
- Guard query ownership per session across WebSocket clients and release it on success, failure, cancellation, or disconnect.

### Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| Session catchup returned unrelated historical model discussion | Task startup | Ignore it and use the current repository, user requirements, and existing planning records as authoritative. |
| In-app browser bootstrap failed because the environment omitted required `sandboxPolicy` metadata | Desktop/mobile visual verification | Do not bypass the browser skill with external automation; retain the live server and complete static, HTTP, WebSocket, and protocol checks. Visual screenshot verification remains explicitly deferred. |
| Final audit found that concentration still supplied old derived layers as model reference input, and an empty active L1 could leave them intact | Memory deletion hardening | Add an atomic rebuild snapshot and clean-rebuild mode that excludes all old derived inputs, clears derived storage when no L1 remains, and keeps legacy unscoped memory permanently fail-closed. |
| A concentration/edit race could keep staleness set but mirror an outdated L2 projection after graph invalidation | Revision-safe graph commit | Verify the rebuild revision, mirror L2, and clear staleness under one invalidation lock; skip the projection callback and report a conflict when the revision changed. |

## Current Task: Implement Resident Novel Brain (2026-07-16)

### Goal
Implement the approved resident NovelBrain architecture end to end: stable Orchestrator lifecycle, project-scoped sessions, MemoryBrain-owned lifecycle persistence, typed Main/User review gates, controlled publication, and removal of the ephemeral Novel Agent path.

### Current Phase
Resident NovelBrain implementation, transaction hardening, migration, and final verification are complete.

### Phases
- [x] Establish baseline and map exact compile/dependency boundaries.
- [x] Add MemoryBrain lifecycle/checkpoint/publication storage and tests.
- [x] Add resident `brain-novel` domain state machine, handle, runtime boundary, and tests.
- [x] Wire Orchestrator, RealToolExecutor, dedicated tools, configuration, status, and traces.
- [x] Update MainBrain prompt/skill and migrate/remove `Agent(Novel)` behavior.
- [x] Verify focused crates, deterministic regression suites, formatting, and workspace diff.

### Implementation Decisions
- Keep non-Novel generic agents unchanged.
- Use a typed resident service with one active mutable task per project.
- Persist every lifecycle transition through MemoryBrain; only accepted publication updates Confirmed Canon.
- Default publication requires Novel self-review, MainBrain pass, and User accept.
- Preserve existing Novel project JSON compatibility while adding checkpoints and publication journals.

### Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| Session catchup returned stale unrelated model discussion | Implementation startup | Ignore it; use the approved local design and current repository state. |
| Combined planning-file patch did not match `findings.md` heading | Initial implementation plan write | No files changed; split planning updates by file. |
| `cargo fmt -p brain-memory -- --check` reported style-only diffs | Memory phase verification | Ran package-scoped formatting; the subsequent format check, whitespace check, and all 195 library tests passed. |
| First `brain-novel` test compile missed a test-only `NovelTaskPhase` import | Resident runtime test pass | Add the missing import and remove one unused test import before rerunning; product code compiled successfully. |
| Initial combined RealToolExecutor patch missed the current field-comment context | Orchestrator/tool wiring | No partial edit occurred; split the change into exact imports/fields, dispatch, legacy-block, and helper patches. |
| Migrated Agent(Novel) rejection test used `expect_err` on non-Debug `AgentLaunch` | Tool migration tests | Use an explicit Result match; keep the production type unchanged. |
| Combined Delta/source-ref hardening patch referenced an actor test helper in the wrong file | Resident contract hardening | No changes were applied; split state, review, and actor-test updates by exact file. |
| Full parallel `cargo test -p tools --lib` stalled while one Agent test waited synchronously and other tests held/waited on the shared `env_lock` | Final regression | A serial reproduction showed the primary test launched two real foreground agents only to check normalization. Replace those calls with pure helper assertions; the complete serial suite now passes 48/48. |
| Focused Agent test command used `--exact` without the `tests::` module prefix and matched zero tests | Test-hang fix verification | Reran with the non-exact name filter; the intended `agent_persists_handoff_metadata` test executed and passed 1/1. |
| First completing serial `tools` run reported 44 passed / 4 failed | Final regression | One failure is cascading `env_lock` poison; inspect the three primary assertion mismatches against current code and Git baseline before making scoped corrections. |
| Second serial `tools` run reported 45 passed / 3 failed | Final regression | The direct `Skill` test contradicts the current RealToolExecutor dispatch boundary and panics before restoring temporary `HOME`; update that stale test, which also removes the two cascading API-key failures. |
| Deterministic `ai-brain-cli` suite reported 192 passed / 1 failed / 1 skipped | Final regression | Orchestrator initialization now owns five tasks after adding the resident NovelBrain actor, but the old test still expected four; update the lifecycle assertion and rerun the complete deterministic suite. |
| Package-scoped `cargo fmt --check` found three style-only diffs in corrected `tools` tests | Final formatting | Run formatting only for the six touched packages, then repeat formatting and whitespace checks. |
| Combined cross-platform lifecycle persistence patch missed the helper context | Final transaction hardening | No partial edit occurred; inspect exact current lines and split dependency, helper, and test updates. |
| `planning-with-files` completion helper reported `0/4` against the accumulated multi-task plan format | Final planning check | Current Resident NovelBrain section is manually verified 6/6 complete; preserve historical deferred tasks instead of rewriting them to satisfy the template parser. |

## Current Task: Resident Novel Brain Architecture Design (2026-07-15)

### Goal
Redesign the Novel brain as a resident v2 domain brain with project-scoped working state, while routing every durable recall/checkpoint/Canon write through the Memory brain instead of direct store paths.

### Non-Negotiable Requirements
- NovelBrain starts with Orchestrator and remains available across user turns.
- User communicates only with MainBrain; MainBrain coordinates and independently reviews NovelBrain work.
- NovelBrain keeps bounded per-project working state but does not own durable storage.
- MemoryBrain is the sole persistence boundary for working checkpoints, approved artifact refs/episodes, Canon deltas, and graph projection; project files remain the artifact source of truth.
- Project isolation, revision safety, and save-before-Canon-commit ordering must be enforced in Rust, not only in prompts.
- By default, a MainBrain-reviewed draft is shown to the user and requires user acceptance before publication and Confirmed Canon commit.

### Phases
- [x] Inspect reusable resident-agent, dispatch, and MemoryBrain interfaces.
- [x] Define component ownership, typed ports, lifecycle, and per-project state model.
- [x] Define user/MainBrain/NovelBrain review and persistence state machine.
- [x] Define migration from `Agent(subagent_type=Novel)` and direct NovelMemoryStore access.
- [x] Write the repository design document with implementation phases and tests.

### Decisions
- Design for the v2 MainBrain runtime; do not attach the new brain to the legacy v1 query bus merely to achieve residency.
- Keep the existing Novel project snapshot as Canon initially, but make it an internal MemoryBrain implementation detail.
- Preserve unrelated working-tree changes; this turn produces architecture documentation, not runtime implementation.
- Reuse `ConversationRuntime` semantics for bounded multi-turn project sessions, but route its durable snapshots through MemoryBrain rather than direct session files.
- Construct the resident brain's client with `LlmConfig::create_brain_client("novel")` so provider, model, max tokens, and temperature obey the Novel configuration as one unit.
- Publish exact reviewed content through a scoped atomic artifact port; MemoryBrain records the transaction, artifact ref/hash, episode, Canon delta, and graph projection.
- Keep one stable NovelBrain identity with project-partitioned workspaces; residency is service-level, while cold project sessions may be checkpointed and evicted under a context budget.
- Treat NovelBrain output as a candidate, MainBrain review as an internal quality gate, and user acceptance as the default publication authority.
- Record the full lifecycle in MemoryBrain with explicit status: raw request/draft/review/feedback events are not Canon; only accepted publication facts become Confirmed Canon.

### Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| Session catchup again reported unrelated model-discussion context | Initial recovery | Ignore stale external context and continue from the completed local Novel audit. |
| Combined planning-file patch did not match `findings.md` heading context | First planning update | No files changed; split the update into exact per-file patches. |

## Current Task: Novel Brain Architecture Audit (2026-07-15)

### Goal
Explain the current implemented novel-brain architecture and the collaboration contract among the user, main brain, and novel brain, grounded in live code rather than design intent alone.

### Phases
- [x] Inspect the completed novel-brain refactor plan and identify claimed runtime boundaries.
- [x] Trace user input from CLI/Web entrypoints through the orchestrator and main brain.
- [x] Trace novel-brain routing, context construction, tool/skill use, persistence, and result return.
- [x] Compare design records with executable code and tests; identify gaps or transitional paths.
- [x] Deliver a concise architecture explanation with source references and a sequence diagram.

### Decisions
- Treat source code and tests as authoritative; use plan documents only to explain intent and recent changes.
- Preserve all unrelated working-tree changes and make no product-code modifications for this read-only audit.
- Model the Novel brain as a constrained synchronous sub-agent reached through the general `Agent` tool, not as a peer process with independent repository authority.
- Separate code-enforced isolation/concurrency guarantees from LLM-enforced review and persistence ordering in the final explanation.

### Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| Session catchup contained unrelated prior ChatGPT/model discussion | Initial recovery check | Ignore the stale context and preserve existing planning files; proceed from the current repository state. |

## Current Task: Secure Tailscale Remote Access (2026-07-14)

### Target Revision: Windows Host + Phone Client

The user clarified that Windows, not macOS, is the always-on AI Brain host. The phone is the remote browser client. The secure architecture remains Tailscale Serve -> Windows loopback Web UI, but host discovery, documentation, and verification must cover Windows explicitly.

#### Windows Phases
- [x] Re-read the current remote implementation and preserve prior work.
- [x] Confirm official Windows Tailscale install/CLI/Serve behavior.
- [x] Add Windows executable discovery and platform-specific setup guidance.
- [x] Update documentation for a Windows host and phone client.
- [x] Stop the Mac cross-target download at the user's request; defer Windows compile/runtime checks to the actual Windows host.
- [x] Write a standalone Windows host setup, test, phone-verification, and Codex handoff runbook.
- [ ] Run native Windows build/runtime verification later from the Windows workspace.

### Goal
Add a safe, practical remote mode that keeps AI Brain bound to localhost and publishes it through Tailscale Serve for access from a phone or another trusted device.

### Scope
- Confirm the current Tailscale Serve and macOS setup flow from official documentation.
- Add a CLI remote mode with dependency/status checks and a clear access URL.
- Keep the Axum service on loopback and refuse unsafe remote bindings in this mode.
- Manage the Tailscale Serve child process/lifecycle without exposing a public port.
- Add focused Rust tests and user documentation.
- Build and smoke-test the local Web UI; verify it in the in-app browser when available.

### Phases
- [x] Inspect existing Web/Tailscale state and preserve unrelated work.
- [x] Confirm official Tailscale installation and Serve behavior.
- [x] Design and implement the remote-mode CLI integration.
- [x] Add tests and documentation.
- [x] Build, run, and verify local Web startup behavior. (Live Tailscale endpoint pending login.)
- [x] Install and launch Tailscale on the host; surface the required local onboarding step.
- [x] macOS onboarding is no longer required for the clarified Windows-host deployment; the installed Mac client remains optional.

### Decisions
- Tailscale is the network trust boundary; AI Brain must continue listening only on loopback.
- Do not implement router port forwarding or bind the unauthenticated Web app to `0.0.0.0`.
- Preserve the existing `web` command for ordinary local use; introduce a distinct remote workflow so its security intent is explicit.

### Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| `tailscale` command not found | Initial host inspection | Tailscale is not installed; installation/setup remains a planned phase. |
| Over-constrained Firecrawl query returned no results | Official documentation lookup | Switched to a broader query and will filter the returned sources to official `tailscale.com` documentation. |
| Initial package formatting check reported style-only diffs | First verification pass | Apply package-scoped `cargo fmt`, then rerun the check before compilation. |
| Cargo rejected two positional test filters | First targeted test attempt | Use one `remote` filter, which selects both `remote_access::tests` and `remote_policy_tests`. |
| Homebrew installation reached an administrator-password prompt | Tailscale host setup | Cancelled the terminal password prompt; opened the verified `.pkg` so authentication stays in macOS UI. |
| GUI Installer remained open without completing | Tailscale host setup | Switch to macOS's native privileged AppleScript installer prompt, which requires only local Touch ID/password approval. |
| Embedded macOS Tailscale CLI probe hung before network approval | Post-install inspection | Stop only the probe process; leave the installed app and macOS authorization UI running for user approval. |
| In-app browser bootstrap failed due missing `sandboxPolicy` metadata | Web smoke test | Use the repository's previously successful fallback: installed Playwright with system Chrome, without changing app code. |
| First grid-track fix left a 5.6px right-column overlap | Geometry regression pass | Increase only the second desktop track minimum to 364px, preserving the mobile Flex override. |
| Installed macOS app CLI blocks while onboarding is incomplete | Live Tailscale status probe | Terminated only the probe; system extension and `/usr/local/bin/tailscale` are installed, and user login/approval remains the required next step. |
| `tailscale login` remained blocked during incomplete macOS onboarding | Live login attempt | Stop the waiting process and harden AI Brain CLI discovery/status calls with a bounded timeout and onboarding-specific guidance. |
| PATH resolved the macOS shell wrapper before the App CLI | Final diff review | Prefer the real app-bundle executable on macOS so timeout termination targets the actual CLI process. |
| Windows Rust target download was slow and no longer useful | Mac cross-target setup | User requested native Windows work instead; terminated rustup cleanly and left only the native Mac target installed. |

## Goal
Build an operational cockpit and conversation trace that expose the real runtime data flow: directional links with arrows, full brain-to-brain and main-brain-to-sub-agent task/result exchanges, streaming main-brain reasoning and intermediate conclusions, and collapsible reasoning/tool details.

## Scope
- Inspect current Web UI structure and runtime event data.
- Design one pragmatic cockpit page/view using the existing static HTML/CSS/JS stack.
- Add navigation so chat and cockpit are separate modes.
- Render cockpit sections for system health, brain nodes, communication flow, agent status, and recent events.
- Use live session/progress data where available, with sensible fallback/demo state when runtime data is sparse.
- Verify layout in browser or via static checks.
- Add session deletion entry in the sidebar.
- Add memory brain and knowledge graph read/write monitoring in cockpit.
- Emit structured communication events with stable exchange IDs and full task/result content.
- Render full communication exchanges in cockpit with expandable detail.
- Add directional arrowheads and clearer active-flow styling to the topology.
- Render streaming reasoning, intermediate conclusions, and tool calls/results as independently collapsible conversation trace blocks.
- Persist paired runtime exchanges in Web sessions so full sub-brain results survive reloads.
- Group all tool calls from one user turn behind one collapsed disclosure.

## Phases
- [x] Confirm current workspace state and relevant files.
- [x] Inspect existing Web UI and WebSocket event model.
- [x] Implement dedicated cockpit view and navigation.
- [x] Add responsive cockpit styling.
- [x] Run formatting/static validation and browser smoke test where feasible.
- [x] Summarize changed files and remaining gaps.
- [x] Add delete-session UI.
- [x] Add memory/graph read-write cockpit monitoring.
- [x] Re-run static and server checks.
- [x] Inspect main-brain, secondary-brain, sub-agent, and tool event paths.
- [x] Extend the shared progress/WebSocket protocol for intermediate conclusions and full communication exchanges.
- [x] Update the cockpit topology and communication detail UI.
- [x] Update the conversation trace UI and visibility interactions.
- [x] Add/update Rust and frontend behavior tests.
- [x] Run Rust verification and browser smoke tests.
- [x] Persist and restore brain communication exchanges in chat history.
- [x] Replace per-tool top-level rows with one collapsed tool group per turn.
- [x] Re-run persistence, browser, and responsive verification.

## Decisions
- Keep implementation in the existing static app rather than introducing a frontend framework.
- Separate cockpit from chat through top-level view switching.
- Favor dense operational UI over marketing-style visuals.
- Structured full-content exchanges require backend events; cockpit state must not infer authoritative task/result payloads from display text.
- Reuse one communication event shape for brain modules and dynamic sub-agents.
- Keep raw model reasoning opt-in/collapsible in the UI and label intermediate status summaries separately.
- Use an orchestrator-level broadcast channel for communication exchanges so background-agent completions remain observable after the query progress channel closes.
- Use a delegation exchange ID generated before agent launch, then pair both synchronous and background completion events with that same ID.
- Keep dynamic agents as distinct cockpit participants while retaining aggregate core brain nodes.
- Generate the delegation exchange ID before invoking `Agent`, so synchronous and background requests appear immediately and pair with later responses.

## Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| In-app browser plugin failed with missing `sandboxPolicy` metadata | Browser verification | Used local service checks, JS syntax validation, DOM/CSS consistency checks, and attempted Python Playwright fallback. |
| Python Playwright browser binary missing | Browser verification | Attempted `playwright install chromium`; download was too slow and was interrupted. Static/runtime HTTP checks passed. |
| Initial planning patch context mismatch | Follow-up planning update | Re-applied the update against the exact current checklist wording. |
| Initial multi-file test patch context mismatch | Test coverage update | Re-applied against the exact existing agent test assertions. |
| `cargo fmt --all` cannot parse `rusty-claude-cli/src/menu.rs:436` | Required workspace formatting | Treat as a pre-existing unrelated blocker and format only the crates changed by this task. |
| Hidden cockpit produced SVG `NaN` coordinates | First Playwright browser pass | Skip topology rendering while the map or endpoint nodes have zero dimensions. |
| Final visual script expected an unsimulated client link | Latest-direction verification | Corrected expected path count from three to the two exchanges actually injected. |
| Workspace clippy fails in `brain-hooks` and existing CLI lint debt | Required clippy gate | Ran package-targeted tests and captured the unrelated workspace lint failures without changing those modules. |
| Workspace tests cannot import `ai_brain_cli` in `v2_integration_test.rs` | Required workspace test gate | Verified the bin unit suite and touched library crates independently; left the pre-existing target-layout issue untouched. |
| Long chat traces were compressed to 2px and overlapped adjacent messages | Chat exchange mouse-toggle test | Disabled flex shrinking for direct message-list children so the message list owns vertical scrolling. |
| Remote advanced after the task commit, leaving local ahead 6 / behind 1 | Patch export | Replayed non-merge local commits onto the latest remote in an isolated worktree and verified the exported patch series by clean `git am`; did not run the export script's destructive reset branch. |
| Full `ai-brain` bin suite timed out in `orchestrator::tests::test_orchestrator_query` | Full regression run | 174/175 tests passed; re-run the external-model integration test separately to distinguish network latency from a code regression. |
| Historical session backfill `jq` duration expression had incorrect precedence | First backfill attempt | Restored the untouched `.bak` copy immediately, reran with `set -e` and explicit parentheses, then validated the 9 messages and 4701-character response before restart. |
| In-app browser lacked required `sandboxPolicy` metadata | Browser verification | Used installed Playwright 1.60 with system Chrome 149 and blocked only external CDN assets; desktop and 390px responsive checks passed. |

## Current Task: LLM Provider 单次调用自动重试（2026-08-09）

### Goal

为 OpenAI 兼容与 Gemini 的普通/流式请求增加统一的结构化自动重试：额外重试 5 次，
只重发当前失败的 Provider 调用，不触碰现有 `retry_last_user_message` 整轮用户重试。

### Phases

- [x] 诊断生产连接重置未重试的根因并确认需求边界。
- [x] 完成并提交设计 `dc16714b`。
- [x] 编写详细 TDD 实施计划。
- [x] 以 RED 测试建立共享错误分类与默认策略合同。
- [x] 实现 OpenAI 兼容普通完成的当前调用级重试。
- [x] 实现 Gemini 普通完成的当前调用级重试。
- [ ] 实现批量流和首事件前增量流重试。
- [ ] 运行定向、回归和仓库门禁。
- [ ] 重建并重启智脑，验证 HTTP/日志和本地故障注入行为。

### Decisions

- 主检出中的 `openai_compat.rs` 含用户未提交 `.no_proxy()` 改动；功能在隔离 worktree 实现，最终合并时保留该行。
- 网络错误在仍为 `reqwest::Error` 时通过 typed API/错误源链分类；禁止字符串包含判断。
- HTTP 只重试 408/429/500/502/503/504，并保留确定性模型路由 503 排除。
- 批量流可丢弃未公开事件并重试；增量流仅在首个事件交付前重试，交付后错误通过 `StreamEvent::Error` 显式报告且不重发。
- 用户手动重试、主脑空响应重试、任务恢复和小说工作流重试均保持不变。

### Errors Encountered

| Error | Attempt | Resolution |
|---|---|---|
| 现有增量流在后台 chunk 失败时只写日志并静默关闭 receiver，没有错误事件 | 实施规划审计 | 在 `brain-llm::StreamEvent` 增加内部统一的 `Error` 事件，保持现有 wildcard 消费者兼容，并用测试证明部分输出后不重发。 |
| 首次创建实施计划的补丁在命令代码块处缺少 `+` 前缀，被 `apply_patch` 原子拒绝 | 实施计划写入 | 记录失败并改用完整带前缀的 Add File 补丁；没有计划文件被部分创建。 |
| typed 断连 RED 测试使用 `Client::new()` 时被系统代理接管，本地断连变成 HTTP 502 | Task 1 RED | 证据是返回 remote proxy 的 502 而非 reqwest error；测试客户端显式 `.no_proxy()`，只修正故障注入边界后重跑 RED。 |
