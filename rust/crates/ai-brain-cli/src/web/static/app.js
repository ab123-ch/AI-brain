// ── State ───────────────────────────────────────────────────────
let ws = null;
let reconnectAttempts = 0;
const MAX_RECONNECT = 5;
const HEARTBEAT_INTERVAL_MS = 25000; // 25秒心跳，服务端30秒Ping互补
let heartbeatTimer = null;
let showThinking = false;
let currentStreamingEl = null;
let currentThinkingEl = null;
let currentThinkingDetails = null;
let currentToolGroup = null;
let currentSpinnerEl = null;
let activeSessionId = null;
let isGenerating = false; // 是否正在生成回复
let isComposing = false;  // IME 组合态标记（中文输入法）
let currentView = 'chat';
let sessionCache = [];
let sessionFiles = [];
let currentTurnFiles = [];
let turnFileBaseline = new Map();
let openedLocalFile = null;
let openedLocalFileTrigger = null;
let fileSaveTimer = null;
let fileSavePromise = null;
let fileEditorWatcherTimer = null;
let fileEditorConflictSnapshot = null;
let fileEditorDirty = false;
let fileEditorRequestId = 0;
let fileEditorMode = 'edit';
let fileEditorSupportsMarkdown = false;
let currentPreview = null;
let currentPreviewSelection = null;
const toolItems = new Map();
const chatExchangeItems = new Map();
const expandedCommunicationIds = new Set();
const collapsedCommunicationIds = new Set();
let roomSnapshot = null;
let roomMode = 'chat';
let replyState = null;
let pendingRoomPost = null;
let pendingRoomOperation = null;
let authoritativeRoomEventSequence = 0;
let authoritativeRoomSnapshotVersion = 0;
let authoritativeRoomStateRevision = 0;
let hasEarlierRoomEvents = false;
let hasLoadedEarlierRoomEvents = false;
let preserveTimelineAnchor = false;
let pendingRoomDirectoryUpdate = null;
const selectedMemberIds = new Set();
const memberRunStates = new Map();
const roomMarkdownCache = new Map();
const runContentRenderState = new WeakMap();
const pendingRunRenderStates = new Set();
const ROOM_MARKDOWN_CACHE_LIMIT = 400;
let pendingRunFollowLatest = false;
let roomTimelineNearBottom = true;
const modelCatalog = window.ModelCatalog;

// ── Typewriter State ──────────────────────────────────────────
let thinkingFullContent = '';
let thinkingDisplayPos = 0;
let thinkingRaf = null;
const CHARS_PER_FRAME = 3;

// ── DOM References ──────────────────────────────────────────────
const $messages = document.getElementById('messages');
const $input = document.getElementById('input');
const $sendBtn = document.getElementById('send-btn');
const $personaSelect = document.getElementById('persona-select');
const $sessionList = document.getElementById('session-list');
const $fileEditorToolbar = document.getElementById('file-editor-toolbar');
const $fileEditorModeSwitch = document.getElementById('file-editor-mode-switch');
const $fileEditorPreviewMode = document.getElementById('file-editor-preview-mode');
const $fileEditorEditMode = document.getElementById('file-editor-edit-mode');
const $fileEditorStatus = document.getElementById('file-editor-status');
const $fileEditorQuote = document.getElementById('file-editor-quote');
const $fileEditorContent = document.getElementById('file-editor-content');
const $fileEditorRefresh = document.getElementById('file-editor-refresh');
const $fileEditorCopyPath = document.getElementById('file-editor-copy-path');
const $fileEditorTabs = document.getElementById('file-editor-tabs');
const $fileEditorTab = document.getElementById('file-editor-tab');
const $fileEditorTabName = document.getElementById('file-editor-tab-name');
const $fileEditorSaveState = document.getElementById('file-editor-save-state');
const $fileEditorConflict = document.getElementById('file-editor-conflict');
const $fileEditorKeepLocal = document.getElementById('file-editor-keep-local');
const $fileEditorLoadDisk = document.getElementById('file-editor-load-disk');
const $fileEditorFrame = document.getElementById('file-editor-frame');
const $fileEditorLineNumbers = document.getElementById('file-editor-line-numbers');
const $fileEditorStatusbar = document.getElementById('file-editor-statusbar');
const $fileEditorRevision = document.getElementById('file-editor-revision');
const $fileEditorCaret = document.getElementById('file-editor-caret');
const $fileEditorLines = document.getElementById('file-editor-lines');
const $fileEditorLineEnding = document.getElementById('file-editor-line-ending');
const $fileEditorLanguage = document.getElementById('file-editor-language');
const $sidebar = document.getElementById('sidebar');
const $sidebarToggle = document.getElementById('sidebar-toggle');
const $newSessionBtn = document.getElementById('new-session-btn');
const $thinkingToggle = document.getElementById('thinking-toggle');
const $askModal = document.getElementById('ask-modal');
const $askQuestion = document.getElementById('ask-question');
const $askOptions = document.getElementById('ask-options');
const $chatTab = document.getElementById('chat-tab');
const $cockpitTab = document.getElementById('cockpit-tab');
const $chatArea = document.getElementById('chat-area');
const $cockpitArea = document.getElementById('cockpit-area');
const $brainNodes = document.getElementById('brain-nodes');
const $brainLinks = document.getElementById('brain-links');
const $brainEventList = document.getElementById('brain-event-list');
const $cockpitSummary = document.getElementById('cockpit-summary');
const $cockpitReset = document.getElementById('cockpit-reset');
const $cockpitConnection = document.getElementById('cockpit-connection');
const $cockpitRuntime = document.getElementById('cockpit-runtime');
const $metricActiveBrains = document.getElementById('metric-active-brains');
const $metricTools = document.getElementById('metric-tools');
const $metricMemory = document.getElementById('metric-memory');
const $metricGraphReads = document.getElementById('metric-graph-reads');
const $metricGraphWrites = document.getElementById('metric-graph-writes');
const $metricErrors = document.getElementById('metric-errors');
const $communicationList = document.getElementById('communication-list');
const $conclusionTabs = document.getElementById('conclusion-tabs');
const $conclusionList = document.getElementById('conclusion-list');
const $conclusionCount = document.getElementById('conclusion-count');
const $agentStatusList = document.getElementById('agent-status-list');
const $memoryGraphList = document.getElementById('memory-graph-list');
const $markdownPreview = document.getElementById('markdown-preview');
const $previewBackdrop = document.getElementById('preview-backdrop');
const $previewClose = document.getElementById('preview-close');
const $previewTitle = document.getElementById('preview-title');
const $previewSource = document.getElementById('preview-source');
const $previewContent = document.getElementById('preview-content');
const $previewSelectionTools = document.getElementById('preview-selection-tools');
const $previewSelectionInfo = document.getElementById('preview-selection-info');
const $previewQuote = document.getElementById('preview-quote');
const $memberList = document.getElementById('member-list');
const $memberCapacity = document.getElementById('member-capacity');
const $workerStatus = document.getElementById('worker-status');
const $workerStatusText = document.getElementById('worker-status-text');
const $addMemberBtn = document.getElementById('add-member-btn');
const $roomTitle = document.getElementById('room-title');
const $roomSequence = document.getElementById('room-sequence');
const $roomRefreshBtn = document.getElementById('room-refresh-btn');
const $roomWorkingDirectory = document.getElementById('room-working-directory');
const $roomDirectoryBtn = document.getElementById('room-directory-btn');
const $loadEarlierEvents = document.getElementById('load-earlier-events');
const $roomMode = document.getElementById('room-mode');
const $recipientSelector = document.getElementById('recipient-selector');
const $replyPreview = document.getElementById('reply-preview');
const $replyPreviewSender = document.getElementById('reply-preview-sender');
const $replyPreviewContent = document.getElementById('reply-preview-content');
const $replyCancel = document.getElementById('reply-cancel');
const $memberModal = document.getElementById('member-modal');
const $memberForm = document.getElementById('member-form');
const $memberModalTitle = document.getElementById('member-modal-title');
const $memberModalClose = document.getElementById('member-modal-close');
const $memberModalCancel = document.getElementById('member-modal-cancel');
const $memberId = document.getElementById('member-id');
const $memberVersion = document.getElementById('member-version');
const $memberName = document.getElementById('member-name');
const $memberModel = document.getElementById('member-model');
const $memberDepth = document.getElementById('member-depth');
const $roomDirectoryModal = document.getElementById('room-directory-modal');
const $roomDirectoryForm = document.getElementById('room-directory-form');
const $roomDirectoryInput = document.getElementById('room-directory-input');
const $roomDirectoryModalClose = document.getElementById('room-directory-modal-close');
const $roomDirectoryModalCancel = document.getElementById('room-directory-modal-cancel');
const $roomDirectorySubmit = document.getElementById('room-directory-submit');
const $toastRegion = document.getElementById('toast-region');
const composerUsesNativeSizing = typeof CSS !== 'undefined'
    && typeof CSS.supports === 'function'
    && CSS.supports('field-sizing', 'content');
const composerResizeScheduler = RoomReply.createFrameScheduler(
    resizeComposerNow,
    (callback) => requestAnimationFrame(callback),
    (frameId) => cancelAnimationFrame(frameId),
);
const runRenderScheduler = RoomReply.createFrameScheduler(
    flushPendingRunRenders,
    (callback) => requestAnimationFrame(callback),
    (frameId) => cancelAnimationFrame(frameId),
);

// ── Cockpit State ───────────────────────────────────────────────
const brainProfiles = {
    client: { label: '用户端', role: '输入 / WebSocket 会话', icon: 'U' },
    instance: { label: '成员实例', role: '独立任务 / 交叉验证', icon: 'I' },
    main: { label: '主脑', role: '任务规划 / 汇总输出', icon: 'M' },
    memory: { label: '记忆脑', role: '历史召回 / 上下文注入', icon: 'R' },
    graph: { label: '知识图谱', role: '语义节点 / 关系读写', icon: 'G' },
    eval: { label: '评估脑', role: '质量检查 / 结果评估', icon: 'E' },
    tool: { label: '工具执行器', role: '本地命令 / 文件 / 外部工具', icon: 'T' },
    agent: { label: '子代理池', role: '并行专题处理', icon: 'A' },
    novel: { label: '小说脑', role: '常驻创作 / 自检 / 修订', icon: 'N' },
    explore: { label: '探索脑', role: '检索 / 方案探索', icon: 'X' },
};

const brainState = {
    nodes: {},
    links: [],
    events: [],
    communications: [],
    conclusions: [],
    conclusionBrain: 'all',
    memoryGraphOps: [],
    agents: {},
    metrics: {
        toolCalls: 0,
        memoryRefs: 0,
        graphReads: 0,
        graphWrites: 0,
        errors: 0,
        retries: 0,
    },
    connection: 'offline',
    activeSince: null,
    currentModel: '',
    activePersona: '',
    activeSessionTitle: '',
};

// ── WebSocket ───────────────────────────────────────────────────
function connect() {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    setConnectionState('connecting');
    ws = new WebSocket(`${proto}//${location.host}/ws`);

    ws.onopen = () => {
        reconnectAttempts = 0;
        console.log('WebSocket connected');
        setConnectionState('online');
        addBrainEvent('WebSocket 已连接');
        startHeartbeat();
        if (activeSessionId && roomSnapshot?.room?.room_id === activeSessionId) {
            send('join_room', {
                room_id: activeSessionId,
                after_sequence: Number(roomSnapshot.room.latest_event_seq || 0),
            });
        }
    };

    ws.onmessage = (e) => {
        try {
            const data = JSON.parse(e.data);
            handleServerMessage(data);
        } catch (err) {
            console.error('Parse error:', err);
        }
    };

    ws.onclose = () => {
        console.log('WebSocket closed');
        stopHeartbeat();
        failPendingRoomOperation();
        setConnectionState('offline');
        addBrainEvent('WebSocket 已断开，等待重连');
        if (reconnectAttempts < MAX_RECONNECT) {
            const delay = Math.pow(2, reconnectAttempts) * 1000;
            reconnectAttempts++;
            setTimeout(connect, delay);
        }
    };

    ws.onerror = (err) => {
        console.error('WebSocket error:', err);
        brainState.metrics.errors += 1;
        setConnectionState('error');
        addBrainEvent('WebSocket 连接异常');
    };
}

function send(type, data = {}) {
    if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type, ...data }));
        return true;
    }
    return false;
}

// ── Heartbeat ────────────────────────────────────────────────────
function startHeartbeat() {
    stopHeartbeat();
    heartbeatTimer = setInterval(() => {
        send('heartbeat');
    }, HEARTBEAT_INTERVAL_MS);
}

function stopHeartbeat() {
    if (heartbeatTimer) {
        clearInterval(heartbeatTimer);
        heartbeatTimer = null;
    }
}

// ── Message Handlers ────────────────────────────────────────────
function handleServerMessage(data) {
    switch (data.type) {
        case 'connecting':
            brainState.currentModel = data.model || '';
            brainState.activeSince = brainState.activeSince || Date.now();
            markBrain(data.brain, 'active', `连接模型 ${data.model}`);
            updateAgentStatus(normalizeBrainKey(data.brain), '连接中', data.model || '模型连接中');
            addCommunication('client', normalizeBrainKey(data.brain), `连接 ${data.model}`, 'connect');
            addBrainEvent(`${displayBrainName(data.brain)} 连接 ${data.model}`);
            removeSpinner();
            currentSpinnerEl = addSystemMessage(`连接 ${data.brain} (${data.model})`);
            break;

        case 'thinking':
            brainState.activeSince = brainState.activeSince || Date.now();
            markBrain(data.brain, 'active', '思考中');
            updateAgentStatus(normalizeBrainKey(data.brain), '工作中', '思考中');
            addBrainEvent(`${displayBrainName(data.brain)} 思考中`);
            removeSpinner();
            currentSpinnerEl = addSpinner(`${data.brain} 思考中...`);
            break;

        case 'text_delta':
            markBrain('main', 'active', '输出回复');
            updateAgentStatus('main', '输出中', '正在生成回复');
            removeSpinner();
            appendStreamingText(data.text);
            break;

        case 'thinking_delta':
            appendThinking(data.content);
            break;

        case 'intermediate_conclusion':
            addIntermediateConclusion(data.brain, data.content);
            markBrain(data.brain, 'active', data.content);
            addBrainEvent(`${displayBrainName(data.brain)}: ${truncate(data.content, 120)}`);
            break;

        case 'tool_start':
            trackToolStart(data.brain, data.tool_name, data.input);
            addToolStart(data.call_id, data.brain, data.tool_name, data.input);
            break;

        case 'tool_done':
            trackToolDone(data.brain, data.tool_name, data.duration_ms, data.is_error);
            updateToolDone(data.call_id, data.tool_name, data.duration_ms, data.output_preview, data.is_error);
            break;

        case 'brain_communication':
            handleBrainCommunication(data.exchange);
            break;

        case 'room_snapshot':
            applyRoomSnapshot(data.snapshot);
            break;

        case 'room_event_appended':
            mergeRoomEvent(data.event);
            break;

        case 'room_events_replayed':
            if (data.room_id === activeSessionId) {
                (data.events || []).forEach((event) => mergeRoomEvent(event));
            }
            break;

        case 'room_events_loaded_before':
            handleRoomEventsLoadedBefore(data);
            break;

        case 'room_message_accepted':
            handleRoomMessageAccepted(data);
            break;

        case 'room_working_directory_accepted':
            handleRoomWorkingDirectoryAccepted(data);
            break;

        case 'member_changed':
            mergeMember(data.member);
            break;

        case 'inbox_item_changed':
            mergeInboxItem(data.room_id, data.item);
            break;

        case 'member_run_progress':
            handleMemberRunProgress(data);
            break;

        case 'member_run_finished':
            handleMemberRunFinished(data);
            break;

        case 'memory_injected':
            markBrain('memory', 'active', `召回 ${data.count} 条记忆`);
            brainState.metrics.memoryRefs += Number(data.count || 0);
            setBrainLink('memory', 'main', `注入 ${data.count} 条记忆`);
            addMemoryGraphOp('memory', 'read', `召回 ${data.count} 条记忆`, data.preview || '');
            updateAgentStatus('memory', '已注入', truncate(data.preview || '', 80));
            addBrainEvent(`记忆脑向主脑注入 ${data.count} 条记忆`);
            addMemoryIndicator(data.count, data.preview);
            break;

        case 'evaluation_start':
            markBrain('eval', 'active', '开始评估');
            setBrainLink('main', 'eval', '提交评估');
            updateAgentStatus('eval', '工作中', '开始评估');
            addBrainEvent('评估脑开始评估');
            addSystemMessage('评估脑开始评估...');
            break;

        case 'evaluation_result':
            markBrain('eval', data.passed ? 'done' : 'error', data.feedback);
            if (!data.passed) brainState.metrics.errors += 1;
            updateAgentStatus('eval', data.passed ? '通过' : '未通过', data.feedback || '');
            addBrainEvent(`评估脑完成: ${data.passed ? '通过' : '未通过'}`);
            const icon = data.passed ? '[PASS]' : '[FAIL]';
            addSystemMessage(`评估结果 ${icon}: ${data.feedback}`);
            break;

        case 'evaluating':
            markBrain('eval', 'active', '评估中');
            updateAgentStatus('eval', '工作中', '评估中');
            removeSpinner();
            currentSpinnerEl = addSpinner('评估脑评估中...');
            break;

        case 'llm_retry':
            brainState.metrics.retries += 1;
            addBrainEvent(`LLM 重试 ${data.attempt}/${data.max_attempts}: ${data.error}`);
            addSystemMessage(`LLM 重试 (${data.attempt}/${data.max_attempts}): ${data.error}`);
            renderCockpit();
            break;

        case 'ask_user':
            showAskModal(data.question, data.options);
            break;

        case 'done':
            finishActiveBrains();
            finalizeStreaming();
            removeSpinner();
            isGenerating = false;
            brainState.activeSince = Object.values(brainState.nodes).some((node) => node.pending)
                ? brainState.activeSince
                : null;
            setInputEnabled(true);
            updateSendButton();
            break;

        case 'final_answer':
            renderAuthoritativeFinalAnswer(data.content);
            markBrain('main', 'done', '最终回复已送达');
            updateAgentStatus('main', '已完成', '最终回复已展示');
            addBrainEvent('主脑最终回复已送达');
            break;

        case 'session_list':
            sessionCache = data.sessions || [];
            renderSessionList(data.sessions);
            updateActiveSessionTitle();
            renderCockpit();
            break;

        case 'session_switched':
            activeSessionId = data.session_id;
            roomSnapshot = null;
            resetRoomLocalState();
            selectedMemberIds.clear();
            memberRunStates.clear();
            $input.value = '';
            resetComposerHeight();
            closeRoomDirectoryModal();
            sessionFiles = data.files || [];
            currentTurnFiles = [];
            turnFileBaseline = new Map(sessionFiles.map((file) => [file.path, file.updated_at]));
            renderSessionList(sessionCache);
            updateActiveSessionTitle();
            renderMessages(data.messages);
            renderCockpit();
            break;

        case 'session_files_updated':
            if (data.session_id === activeSessionId) {
                const nextFiles = data.files || [];
                currentTurnFiles = nextFiles.filter((file) => turnFileBaseline.get(file.path) !== file.updated_at);
                sessionFiles = nextFiles;
            }
            break;

        case 'session_messages_updated':
            if (data.session_id === activeSessionId) {
                renderMessages(data.messages);
            }
            break;

        case 'persona_list':
            renderPersonaList(data.personas, data.active);
            brainState.activePersona = personaLabel(data.personas, data.active);
            renderCockpit();
            break;

        case 'persona_switched':
            brainState.activePersona = $personaSelect.options[$personaSelect.selectedIndex]?.textContent || data.persona_id;
            addBrainEvent(`人格切换为 ${brainState.activePersona}`);
            renderCockpit();
            addSystemMessage(`已切换人格: ${data.persona_id}`);
            break;

        case 'error':
            if (!RoomReply.shouldHandleRoomError(data, activeSessionId)) break;
            brainState.metrics.errors += 1;
            addBrainEvent(`错误: ${data.message}`);
            if (roomSnapshot?.room?.room_id === activeSessionId) {
                failPendingRoomOperation(data);
                showToast(data.message);
                break;
            }
            markBrain('main', 'error', data.message);
            addSystemMessage(`错误: ${data.message}`);
            removeSpinner();
            isGenerating = false;
            setInputEnabled(true);
            updateSendButton();
            break;

        default:
            console.log('Unknown message type:', data.type, data);
    }
}

// ── Collaboration Room ─────────────────────────────────────────
function resetRoomLocalState() {
    runRenderScheduler.cancel();
    pendingRunRenderStates.clear();
    pendingRunFollowLatest = false;
    roomTimelineNearBottom = true;
    roomMarkdownCache.clear();
    const reset = RoomReply.resetRoomUiState();
    replyState = reset.replyState;
    pendingRoomPost = reset.pendingRoomPost;
    pendingRoomOperation = reset.pendingRoomOperation;
    authoritativeRoomEventSequence = reset.authoritativeRoomEventSequence;
    authoritativeRoomSnapshotVersion = reset.authoritativeRoomSnapshotVersion;
    authoritativeRoomStateRevision = reset.authoritativeRoomStateRevision;
    hasEarlierRoomEvents = reset.hasEarlierRoomEvents;
    hasLoadedEarlierRoomEvents = reset.hasLoadedEarlierRoomEvents;
    preserveTimelineAnchor = reset.preserveTimelineAnchor;
    pendingRoomDirectoryUpdate = null;
    renderReplyPreview();
    updateRoomOperationControls();
}

function beginPendingRoomOperation(operation) {
    const result = RoomReply.beginRoomOperation(pendingRoomOperation, operation);
    pendingRoomOperation = result.pending;
    if (result.started) updateRoomOperationControls();
    return result.started;
}

function settlePendingRoomOperation(event) {
    const result = RoomReply.settleRoomOperation(pendingRoomOperation, event);
    if (result.settled) {
        pendingRoomOperation = result.pending;
        updateRoomOperationControls();
    }
    return result;
}

function failPendingRoomOperation(error = null) {
    const scopedError = error || RoomReply.buildRoomOperationError(pendingRoomOperation);
    const result = settlePendingRoomOperation(scopedError);
    if (!result.settled) return result;
    if (result.operationType === 'post') {
        pendingRoomPost = null;
    } else if (result.operationType === 'directory' && pendingRoomDirectoryUpdate) {
        pendingRoomDirectoryUpdate.failed = true;
    }
    return result;
}

function updateRoomOperationControls() {
    const controls = RoomReply.roomOperationControls(pendingRoomOperation);
    $roomDirectoryBtn.disabled = controls.directoryDisabled;
    $roomDirectorySubmit.disabled = controls.directoryDisabled;
    $loadEarlierEvents.disabled = controls.paginationDisabled;
    updateSendButton();
}

function applyRoomSnapshot(snapshot) {
    if (!snapshot?.room || snapshot.room.room_id !== activeSessionId) return;
    const sameRoom = roomSnapshot?.room?.room_id === snapshot.room.room_id;
    if (!RoomReply.shouldApplyRoomSnapshot(sameRoom ? {
        roomId: snapshot.room.room_id,
        stateRevision: authoritativeRoomStateRevision,
        version: authoritativeRoomSnapshotVersion,
        eventSequence: authoritativeRoomEventSequence,
    } : null, snapshot)) return;
    const authoritativeEvents = snapshot.events || [];
    const snapshotEventSequence = Number(snapshot.room.latest_event_seq || 0);
    const events = sameRoom
        ? RoomReply.mergeSnapshotWindow(
            roomSnapshot.events,
            authoritativeEvents,
            snapshotEventSequence,
            hasLoadedEarlierRoomEvents,
        )
        : [...authoritativeEvents];
    hasEarlierRoomEvents = RoomReply.resolveHasEarlierEvents({
        snapshotHasEarlier: snapshot.has_earlier_events,
        currentHasEarlier: hasEarlierRoomEvents,
        hasLoadedEarlier: sameRoom && hasLoadedEarlierRoomEvents,
    });
    roomSnapshot = {
        ...snapshot,
        room: {
            ...snapshot.room,
            latest_event_seq: sameRoom
                ? Math.max(
                    snapshotEventSequence,
                    Number(roomSnapshot.room.latest_event_seq || 0),
                )
                : snapshotEventSequence,
        },
        members: snapshot.members || [],
        events,
        inbox: snapshot.inbox || [],
    };
    authoritativeRoomEventSequence = snapshotEventSequence;
    authoritativeRoomSnapshotVersion = Number(snapshot.room.version || 0);
    authoritativeRoomStateRevision = Number(snapshot.room.state_revision || 0);
    replyState = RoomReply.reconcileReplyState(replyState, roomSnapshot.events);
    roomSnapshot.members.forEach((member) => {
        ensureBrainNode(member.member_id, {
            label: member.display_name,
            role: '独立成员实例',
            icon: member.display_name.slice(0, 1).toUpperCase(),
        });
        markBrain(
            member.member_id,
            member.activity === 'running' ? 'active' : member.activity === 'failed' ? 'error' : 'idle',
            memberActivityLabel(member),
        );
    });
    reconcileSelectedMembers();
    renderCollaborationRoom({ scrollToLatest: true });
    setInputEnabled(true);
    updateRoomOperationControls();
}

function reconcileSelectedMembers() {
    if (!roomSnapshot) return false;
    const next = MentionRecipients.recipientMemberIds($input.value, roomSnapshot.members);
    const changed = next.size !== selectedMemberIds.size
        || [...next].some((memberId) => !selectedMemberIds.has(memberId));
    selectedMemberIds.clear();
    next.forEach((memberId) => selectedMemberIds.add(memberId));
    return changed;
}

function renderCollaborationRoom({ scrollToLatest = false } = {}) {
    if (!roomSnapshot) return;
    $roomTitle.textContent = roomSnapshot.room.title || '当前会话';
    $roomSequence.textContent = `${roomSnapshot.room.latest_event_seq || 0} 条事件`;
    $roomWorkingDirectory.textContent = roomSnapshot.room.working_directory || '';
    $roomWorkingDirectory.title = roomSnapshot.room.working_directory || '';
    $memberCapacity.textContent = `${roomSnapshot.members.filter((member) => member.availability !== 'archived').length} / ${roomSnapshot.max_members}`;
    renderEarlierEventsControl();
    renderReplyPreview();
    renderMemberList();
    renderRecipientSelector();
    renderRoomTimeline({ scrollToLatest });
}

function renderEarlierEventsControl() {
    $loadEarlierEvents.classList.toggle('hidden', !hasEarlierRoomEvents);
}

function renderReplyPreview() {
    $replyPreview.classList.toggle('hidden', !replyState);
    if (!replyState) {
        $replyPreviewSender.textContent = '';
        $replyPreviewContent.textContent = '';
        return;
    }
    $replyPreviewSender.textContent = `回复 ${replyState.sender_name || '消息'}`;
    $replyPreviewContent.textContent = truncate(String(replyState.content || ''), 160);
}

function renderMemberList() {
    $memberList.innerHTML = '';
    if (!roomSnapshot) return;
    const members = [...roomSnapshot.members].sort((left, right) => {
        if (left.availability === 'archived' && right.availability !== 'archived') return 1;
        if (left.availability !== 'archived' && right.availability === 'archived') return -1;
        return String(left.created_at).localeCompare(String(right.created_at));
    });
    let runningCount = 0;
    members.forEach((member) => {
        if (member.activity === 'running') runningCount += 1;
        const row = document.createElement('div');
        const stateClass = member.activity === 'running'
            ? 'running'
            : member.activity === 'failed'
                ? 'failed'
                : member.availability === 'active' ? 'active' : '';
        row.className = `member-row ${stateClass} ${member.availability === 'archived' ? 'archived' : ''} ${selectedMemberIds.has(member.member_id) ? 'selected' : ''}`;
        row.dataset.memberId = member.member_id;
        const selectable = member.availability === 'active';
        const modelDetail = resolvedMemberModel(member);
        const modelLabel = modelDetail
            ? modelCatalog.compactLabel(modelDetail)
            : member.model_policy;
        const modelTitle = modelDetail
            ? modelPolicyTitle(modelDetail)
            : member.model_policy;
        row.innerHTML = `
            <input class="member-select" type="checkbox" aria-label="选择 ${escapeHtml(member.display_name)}" ${selectedMemberIds.has(member.member_id) ? 'checked' : ''} ${selectable ? '' : 'disabled'}>
            <div class="member-copy">
                <div class="member-name-line"><span class="member-state-dot"></span><strong>${escapeHtml(member.display_name)}</strong></div>
                <div class="member-meta"><span>${escapeHtml(memberActivityLabel(member))}</span><span title="${escapeHtml(modelTitle)}">${escapeHtml(modelLabel)}</span><span>${escapeHtml(member.reasoning_depth)}</span>${member.pending_count ? `<span>${member.pending_count} 待处理</span>` : ''}</div>
            </div>
            <div class="member-actions">${memberActionButtons(member)}</div>
        `;
        row.querySelector('.member-select').addEventListener('change', (event) => {
            setMemberSelected(member.member_id, event.target.checked);
        });
        row.querySelectorAll('[data-member-action]').forEach((button) => {
            button.addEventListener('click', () => {
                handleMemberAction(button.dataset.memberAction, member);
            });
        });
        $memberList.appendChild(row);
    });
    $workerStatus.classList.toggle('busy', runningCount > 0);
    $workerStatusText.textContent = `${runningCount} 个运行中 · 上限 ${roomSnapshot.max_workers}`;
    refreshIcons();
}

function resolvedMemberModel(member) {
    return (roomSnapshot?.model_policy_details || [])
        .find((policy) => policy.policy_id === member.model_policy) || null;
}

function modelPolicyTitle(policy) {
    return [
        policy.label || policy.model || policy.policy_id,
        policy.provider,
        policy.model,
        policy.policy_id,
    ].filter(Boolean).join(' · ');
}

function memberActionButtons(member) {
    if (member.availability === 'archived') {
        return '<button class="member-action" type="button" data-member-action="restore" title="恢复成员" aria-label="恢复成员"><i data-lucide="archive-restore"></i></button>';
    }
    const buttons = [];
    if (member.activity === 'running' && member.active_run_id) {
        buttons.push('<button class="member-action danger" type="button" data-member-action="interrupt" title="停止当前运行" aria-label="停止当前运行"><i data-lucide="square"></i></button>');
    } else if (member.availability === 'active') {
        buttons.push('<button class="member-action" type="button" data-member-action="sleep" title="休眠成员" aria-label="休眠成员"><i data-lucide="moon"></i></button>');
    } else {
        buttons.push('<button class="member-action" type="button" data-member-action="wake" title="唤醒成员" aria-label="唤醒成员"><i data-lucide="play"></i></button>');
    }
    buttons.push('<button class="member-action" type="button" data-member-action="configure" title="成员设置" aria-label="成员设置"><i data-lucide="settings-2"></i></button>');
    if (member.activity !== 'running') {
        buttons.push('<button class="member-action danger" type="button" data-member-action="archive" title="归档成员" aria-label="归档成员"><i data-lucide="archive"></i></button>');
    }
    return buttons.join('');
}

function memberActivityLabel(member) {
    if (member.activity === 'running') return '运行中';
    if (member.activity === 'queued') return '排队中';
    if (member.activity === 'failed') return '上次失败';
    if (member.availability === 'sleep_after_current') return '完成后休眠';
    if (member.availability === 'sleeping') return '休眠';
    if (member.availability === 'archived') return '已归档';
    return '空闲';
}

function renderRecipientSelector() {
    $recipientSelector.innerHTML = '';
    if (!roomSnapshot) return;
    roomSnapshot.members
        .filter((member) => member.availability !== 'archived')
        .forEach((member) => {
            const option = document.createElement('button');
            option.type = 'button';
            option.className = `recipient-option ${selectedMemberIds.has(member.member_id) ? 'selected' : ''}`;
            option.disabled = member.availability !== 'active';
            option.innerHTML = `<span class="member-state-dot"></span><span>@${escapeHtml(member.display_name)}</span>`;
            option.addEventListener('click', () => {
                setMemberSelected(member.member_id, !selectedMemberIds.has(member.member_id));
            });
            $recipientSelector.appendChild(option);
        });
}

function resizeComposerNow() {
    if (composerUsesNativeSizing) {
        $input.style.height = '';
        return;
    }
    $input.style.height = 'auto';
    $input.style.height = `${Math.min($input.scrollHeight, 120)}px`;
}

function scheduleComposerResize() {
    if (!composerUsesNativeSizing) composerResizeScheduler.schedule();
}

function resetComposerHeight() {
    composerResizeScheduler.cancel();
    $input.style.height = '';
    scheduleComposerResize();
}

function setMemberSelected(memberId, selected) {
    const member = roomSnapshot?.members.find((candidate) => candidate.member_id === memberId);
    if (!member || member.availability !== 'active') return;
    $input.value = selected
        ? MentionRecipients.appendMention($input.value, member.display_name)
        : MentionRecipients.removeMention($input.value, member.display_name);
    scheduleComposerResize();
    reconcileSelectedMembers();
    renderMemberList();
    renderRecipientSelector();
    $input.focus();
}

function syncMentionRecipients() {
    if (reconcileSelectedMembers()) {
        renderMemberList();
        renderRecipientSelector();
    }
}

function renderRoomTimeline({ scrollToLatest = false } = {}) {
    if (!roomSnapshot) return;
    const viewport = RoomReply.captureTimelineViewport($messages);
    $messages.innerHTML = '';
    const entries = [];
    const lastVisibleUserEventId = roomSnapshot.events
        .filter((event) => event.kind === 'user_message' && event.sender_kind === 'user')
        .at(-1)?.event_id || null;
    roomSnapshot.events
        .filter((event) => event.kind !== 'member_message' || String(event.content || '').trim())
        .forEach((event) => {
            entries.push({ kind: 'event', at: Date.parse(event.created_at) || 0, value: event });
        });
    roomSnapshot.inbox
        .filter((item) => ['leased', 'running', 'failed', 'cancelled'].includes(item.state) && item.run_id)
        .forEach((item) => {
            entries.push({
                kind: 'run',
                at: Date.parse(item.started_at || item.completed_at || item.created_at) || 0,
                value: item,
            });
        });
    entries.sort((left, right) => left.at - right.at || (left.kind === 'event' ? -1 : 1));
    entries.forEach((entry) => {
        if (entry.kind === 'event') {
            renderRoomEvent(entry.value, entry.value.event_id === lastVisibleUserEventId);
        }
        else renderRunItem(entry.value);
    });
    if (entries.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'service-event';
        empty.textContent = '暂无消息';
        $messages.appendChild(empty);
    }
    refreshIcons();
    const scrollPolicy = preserveTimelineAnchor
        ? 'prepend'
        : scrollToLatest ? 'latest' : 'follow-if-near-bottom';
    $messages.scrollTop = RoomReply.timelineScrollTarget(
        viewport,
        $messages.scrollHeight,
        scrollPolicy,
    );
    roomTimelineNearBottom = scrollPolicy === 'latest'
        || (scrollPolicy === 'follow-if-near-bottom' && viewport.nearBottom);
}

function renderCachedRoomMarkdown(element, event) {
    const content = String(event.content || '');
    const cached = roomMarkdownCache.get(event.event_id);
    if (cached?.content === content) {
        element.innerHTML = cached.html;
        return;
    }
    renderMarkdown(element, content);
    roomMarkdownCache.set(event.event_id, { content, html: element.innerHTML });
    while (roomMarkdownCache.size > ROOM_MARKDOWN_CACHE_LIMIT) {
        roomMarkdownCache.delete(roomMarkdownCache.keys().next().value);
    }
}

function renderRoomEvent(event, isLastUserEvent = false) {
    if (event.kind !== 'user_message' && event.kind !== 'member_message') {
        const service = document.createElement('div');
        service.className = 'service-event';
        service.textContent = `${event.content} · ${formatRoomTime(event.created_at)}`;
        $messages.appendChild(service);
        return;
    }
    const message = document.createElement('div');
    message.className = `msg ${event.sender_kind === 'user' ? 'user' : 'assistant'}`;
    const metadata = document.createElement('div');
    metadata.className = 'msg-meta';
    const recipientNames = (event.recipients || [])
        .map((memberId) => roomSnapshot.members.find((member) => member.member_id === memberId)?.display_name)
        .filter(Boolean);
    const audienceCount = Array.isArray(event.audience) ? event.audience.length : 0;
    const audienceMeta = event.group_enabled && audienceCount
        ? `<span class="msg-audience" title="已投递给 ${audienceCount} 个智脑"><i data-lucide="users"></i>${audienceCount}</span>`
        : '';
    metadata.innerHTML = `<strong>${escapeHtml(event.sender_name)}</strong><span>${escapeHtml(formatRoomTime(event.created_at))}</span>${recipientNames.length ? `<span>→ ${escapeHtml(recipientNames.join('、'))}</span>` : ''}${audienceMeta}`;
    const content = document.createElement('div');
    content.className = 'msg-content';
    if (event.sender_kind === 'member') {
        renderCachedRoomMarkdown(content, event);
        attachPreviewAction(message, `${event.sender_name} 回复`, event.sender_name, event.content);
        addBrainConclusion(event.member_id || event.sender_id, event.content, `${event.sender_name} 回复`, `room-${event.event_id}`, false);
    } else {
        content.textContent = event.content;
    }
    message.appendChild(metadata);
    if (event.reply_reference) {
        message.appendChild(createRoomReplyReference(event.reply_reference));
    }
    message.appendChild(content);
    if (event.sender_kind === 'member') {
        appendModifiedFiles(message, event.changed_files || [], event.run_id);
    }
    const actions = document.createElement('div');
    actions.className = 'msg-actions';
    if (RoomReply.canReplyToEvent(event)) {
        actions.appendChild(createMessageAction('reply', '回复这条消息', () => {
            beginRoomReply(event);
        }));
    }
    if (isLastUserEvent) {
        actions.appendChild(createMessageAction('rotate-ccw', '重试最后一条消息', () => {
            roomSnapshot.events = roomSnapshot.events.filter(
                (candidate) => candidate.sequence <= event.sequence,
            );
            roomSnapshot.inbox = [];
            replyState = RoomReply.reconcileReplyState(replyState, roomSnapshot.events);
            memberRunStates.clear();
            renderCollaborationRoom({ scrollToLatest: true });
            showToast('正在重试最后一条消息');
            send('retry_last_user_message', { message_id: event.event_id });
        }));
    }
    if (actions.childElementCount > 0) {
        message.classList.add('has-actions');
        message.appendChild(actions);
    }
    $messages.appendChild(message);
}

function createRoomReplyReference(reference) {
    const card = document.createElement('div');
    card.className = 'room-reply-reference';
    const sender = document.createElement('strong');
    sender.textContent = `回复 ${reference.sender_name || '消息'}`;
    const body = document.createElement('span');
    body.textContent = String(reference.content || '');
    card.appendChild(sender);
    card.appendChild(body);
    return card;
}

function beginRoomReply(event) {
    const selection = RoomReply.beginReply(event, roomSnapshot.members);
    replyState = selection.reply;
    if (selection.auto_recipient_id) {
        setMemberSelected(selection.auto_recipient_id, true);
    }
    if (selection.warning) showToast(selection.warning);
    renderReplyPreview();
    $input.focus();
}

function renderRunItem(item) {
    const member = roomSnapshot.members.find((candidate) => candidate.member_id === item.member_id);
    const state = ensureMemberRunState(item.run_id, item.member_id, roomSnapshot.room.room_id);
    state.status = item.state;
    state.purpose = item.purpose || state.purpose;
    state.error = item.error || state.error;
    const message = document.createElement('div');
    message.className = `msg assistant run-message ${state.status}`;
    message.dataset.runId = item.run_id;
    message.innerHTML = `
        <div class="msg-meta"><strong>${escapeHtml(member?.display_name || item.member_id)}</strong><span>${escapeHtml(formatRoomTime(item.started_at || item.created_at))}</span></div>
        <div class="run-status"><span>${escapeHtml(runStatusLabel(state))}</span>${item.state === 'running' ? '<button class="run-cancel" type="button" title="停止运行" aria-label="停止运行"><i data-lucide="square"></i></button>' : ''}</div>
        <div class="run-content"></div>
        <div class="run-log ${state.logs.length ? '' : 'hidden'}"></div>
    `;
    state.element = message;
    message.querySelector('.run-cancel')?.addEventListener('click', () => {
        send('interrupt_member_run', {
            member_id: item.member_id,
            run_id: item.run_id,
            expected_version: Number(item.version),
        });
    });
    $messages.appendChild(message);
    updateRunElement(state);
}

function ensureMemberRunState(runId, memberId, roomId) {
    if (!memberRunStates.has(runId)) {
        memberRunStates.set(runId, {
            runId,
            memberId,
            roomId,
            status: 'running',
            statusText: '',
            rawText: '',
            logs: [],
            error: null,
            purpose: null,
            element: null,
            markdownFinalized: false,
        });
    }
    return memberRunStates.get(runId);
}

function updateRunElement(state) {
    const element = state.element;
    if (!element?.isConnected) return;
    element.className = `msg assistant run-message ${state.status}`;
    const status = element.querySelector('.run-status > span');
    if (status) status.textContent = runStatusLabel(state);
    const content = element.querySelector('.run-content');
    if (content) {
        const nextText = state.rawText || state.error || '';
        const rendered = runContentRenderState.get(content);
        if (rendered?.text !== nextText || rendered.markdown !== state.markdownFinalized) {
            if (state.rawText && state.markdownFinalized) renderMarkdown(content, state.rawText);
            else content.textContent = nextText;
            runContentRenderState.set(content, {
                text: nextText,
                markdown: state.markdownFinalized,
            });
        }
    }
    const log = element.querySelector('.run-log');
    if (log) {
        log.textContent = state.logs.slice(-5).join('\n');
        log.classList.toggle('hidden', state.logs.length === 0);
    }
}

function scheduleRunElementUpdate(state, followLatest) {
    pendingRunRenderStates.add(state);
    pendingRunFollowLatest ||= followLatest;
    runRenderScheduler.schedule();
}

function flushPendingRunRenders() {
    const states = [...pendingRunRenderStates];
    const followLatest = pendingRunFollowLatest;
    pendingRunRenderStates.clear();
    pendingRunFollowLatest = false;
    states.forEach((state) => {
        if (state.roomId === activeSessionId) updateRunElement(state);
    });
    if (followLatest && roomTimelineNearBottom && !preserveTimelineAnchor) {
        $messages.scrollTop = $messages.scrollHeight;
        roomTimelineNearBottom = true;
    }
}

function runStatusLabel(state) {
    if (state.statusText) return state.statusText;
    if (state.purpose === 'participation') {
        if (state.status === 'leased') return '等待查看群聊';
        if (state.status === 'running') return '判断是否参与';
    }
    if (state.status === 'leased') return '等待调度';
    if (state.status === 'completed') return '已完成';
    if (state.status === 'failed') return state.error ? `失败：${state.error}` : '运行失败';
    if (state.status === 'cancelled') return '已停止';
    return '运行中';
}

function handleMemberRunProgress(data) {
    if (data.room_id !== activeSessionId || !data.event) return;
    const shouldFollowLatest = roomTimelineNearBottom;
    const state = ensureMemberRunState(data.run_id, data.member_id, data.room_id);
    const inbox = roomSnapshot?.inbox.find((item) => item.run_id === data.run_id);
    state.purpose = inbox?.purpose || state.purpose;
    const event = data.event;
    switch (event.type) {
        case 'connecting':
            state.statusText = `连接 ${event.model}`;
            break;
        case 'thinking':
            state.statusText = '思考中';
            break;
        case 'text_delta':
            state.statusText = '输出中';
            state.rawText += event.text || '';
            break;
        case 'thinking_delta':
            state.statusText = '思考中';
            break;
        case 'intermediate_conclusion':
            state.logs.push(`阶段结论：${truncate(event.content || '', 120)}`);
            break;
        case 'tool_start':
            state.statusText = `调用 ${event.tool_name}`;
            state.logs.push(`开始：${event.tool_name}`);
            break;
        case 'tool_done':
            state.statusText = event.is_error ? `${event.tool_name} 失败` : `${event.tool_name} 完成`;
            state.logs.push(`${event.is_error ? '失败' : '完成'}：${event.tool_name} · ${event.duration_ms}ms`);
            break;
        case 'memory_injected':
            state.logs.push(`记忆：${event.count} 条`);
            break;
        case 'llm_retry':
            state.logs.push(`重试：${event.attempt}/${event.max_attempts}`);
            break;
        case 'final_answer':
            state.rawText = event.content || state.rawText;
            state.statusText = '提交结果';
            state.markdownFinalized = true;
            break;
        case 'error':
            state.error = event.message;
            state.statusText = event.message;
            break;
        default:
            break;
    }
    if (!state.element?.isConnected) renderRoomTimeline();
    scheduleRunElementUpdate(state, shouldFollowLatest);
    const member = roomSnapshot?.members.find((candidate) => candidate.member_id === data.member_id);
    updateAgentStatus(data.member_id, state.statusText || '运行中', member?.display_name || data.member_id);
}

function handleMemberRunFinished(data) {
    if (data.room_id !== activeSessionId) return;
    const state = ensureMemberRunState(data.run_id, data.member_id, data.room_id);
    state.status = data.status;
    state.statusText = '';
    state.error = data.error || state.error;
    const inbox = roomSnapshot?.inbox.find((item) => item.run_id === data.run_id);
    if (inbox) {
        state.purpose = inbox.purpose || state.purpose;
        inbox.state = data.status;
        inbox.error = data.error || inbox.error;
    }
    renderRoomTimeline();
}

function mergeRoomEvent(event) {
    if (!roomSnapshot || event.room_id !== activeSessionId) return;
    const appendResult = RoomReply.appendRoomEvent(
        roomSnapshot.events,
        event,
        authoritativeRoomEventSequence,
    );
    roomSnapshot.events = appendResult.events;
    if (!appendResult.appended) return;
    roomSnapshot.room.latest_event_seq = Math.max(
        roomSnapshot.room.latest_event_seq || 0,
        event.sequence || 0,
    );
    if (event.run_id) {
        const inbox = roomSnapshot.inbox.find((item) => item.run_id === event.run_id);
        if (inbox) inbox.state = 'completed';
    }
    renderCollaborationRoom({ scrollToLatest: appendResult.appended });
}

function handleRoomEventsLoadedBefore(data) {
    if (!roomSnapshot || data.room_id !== activeSessionId) return;
    const operationResult = settlePendingRoomOperation(data);
    if (!operationResult.settled || operationResult.operationType !== 'pagination') return;
    preserveTimelineAnchor = true;
    roomSnapshot.events = RoomReply.mergeEventsBySequence(
        roomSnapshot.events,
        data.events || [],
    );
    replyState = RoomReply.reconcileReplyState(replyState, roomSnapshot.events);
    hasLoadedEarlierRoomEvents = true;
    hasEarlierRoomEvents = Boolean(data.has_more);
    renderCollaborationRoom();
    preserveTimelineAnchor = false;
}

function handleRoomMessageAccepted(data) {
    const operationResult = settlePendingRoomOperation(data);
    if (!operationResult.settled || operationResult.operationType !== 'post') return;
    pendingRoomPost = null;
    replyState = null;
    $input.value = '';
    resetComposerHeight();
    selectedMemberIds.clear();
    renderReplyPreview();
    renderMemberList();
    renderRecipientSelector();
    updateRoomOperationControls();
}

function handleRoomWorkingDirectoryAccepted(data) {
    const accepted = RoomReply.settleRoomOperation(pendingRoomOperation, data);
    if (!accepted.settled || accepted.operationType !== 'directory') return;
    applyRoomSnapshot(data.snapshot);
    const operationResult = settlePendingRoomOperation(data);
    if (!operationResult.settled || operationResult.operationType !== 'directory') return;
    pendingRoomDirectoryUpdate = null;
    closeRoomDirectoryModal();
    setInputEnabled(true);
}

function mergeMember(member) {
    if (!roomSnapshot || member.room_id !== activeSessionId) return;
    const result = RoomReply.mergeVersionedEntity(
        roomSnapshot.members,
        member,
        'member_id',
        true,
    );
    if (!result.changed) return;
    roomSnapshot.members = result.items;
    reconcileSelectedMembers();
    renderCollaborationRoom();
}

function mergeInboxItem(roomId, item) {
    if (!roomSnapshot || roomId !== activeSessionId) return;
    const result = RoomReply.mergeVersionedEntity(
        roomSnapshot.inbox,
        item,
        'inbox_item_id',
        false,
    );
    if (!result.changed) return;
    roomSnapshot.inbox = result.items;
    renderCollaborationRoom();
}

function formatRoomTime(value) {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return '';
    return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function showToast(message) {
    const toast = document.createElement('div');
    toast.className = 'toast';
    toast.textContent = message;
    $toastRegion.appendChild(toast);
    setTimeout(() => toast.remove(), 5000);
}

function handleMemberAction(action, member) {
    switch (action) {
        case 'configure':
            openMemberModal(member);
            break;
        case 'sleep':
            send('sleep_member', {
                member_id: member.member_id,
                expected_version: Number(member.version),
            });
            break;
        case 'wake':
            send('wake_member', {
                member_id: member.member_id,
                expected_version: Number(member.version),
            });
            break;
        case 'restore':
            send('restore_member', {
                member_id: member.member_id,
                expected_version: Number(member.version),
            });
            break;
        case 'interrupt':
            if (member.active_run_id) {
                const item = roomSnapshot.inbox.find(
                    (candidate) => candidate.run_id === member.active_run_id,
                );
                if (!item) {
                    showToast('运行状态已变化，请刷新后重试');
                    break;
                }
                send('interrupt_member_run', {
                    member_id: member.member_id,
                    run_id: member.active_run_id,
                    expected_version: Number(item.version),
                });
            }
            break;
        case 'archive':
            if (window.confirm(`归档成员「${member.display_name}」？`)) {
                send('archive_member', {
                    member_id: member.member_id,
                    expected_version: Number(member.version),
                });
            }
            break;
        default:
            break;
    }
}

function openMemberModal(member = null) {
    if (!roomSnapshot) return;
    $memberModalTitle.textContent = member ? '成员设置' : '创建成员';
    $memberId.value = member?.member_id || '';
    $memberVersion.value = member?.version ?? '';
    $memberName.value = member?.display_name || '';
    fillModelSelect($memberModel, member?.model_policy);
    fillSelect($memberDepth, roomSnapshot.reasoning_depths || [], member?.reasoning_depth);
    $memberModal.classList.remove('hidden');
    setTimeout(() => $memberName.focus(), 0);
    refreshIcons();
}

function closeMemberModal() {
    $memberModal.classList.add('hidden');
    $memberForm.reset();
    $memberId.value = '';
    $memberVersion.value = '';
}

function openRoomDirectoryModal() {
    if (!roomSnapshot || pendingRoomOperation) return;
    const retainedFailure = pendingRoomDirectoryUpdate?.failed
        && pendingRoomDirectoryUpdate.roomId === roomSnapshot.room.room_id;
    if (!retainedFailure) {
        pendingRoomDirectoryUpdate = null;
        $roomDirectoryInput.value = roomSnapshot.room.working_directory || '';
    }
    $roomDirectoryModal.classList.remove('hidden');
    setTimeout(() => {
        $roomDirectoryInput.focus();
        $roomDirectoryInput.select();
    }, 0);
    refreshIcons();
}

function closeRoomDirectoryModal() {
    $roomDirectoryModal.classList.add('hidden');
    if (pendingRoomOperation?.type !== 'directory') {
        pendingRoomDirectoryUpdate = null;
    }
    updateRoomOperationControls();
}

function submitRoomDirectoryForm() {
    if (!roomSnapshot) return;
    const workingDirectory = $roomDirectoryInput.value.trim();
    if (!workingDirectory) return;
    const commandId = globalThis.crypto?.randomUUID
        ? globalThis.crypto.randomUUID()
        : `web-${Date.now()}-${Math.random().toString(16).slice(2)}`;
    const operation = {
        type: 'directory',
        roomId: roomSnapshot.room.room_id,
        commandId,
        expectedVersion: Number(roomSnapshot.room.version),
        previousDirectory: roomSnapshot.room.working_directory || '',
        requestedDirectory: workingDirectory,
        failed: false,
    };
    if (!beginPendingRoomOperation(operation)) {
        showToast('请等待当前房间操作完成');
        return;
    }
    pendingRoomDirectoryUpdate = pendingRoomOperation;
    const sent = send('update_room_working_directory', {
        working_directory: workingDirectory,
        expected_room_version: pendingRoomDirectoryUpdate.expectedVersion,
        command_id: commandId,
    });
    if (!sent) {
        failPendingRoomOperation();
        showToast('连接不可用，目录未更新');
    }
}

function fillModelSelect(select, selected) {
    const details = roomSnapshot?.model_policy_details || [];
    const policies = roomSnapshot?.model_policies || [];
    select.innerHTML = '';
    policies.forEach((policyId) => {
        const detail = details.find((policy) => policy.policy_id === policyId);
        const option = document.createElement('option');
        option.value = policyId;
        const optionLabel = detail
            ? modelCatalog.optionText(detail)
            : modelCatalog.optionTextOrFallback(detail, policyId);
        option.textContent = optionLabel;
        option.title = optionLabel;
        option.selected = policyId === selected;
        select.appendChild(option);
    });
}

function fillSelect(select, values, selected) {
    select.innerHTML = '';
    values.forEach((value) => {
        const option = document.createElement('option');
        option.value = value;
        option.textContent = value;
        option.selected = value === selected;
        select.appendChild(option);
    });
}

function submitMemberForm() {
    const displayName = $memberName.value.trim();
    if (!displayName) return;
    const memberId = $memberId.value;
    if (memberId) {
        send('configure_member', {
            member_id: memberId,
            display_name: displayName,
            model_policy: $memberModel.value,
            reasoning_depth: $memberDepth.value,
            expected_version: Number($memberVersion.value),
        });
    } else {
        send('create_member', {
            display_name: displayName,
            model_policy: $memberModel.value || null,
            reasoning_depth: $memberDepth.value || null,
        });
    }
    closeMemberModal();
}

function submitCollaborationMessage() {
    if (!roomSnapshot) return;
    if (pendingRoomOperation) {
        showToast('请等待当前房间操作完成');
        return;
    }
    const content = $input.value.trim();
    if (!content) return;
    syncMentionRecipients();
    const recipients = [...selectedMemberIds]
        .map((memberId) => roomSnapshot.members.find((member) => member.member_id === memberId))
        .filter(Boolean)
        .map((member) => ({
            member_id: member.member_id,
            expected_version: Number(member.version),
        }));
    if (recipients.length === 0) {
        showToast('请选择至少一个成员');
        return;
    }
    const commandId = globalThis.crypto?.randomUUID
        ? globalThis.crypto.randomUUID()
        : `web-${Date.now()}-${Math.random().toString(16).slice(2)}`;
    const payload = RoomReply.buildRoomPostPayload({
        recipients,
        content,
        mode: roomMode,
        expectedRoomVersion: roomSnapshot.room.version,
        commandId,
        replyState,
    });
    const operation = {
        type: 'post',
        commandId,
        roomId: roomSnapshot.room.room_id,
    };
    if (!beginPendingRoomOperation(operation)) return;
    pendingRoomPost = pendingRoomOperation;
    if (!send('post_room_message', payload)) {
        failPendingRoomOperation();
        showToast('连接不可用，消息未发送');
    }
}

// ── Streaming Text (Typewriter) ─────────────────────────────────
function appendStreamingText(text) {
    if (!currentStreamingEl) {
        currentStreamingEl = document.createElement('div');
        currentStreamingEl.className = 'msg assistant';
        currentStreamingEl.dataset.rawText = '';
        $messages.appendChild(currentStreamingEl);
    }

    currentStreamingEl.dataset.rawText += text;
    renderMarkdown(currentStreamingEl, currentStreamingEl.dataset.rawText);
    const streamingEl = currentStreamingEl;
    attachPreviewAction(
        streamingEl,
        '主脑回复',
        '主脑',
        () => streamingEl.dataset.rawText || streamingEl.textContent || '',
    );
    scrollToBottom();
}

function finalizeStreaming() {
    if (currentStreamingEl) {
        // Final render with full markdown
        renderMarkdown(currentStreamingEl, currentStreamingEl.dataset.rawText);
        currentStreamingEl = null;
    }
    // 确保思考内容全部显示完毕
    if (thinkingDisplayPos < thinkingFullContent.length && currentThinkingEl) {
        thinkingDisplayPos = thinkingFullContent.length;
        currentThinkingEl.textContent = thinkingFullContent;
    }
    if (currentThinkingDetails) {
        currentThinkingDetails.classList.remove('streaming');
        const state = currentThinkingDetails.querySelector('.trace-state');
        if (state) state.textContent = '完成';
    }
    currentThinkingEl = null;
    currentThinkingDetails = null;
    currentToolGroup = null;
    resetThinkingState();
}

function renderMarkdown(el, text) {
    if (typeof marked !== 'undefined') {
        el.innerHTML = marked.parse(text);
        // Apply syntax highlighting
        el.querySelectorAll('pre code').forEach((block) => {
            if (typeof hljs !== 'undefined') {
                hljs.highlightElement(block);
            }
        });
    } else {
        el.textContent = text;
    }
}

function refreshIcons() {
    if (typeof lucide !== 'undefined') lucide.createIcons({ attrs: { 'stroke-width': 1.8 } });
}

// ── Thinking (Typewriter) ───────────────────────────────────────
function appendThinking(content) {
    if (!currentThinkingEl) {
        const trace = createTraceDetails(
            'thinking-block reasoning-trace streaming',
            '主脑思考',
            '流式接收',
            showThinking,
        );
        currentThinkingDetails = trace.root;
        currentThinkingEl = trace.body;
        $messages.appendChild(trace.root);
    }

    thinkingFullContent += content;

    if (!thinkingRaf) {
        thinkingRaf = requestAnimationFrame(typeThinkingFrame);
    }
}

function addIntermediateConclusion(brain, content) {
    if (!content) return;
    addBrainConclusion(brain, content, '阶段结论');
    const trace = createTraceDetails(
        'intermediate-block',
        `${displayBrainName(brain)} · 阶段结论`,
        new Date().toLocaleTimeString(),
        true,
    );
    trace.body.textContent = content;
    attachPreviewAction(
        trace.root,
        `${displayBrainName(brain)} · 阶段结论`,
        displayBrainName(brain),
        content,
    );
    $messages.appendChild(trace.root);
    scrollToBottom();
}

function createTraceDetails(className, title, stateText, open) {
    const root = document.createElement('details');
    root.className = `trace-details ${className}`;
    root.open = Boolean(open);

    const summary = document.createElement('summary');
    summary.innerHTML = `
        <span class="trace-chevron" aria-hidden="true"></span>
        <strong>${escapeHtml(title)}</strong>
        <span class="trace-state">${escapeHtml(stateText || '')}</span>
    `;

    const body = document.createElement('pre');
    body.className = 'trace-content';
    root.append(summary, body);
    return { root, summary, body };
}

function typeThinkingFrame() {
    if (!currentThinkingEl || thinkingDisplayPos >= thinkingFullContent.length) {
        thinkingRaf = null;
        return;
    }

    // 自适应速度：积压越多打字越快
    const remaining = thinkingFullContent.length - thinkingDisplayPos;
    const speed = Math.max(CHARS_PER_FRAME, Math.ceil(remaining / 30));
    thinkingDisplayPos = Math.min(thinkingDisplayPos + speed, thinkingFullContent.length);

    currentThinkingEl.textContent = thinkingFullContent.substring(0, thinkingDisplayPos);
    scrollToBottom();

    thinkingRaf = requestAnimationFrame(typeThinkingFrame);
}

function resetThinkingState() {
    thinkingFullContent = '';
    thinkingDisplayPos = 0;
    if (thinkingRaf) {
        cancelAnimationFrame(thinkingRaf);
        thinkingRaf = null;
    }
}

// ── Welcome ────────────────────────────────────────────────────
function showWelcome() {
    const el = document.createElement('div');
    el.className = 'welcome';
    el.innerHTML = `
        <h2>智脑协作</h2>
        <p>等待消息</p>
    `;
    $messages.appendChild(el);
}

// ── Tool Calls ──────────────────────────────────────────────────
function addToolStart(callId, brain, toolName, input) {
    const stableId = callId || `tool-${Date.now()}-${toolItems.size}`;
    const item = document.createElement('details');
    item.className = 'tool-item trace-details running';
    item.dataset.callId = stableId;
    item.dataset.toolName = toolName;

    const header = document.createElement('summary');
    header.className = 'tool-header';
    header.innerHTML = `
        <span class="trace-chevron" aria-hidden="true"></span>
        <span class="tool-status running"></span>
        <span class="tool-name">${escapeHtml(displayBrainName(brain))} / ${escapeHtml(toolName)}</span>
        <span class="tool-duration">运行中...</span>
    `;

    const body = document.createElement('div');
    body.className = 'tool-body';
    const inputSection = createToolSection('输入', formatStructuredContent(input));
    const outputSection = createToolSection('结果', '等待工具返回...');
    outputSection.classList.add('tool-output');
    body.append(inputSection, outputSection);

    item.appendChild(header);
    item.appendChild(body);

    const group = ensureToolGroup();
    group.querySelector('.tool-group-body').appendChild(item);
    toolItems.set(stableId, item);
    updateToolGroupSummary(group);
    scrollToBottom();
}

function ensureToolGroup() {
    if (currentToolGroup) return currentToolGroup;

    const group = document.createElement('details');
    group.className = 'tool-group running';
    const summary = document.createElement('summary');
    summary.innerHTML = `
        <span class="trace-chevron" aria-hidden="true"></span>
        <i data-lucide="wrench" aria-hidden="true"></i>
        <strong>工具调用</strong>
        <span class="tool-group-count">0 次</span>
        <span class="tool-group-state">运行中</span>
    `;
    const body = document.createElement('div');
    body.className = 'tool-group-body';
    group.append(summary, body);
    $messages.appendChild(group);
    currentToolGroup = group;
    refreshIcons();
    return group;
}

function updateToolGroupSummary(group) {
    if (!group) return;
    const items = Array.from(group.querySelectorAll('.tool-item'));
    const running = items.filter((item) => item.classList.contains('running')).length;
    const errors = items.filter((item) => item.classList.contains('error')).length;
    const count = group.querySelector('.tool-group-count');
    const state = group.querySelector('.tool-group-state');
    if (count) count.textContent = `${items.length} 次`;
    if (state) {
        state.textContent = running > 0
            ? `${running} 个运行中`
            : (errors > 0 ? `${errors} 个失败` : '已完成');
    }
    group.classList.toggle('running', running > 0);
    group.classList.toggle('has-error', errors > 0);
    group.classList.toggle('done', items.length > 0 && running === 0 && errors === 0);
}

function createToolSection(label, content) {
    const section = document.createElement('section');
    const heading = document.createElement('strong');
    const pre = document.createElement('pre');
    heading.textContent = label;
    pre.textContent = content || '无内容';
    section.append(heading, pre);
    return section;
}

function formatStructuredContent(content) {
    if (!content) return '';
    try {
        return JSON.stringify(JSON.parse(content), null, 2);
    } catch (_) {
        return String(content);
    }
}

function updateToolDone(callId, toolName, durationMs, output, isError) {
    let item = callId ? toolItems.get(callId) : null;
    if (!item) {
        const candidates = Array.from(document.querySelectorAll('.tool-item')).reverse();
        item = candidates.find((candidate) => (
            candidate.dataset.toolName === toolName && candidate.classList.contains('running')
        ));
    }
    if (!item) return;

    const status = item.querySelector('.tool-status');
    const duration = item.querySelector('.tool-duration');
    const outputPre = item.querySelector('.tool-output pre');
    status.className = `tool-status ${isError ? 'err' : 'ok'}`;
    duration.textContent = `${durationMs}ms`;
    if (outputPre) outputPre.textContent = formatStructuredContent(output) || '无返回内容';
    item.classList.remove('running');
    item.classList.add(isError ? 'error' : 'done');
    if (isError) item.open = true;
    updateToolGroupSummary(item.closest('.tool-group'));
}

// ── UI Helpers ──────────────────────────────────────────────────
function addSystemMessage(text) {
    const el = document.createElement('div');
    el.className = 'msg system';
    el.textContent = text;
    $messages.appendChild(el);
    scrollToBottom();
    return el;
}

function renderAuthoritativeFinalAnswer(content) {
    if (currentStreamingEl) {
        currentStreamingEl.remove();
        currentStreamingEl = null;
    }
    const el = document.createElement('div');
    el.className = 'msg assistant final-answer';
    el.dataset.rawText = content || '';
    if (el.dataset.rawText) addBrainConclusion('main', el.dataset.rawText, '最终回复');
    renderMarkdown(el, el.dataset.rawText);
    attachPreviewAction(el, '主脑最终回复', '主脑', el.dataset.rawText);
    appendModifiedFiles(el, currentTurnFiles);
    $messages.appendChild(el);
    scrollToBottom();
}

// ── Markdown Preview Drawer ───────────────────────────────────
function createPreviewButton(title, source, content, metadata = {}) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'preview-trigger';
    button.title = '在右侧预览 Markdown';
    button.setAttribute('aria-label', `预览 ${title}`);
    button.innerHTML = '<i data-lucide="panel-right-open" aria-hidden="true"></i><span>预览</span>';
    button.addEventListener('click', (event) => {
        event.preventDefault();
        event.stopPropagation();
        const resolved = typeof content === 'function' ? content() : content;
        openMarkdownPreview(title, source, resolved || '暂无内容', metadata);
    });
    return button;
}

function attachPreviewAction(element, title, source, content) {
    element.classList.add('previewable');
    element.querySelector(':scope > .preview-trigger')?.remove();
    element.appendChild(createPreviewButton(title, source, content));
    refreshIcons();
}

async function openMarkdownPreview(title, source, content, metadata = {}) {
    if (openedLocalFile) {
        const flushed = await flushOpenedLocalFile();
        if (!flushed) {
            showToast('请先处理当前文件的保存冲突');
            return false;
        }
        resetLocalFileEditorSession();
    }
    const rawContent = String(content || '');
    currentPreview = buildPreviewMetadata(title, source, rawContent, metadata);
    currentPreviewSelection = null;
    $previewTitle.textContent = title || '内容预览';
    $previewSource.textContent = source || '智脑内容';
    renderMarkdown($previewContent, rawContent);
    $previewContent.classList.remove('hidden');
    $fileEditorToolbar.classList.add('hidden');
    $fileEditorTabs.classList.add('hidden');
    $fileEditorConflict.classList.add('hidden');
    $fileEditorFrame.classList.add('hidden');
    $fileEditorStatusbar.classList.add('hidden');
    $fileEditorRefresh.classList.add('hidden');
    $fileEditorCopyPath.classList.add('hidden');
    $previewSelectionTools.classList.add('hidden');
    $markdownPreview.classList.add('open');
    $markdownPreview.setAttribute('aria-hidden', 'false');
    $previewBackdrop.classList.remove('hidden');
    document.body.classList.add('preview-open');
    refreshIcons();
    return true;
}

async function closeMarkdownPreview() {
    const returnFocus = openedLocalFileTrigger;
    if (openedLocalFile) {
        const flushed = await flushOpenedLocalFile();
        if (!flushed) {
            showToast('请先处理当前文件的保存冲突');
            $fileEditorContent.focus();
            return false;
        }
    }
    resetLocalFileEditorSession();
    $markdownPreview.classList.remove('open');
    $markdownPreview.setAttribute('aria-hidden', 'true');
    $previewBackdrop.classList.add('hidden');
    document.body.classList.remove('preview-open');
    currentPreview = null;
    currentPreviewSelection = null;
    if (returnFocus?.isConnected) returnFocus.focus();
    return true;
}

function buildPreviewMetadata(title, source, content, metadata = {}) {
    const detectedPath = metadata.path || detectFilePath(content) || '';
    const normalizedPath = detectedPath.replace(/^\\\\\?\\/, '');
    const pathParts = normalizedPath.split(/[\\/]/);
    return {
        title: title || '内容预览',
        source: source || '智脑内容',
        content,
        path: normalizedPath,
        fileName: metadata.fileName || (normalizedPath ? pathParts[pathParts.length - 1] : ''),
    };
}

function detectFilePath(content) {
    const text = String(content || '');
    const windowsPath = text.match(/(?:\\\\\?\\)?[A-Za-z]:\\[^\r\n`"<>|]+?\.[A-Za-z0-9]{1,10}/);
    if (windowsPath) return windowsPath[0].trim();
    const markdownPath = text.match(/`([^`\r\n]+\.[A-Za-z0-9]{1,10})`/);
    return markdownPath ? markdownPath[1].trim() : '';
}

function updatePreviewSelection() {
    if (!currentPreview || !$markdownPreview.classList.contains('open')) return;
    const selection = window.getSelection();
    const selectedText = selection?.toString().trim() || '';
    if (!selectedText || !selection.rangeCount || !$previewContent.contains(selection.anchorNode)) {
        currentPreviewSelection = null;
        $previewSelectionTools.classList.add('hidden');
        return;
    }
    const raw = currentPreview.content;
    let index = raw.indexOf(selectedText);
    if (index < 0) index = raw.indexOf(selectedText.replace(/\s+/g, ' '));
    const startLine = index < 0 ? null : raw.slice(0, index).split('\n').length;
    const endLine = startLine == null ? null : startLine + selectedText.split('\n').length - 1;
    currentPreviewSelection = { text: selectedText, startLine, endLine };
    const lineLabel = startLine == null ? '行号无法定位' : `第 ${startLine}${endLine > startLine ? `-${endLine}` : ''} 行`;
    $previewSelectionInfo.textContent = `${lineLabel} · ${selectedText.length} 字符`;
    $previewSelectionTools.classList.remove('hidden');
}

function quotePreviewSelection() {
    if (!currentPreview || !currentPreviewSelection) return;
    const { text, startLine, endLine } = currentPreviewSelection;
    const lineLabel = startLine == null ? '未知' : (endLine > startLine ? `${startLine}-${endLine}` : `${startLine}`);
    const fileName = currentPreview.fileName || currentPreview.title || '当前预览内容';
    const filePath = currentPreview.path || `未提供（来源：${currentPreview.source}）`;
    const quoted = text.split('\n').map((line) => `> ${line}`).join('\n');
    const reference = `[引用文件: ${fileName} | 路径: ${filePath} | 行号: ${lineLabel}]\n${quoted}\n\n`;
    const start = $input.selectionStart ?? $input.value.length;
    const end = $input.selectionEnd ?? start;
    $input.setRangeText(reference, start, end, 'end');
    $input.dispatchEvent(new Event('input'));
    switchView('chat');
    $input.focus();
}

function addMemoryIndicator(count, preview) {
    const el = document.createElement('div');
    el.className = 'memory-indicator';
    el.textContent = `记忆召回: ${preview}`;
    $messages.appendChild(el);
    scrollToBottom();
    return el;
}

function addSpinner(text) {
    const el = document.createElement('div');
    el.className = 'spinner';
    el.textContent = text;
    $messages.appendChild(el);
    scrollToBottom();
    return el;
}

function removeSpinner() {
    if (currentSpinnerEl && currentSpinnerEl.parentNode) {
        currentSpinnerEl.remove();
    }
    currentSpinnerEl = null;
}

function addUserMessage(text, options = {}) {
    const {
        messageIndex = null,
        messageId = null,
        isLastUser = false,
    } = options;
    const el = document.createElement('div');
    el.className = 'msg user';
    const content = document.createElement('div');
    content.className = 'msg-content';
    content.textContent = text;
    el.appendChild(content);

    if (messageId) {
        el.dataset.messageId = messageId;
        el.classList.add('has-actions');
        const actions = document.createElement('div');
        actions.className = 'msg-actions';
        actions.appendChild(createMessageAction('pencil', '编辑这条消息', () => {
            startInlineMessageEdit(el, text, messageId);
        }));
        if (isLastUser) {
            actions.appendChild(createMessageAction('rotate-ccw', '重试最后一条消息', () => {
                if (isGenerating) {
                    addSystemMessage('生成中暂不能重试');
                    return;
                }
                beginHistoryRegeneration('重试最后一条用户消息');
                send('retry_last_user_message', { message_id: messageId });
            }));
        }
        if (messageIndex !== null && messageIndex !== undefined) {
            actions.appendChild(createMessageAction('trash-2', '删除这一轮历史', () => {
                if (isGenerating) {
                    addSystemMessage('生成中暂不能删除历史');
                    return;
                }
                send('delete_turn', { message_index: messageIndex });
            }, 'danger'));
        }
        el.appendChild(actions);
    } else {
        attachDeleteAction(el, messageIndex);
    }
    $messages.appendChild(el);
    scrollToBottom();
    refreshIcons();
    return el;
}

function createMessageAction(icon, title, onClick, tone = '') {
    const button = document.createElement('button');
    button.className = `msg-action${tone ? ` ${tone}` : ''}`;
    button.type = 'button';
    button.title = title;
    button.setAttribute('aria-label', title);
    button.innerHTML = `<i data-lucide="${icon}" aria-hidden="true"></i>`;
    button.addEventListener('click', (event) => {
        event.stopPropagation();
        onClick();
    });
    return button;
}

function startInlineMessageEdit(messageEl, originalText, messageId) {
    if (isGenerating) {
        addSystemMessage('生成中暂不能编辑历史');
        return;
    }
    document.querySelectorAll('.msg.user.editing').forEach((element) => {
        element.querySelector('.msg-edit-cancel')?.click();
    });

    const content = messageEl.querySelector('.msg-content');
    const actions = messageEl.querySelector('.msg-actions');
    const editor = document.createElement('div');
    editor.className = 'msg-edit-form';
    const textarea = document.createElement('textarea');
    textarea.className = 'msg-edit-textarea';
    textarea.value = originalText;
    textarea.rows = Math.min(8, Math.max(2, originalText.split('\n').length));
    textarea.setAttribute('aria-label', '编辑用户消息');

    const controls = document.createElement('div');
    controls.className = 'msg-edit-controls';
    const cancel = createMessageAction('x', '取消编辑', () => {
        editor.remove();
        content.classList.remove('hidden');
        actions.classList.remove('hidden');
        messageEl.classList.remove('editing');
    });
    cancel.classList.add('msg-edit-cancel');
    const save = createMessageAction('check', '保存并重新生成', () => {
        const nextContent = textarea.value.trim();
        if (!nextContent) {
            textarea.focus();
            return;
        }
        if (nextContent === originalText.trim()) {
            cancel.click();
            return;
        }
        beginHistoryRegeneration('编辑用户消息并重新生成');
        send('edit_user_message', { message_id: messageId, content: nextContent });
    });
    save.classList.add('primary');
    controls.append(cancel, save);
    editor.append(textarea, controls);

    content.classList.add('hidden');
    actions.classList.add('hidden');
    messageEl.classList.add('editing');
    messageEl.appendChild(editor);
    textarea.addEventListener('keydown', (event) => {
        if (event.key === 'Escape') {
            event.preventDefault();
            cancel.click();
        } else if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
            event.preventDefault();
            save.click();
        }
    });
    requestAnimationFrame(() => {
        textarea.focus();
        textarea.setSelectionRange(textarea.value.length, textarea.value.length);
    });
    refreshIcons();
}

function attachDeleteAction(el, messageIndex) {
    if (messageIndex === null || messageIndex === undefined) return;

    const btn = document.createElement('button');
    btn.className = 'msg-delete';
    btn.type = 'button';
    btn.title = '删除这一轮历史';
    btn.textContent = '×';
    btn.addEventListener('click', (e) => {
        e.stopPropagation();
        if (isGenerating) {
            addSystemMessage('生成中暂不能删除历史');
            return;
        }
        send('delete_turn', { message_index: messageIndex });
    });
    el.appendChild(btn);
}

function setInputEnabled(enabled) {
    const effectiveEnabled = roomSnapshot ? true : enabled;
    $input.disabled = !effectiveEnabled;
    const modalOpen = Boolean(document.querySelector(
        '.modal[aria-modal="true"]:not(.hidden)',
    ));
    if (RoomReply.shouldFocusComposer(effectiveEnabled, modalOpen)) {
        $input.focus();
    }
}

function updateSendButton() {
    const roomOperationPending = Boolean(roomSnapshot && pendingRoomOperation);
    $sendBtn.disabled = roomOperationPending;
    if (isGenerating && !roomSnapshot) {
        $sendBtn.classList.add('stop-mode');
        $sendBtn.innerHTML = '<i data-lucide="square"></i>';
        $sendBtn.title = '停止生成 (Esc / Ctrl+C)';
        $sendBtn.setAttribute('aria-label', '停止生成');
    } else {
        $sendBtn.classList.remove('stop-mode');
        $sendBtn.innerHTML = '<i data-lucide="send-horizontal"></i>';
        const roomOperationLabel = pendingRoomOperation?.type === 'post'
            ? '消息发送中'
            : '房间操作处理中';
        $sendBtn.title = roomOperationPending ? roomOperationLabel : '发送';
        $sendBtn.setAttribute('aria-label', roomOperationPending ? roomOperationLabel : '发送');
    }
    refreshIcons();
}

function scrollToBottom() {
    requestAnimationFrame(() => {
        $messages.scrollTop = $messages.scrollHeight;
    });
}

function escapeHtml(str) {
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
}

function truncate(str, maxLen) {
    return str.length > maxLen ? str.substring(0, maxLen) + '...' : str;
}

// ── Cockpit ────────────────────────────────────────────────────
function normalizeBrainKey(brain) {
    const raw = String(brain || '').trim();
    const value = raw.toLowerCase();
    if (/^(agent|novel):/.test(value)) return value;
    if (value.includes('client') || value.includes('user') || value.includes('用户')) return 'client';
    if (value.includes('memory') || value.includes('记忆')) return 'memory';
    if (value.includes('graph') || value.includes('图谱')) return 'graph';
    if (value.includes('eval') || value.includes('评估')) return 'eval';
    if (value.includes('novel') || value.includes('小说')) return 'novel';
    if (value.includes('tool') || value.includes('executor') || value.includes('工具')) return 'tool';
    if (value.includes('explore')) return 'explore';
    if (value.includes('agent')) return 'agent';
    return 'main';
}

function displayBrainName(brain) {
    const exact = String(brain || '').toLowerCase();
    if (brainState.nodes[exact]) return brainState.nodes[exact].label;
    const key = normalizeBrainKey(brain);
    ensureBrainNode(key);
    return brainState.nodes[key].label;
}

function ensureBrainNode(key, overrides = {}) {
    if (!brainState.nodes[key]) {
        const profile = brainProfiles[key] || { label: key, role: '动态脑区', icon: key.slice(0, 1).toUpperCase() };
        brainState.nodes[key] = {
            key,
            label: profile.label,
            role: profile.role,
            icon: profile.icon,
            status: 'idle',
            detail: '待命',
            active: false,
            lastSeen: null,
            model: '',
            pending: false,
        };
    }
    if (overrides.label) brainState.nodes[key].label = overrides.label;
    if (overrides.role) brainState.nodes[key].role = overrides.role;
    if (overrides.icon) brainState.nodes[key].icon = overrides.icon;
}

function markBrain(brain, status, detail) {
    const key = normalizeBrainKey(brain);
    ensureBrainNode(key);
    brainState.nodes[key].status = status;
    brainState.nodes[key].detail = detail || '';
    brainState.nodes[key].active = status === 'active';
    brainState.nodes[key].lastSeen = Date.now();
    renderCockpit();
}

function setBrainLink(from, to, label, active = true) {
    const source = normalizeBrainKey(from);
    const target = normalizeBrainKey(to);
    ensureBrainNode(source);
    ensureBrainNode(target);
    const existing = brainState.links.find((l) => l.from === source && l.to === target);
    if (existing) {
        existing.label = label;
        existing.active = active;
        existing.lastSeen = Date.now();
    } else {
        brainState.links.push({ from: source, to: target, label, active, lastSeen: Date.now() });
    }
    renderCockpit();
}

function addCommunication(from, to, label, kind = 'info') {
    const source = normalizeBrainKey(from);
    const target = normalizeBrainKey(to);
    ensureBrainNode(source);
    ensureBrainNode(target);
    setBrainLink(source, target, label);
    brainState.communications.unshift({
        id: `event-${Date.now()}-${brainState.communications.length}`,
        synthetic: true,
        stamp: new Date().toLocaleTimeString(),
        from: source,
        to: target,
        label,
        kind,
    });
    brainState.communications = brainState.communications.slice(0, 60);
}

function handleBrainCommunication(exchange) {
    if (!exchange || !exchange.exchange_id) return;
    const sender = normalizeBrainKey(exchange.sender);
    const receiver = normalizeBrainKey(exchange.receiver);
    ensureBrainNode(sender, {
        label: exchange.sender_label,
        role: participantRole(exchange.kind, exchange.sender_label),
        icon: participantIcon(sender),
    });
    ensureBrainNode(receiver, {
        label: exchange.receiver_label,
        role: participantRole(exchange.kind, exchange.receiver_label),
        icon: participantIcon(receiver),
    });

    let record = brainState.communications.find((item) => (
        !item.synthetic && item.exchangeId === exchange.exchange_id
    ));
    if (!record) {
        record = {
            id: `exchange-${exchange.exchange_id}`,
            exchangeId: exchange.exchange_id,
            synthetic: false,
            kind: exchange.kind,
            request: null,
            response: null,
            updatedAt: Date.now(),
        };
        brainState.communications.unshift(record);
    }
    record[exchange.phase] = { ...exchange, sender, receiver };
    if (exchange.phase === 'response' && exchange.content) {
        addBrainConclusion(sender, exchange.content, exchange.title || '最终结果', `exchange-${exchange.exchange_id}`);
    }
    record.updatedAt = Date.now();
    brainState.communications.sort((a, b) => (b.updatedAt || 0) - (a.updatedAt || 0));
    brainState.communications = brainState.communications.slice(0, 40);
    renderChatExchange(record);

    const failed = exchange.status === 'failed';
    setBrainLink(sender, receiver, exchange.title, exchange.phase === 'request');
    if (exchange.phase === 'response') {
        brainState.nodes[sender].pending = false;
        markBrain(sender, failed ? 'error' : 'done', exchange.title);
        markBrain(receiver, isGenerating ? 'active' : 'done', '收到完整结果，继续汇总');
    } else {
        brainState.nodes[receiver].pending = true;
        markBrain(sender, 'active', exchange.title);
        markBrain(receiver, 'active', '收到完整任务');
    }
    updateAgentStatus(
        exchange.phase === 'request' ? receiver : sender,
        exchange.phase === 'request' ? '执行中' : (failed ? '失败' : '已返回'),
        exchange.title,
    );
    if (
        exchange.phase === 'response'
        && !isGenerating
        && !Object.values(brainState.nodes).some((node) => node.pending)
    ) {
        brainState.activeSince = null;
    }
    addBrainEvent(
        `${exchange.sender_label} -> ${exchange.receiver_label}: ${exchange.title}`,
    );
}

function renderChatExchange(item, shouldScroll = true) {
    const request = item.request;
    const response = item.response;
    const primary = request || response;
    if (!primary) return;

    const status = response?.status || request?.status || 'running';
    const exchangeId = item.exchangeId;
    let details = chatExchangeItems.get(exchangeId);
    const isNew = !details;
    const wasOpen = details?.open ?? true;

    if (!details) {
        details = document.createElement('details');
        details.dataset.exchangeId = exchangeId;
        chatExchangeItems.set(exchangeId, details);
        $messages.appendChild(details);
    }

    details.className = `trace-details communication-trace ${status}`;
    details.replaceChildren();
    details.open = wasOpen;

    const routeFrom = request?.sender_label || response?.receiver_label || primary.sender_label;
    const routeTo = request?.receiver_label || response?.sender_label || primary.receiver_label;
    const summary = document.createElement('summary');
    summary.className = 'communication-trace-summary';
    summary.innerHTML = `
        <span class="trace-chevron" aria-hidden="true"></span>
        <span class="communication-trace-route">
            <strong>${escapeHtml(routeFrom || '未知脑区')}</strong>
            <span aria-hidden="true">→</span>
            <strong>${escapeHtml(routeTo || '未知脑区')}</strong>
        </span>
        <span class="comm-status">${escapeHtml(exchangeStatusLabel(status))}</span>
        <span class="communication-trace-title">${escapeHtml(request?.title || response?.title || '脑区交流')}</span>
    `;

    const body = document.createElement('div');
    body.className = 'communication-body';
    if (request) appendExchangePayload(body, '任务原文', request);
    if (response) {
        appendExchangePayload(body, response.status === 'failed' ? '失败信息' : '最终结果', response);
    } else {
        const pending = document.createElement('p');
        pending.className = 'communication-pending';
        pending.textContent = '等待接收方返回完整结果...';
        body.appendChild(pending);
    }

    details.append(summary, body);
    if (isNew && shouldScroll) scrollToBottom();
}

function participantRole(kind, label) {
    if (kind === 'delegation') return label.includes('小说脑') ? '创作任务执行' : '独立子任务执行';
    if (kind === 'memory') return '历史召回 / 上下文注入';
    if (kind === 'evaluation') return '质量检查 / 结果评估';
    return '运行时参与者';
}

function participantIcon(key) {
    if (key.startsWith('novel:')) return 'N';
    if (key.startsWith('agent:')) return 'A';
    return brainProfiles[key]?.icon || key.slice(0, 1).toUpperCase();
}

function addBrainEvent(text) {
    const stamp = new Date().toLocaleTimeString();
    brainState.events.unshift({ stamp, text });
    brainState.events = brainState.events.slice(0, 50);
    renderCockpit();
}

function finishActiveBrains() {
    Object.values(brainState.nodes).forEach((node) => {
        if (node.active) {
            if (node.pending) return;
            node.status = 'done';
            node.active = false;
            node.detail = node.detail || '完成';
        }
    });
    brainState.links.forEach((link) => {
        link.active = Boolean(
            brainState.nodes[link.from]?.pending || brainState.nodes[link.to]?.pending,
        );
    });
    renderCockpit();
}

function trackToolStart(brain, toolName, input) {
    const owner = normalizeBrainKey(brain);
    brainState.metrics.toolCalls += 1;
    markBrain(owner, 'active', `调用 ${toolName}`);
    markBrain('tool', 'active', toolName);
    setBrainLink(owner, 'tool', `调用 ${toolName}`);
    addCommunication(owner, 'tool', `调用工具 ${toolName}`, 'tool');
    updateAgentStatus('tool', '运行中', toolName);
    addBrainEvent(`${displayBrainName(brain)} 调用工具 ${toolName}`);

    const graphMode = graphToolMode(toolName);
    if (graphMode) {
        const modeLabel = graphMode === 'write' ? '写入' : '读取';
        markBrain('graph', 'active', `${modeLabel} ${toolName}`);
        setBrainLink('tool', 'graph', `图谱${modeLabel}`);
        addCommunication('tool', 'graph', `${modeLabel}知识图谱: ${toolName}`, graphMode === 'write' ? 'graph-write' : 'graph-read');
        addMemoryGraphOp('graph', graphMode, `${modeLabel} ${toolName}`, input || '');
        updateAgentStatus('graph', `${modeLabel}中`, toolName);
    }

    if (toolName === 'Agent') {
        const subagent = extractSubagentType(input);
        const target = normalizeBrainKey(subagent || 'agent');
        const label = target === 'novel' ? '发布小说任务' : '发布任务';
        updateAgentStatus(target, '启动中', label);
        addBrainEvent(`${displayBrainName(brain)}: ${label}`);
    }
    renderCockpit();
}

function trackToolDone(brain, toolName, durationMs, isError) {
    const owner = normalizeBrainKey(brain);
    markBrain(owner, isError ? 'error' : 'done', `${toolName} ${durationMs}ms`);
    markBrain('tool', isError ? 'error' : 'done', `${toolName} ${durationMs}ms`);
    if (isError) brainState.metrics.errors += 1;
    addCommunication('tool', owner, `${toolName} ${isError ? '失败' : '完成'} ${durationMs}ms`, isError ? 'error' : 'ok');
    updateAgentStatus('tool', isError ? '异常' : '完成', `${toolName} ${durationMs}ms`);
    addBrainEvent(`${displayBrainName(brain)} 完成工具 ${toolName}`);

    const graphMode = graphToolMode(toolName);
    if (graphMode) {
        if (graphMode === 'write') {
            brainState.metrics.graphWrites += 1;
        } else {
            brainState.metrics.graphReads += 1;
        }
        markBrain('graph', isError ? 'error' : 'done', `${toolName} ${durationMs}ms`);
        addCommunication('graph', owner, `${toolName} ${isError ? '失败' : '完成'} ${durationMs}ms`, isError ? 'error' : 'ok');
        addMemoryGraphOp('graph', graphMode, `${toolName} ${isError ? '失败' : '完成'}`, `${durationMs}ms`);
        updateAgentStatus('graph', isError ? '异常' : '完成', `${toolName} ${durationMs}ms`);
    }
    renderCockpit();
}

function graphToolMode(toolName) {
    const name = String(toolName || '').toLowerCase();
    if (!name.startsWith('graph_')) return null;
    if (
        name.includes('add_') ||
        name.includes('link_') ||
        name.includes('index_') ||
        name.includes('write') ||
        name.includes('upsert')
    ) {
        return 'write';
    }
    return 'read';
}

function addMemoryGraphOp(source, mode, title, detail) {
    brainState.memoryGraphOps.unshift({
        stamp: new Date().toLocaleTimeString(),
        source,
        mode,
        title,
        detail: truncate(String(detail || ''), 140),
    });
    brainState.memoryGraphOps = brainState.memoryGraphOps.slice(0, 40);
    renderCockpit();
}

function extractSubagentType(input) {
    if (!input) return null;
    try {
        const parsed = JSON.parse(input);
        return parsed.subagent_type || parsed.subagentType || parsed.agent || null;
    } catch (_) {
        const match = String(input).match(/subagent[_-]?type["'\s:=]+([A-Za-z0-9_\-\u4e00-\u9fa5]+)/i);
        return match ? match[1] : null;
    }
}

function renderCockpit() {
    if (!$brainNodes) return;
    initializeCoreBrainNodes();

    const activeNames = Object.values(brainState.nodes)
        .filter((node) => node.active)
        .map((node) => node.label);
    $cockpitSummary.textContent = activeNames.length > 0
        ? `${activeNames.join('、')} 正在工作`
        : cockpitIdleSummary();

    renderConnectionState();
    renderCockpitMetrics(activeNames.length);
    renderRuntime();

    $brainNodes.innerHTML = '';
    Object.entries(brainState.nodes).forEach(([key, node]) => {
        const card = document.createElement('div');
        card.className = `brain-node ${node.status}`;
        card.dataset.brain = key;
        card.innerHTML = `
            <div class="brain-node-head">
                <span class="brain-node-icon">${escapeHtml(node.icon || key.slice(0, 1).toUpperCase())}</span>
                <div>
                    <div class="brain-node-title">${escapeHtml(node.label)}</div>
                    <div class="brain-node-role">${escapeHtml(node.role || '')}</div>
                </div>
            </div>
            <div class="brain-node-meta">
                <span class="brain-node-status">${escapeHtml(statusText(node.status))}</span>
                <span>${escapeHtml(formatLastSeen(node.lastSeen))}</span>
            </div>
            <div class="brain-node-detail">${escapeHtml(truncate(node.detail || '待命', 96))}</div>
        `;
        $brainNodes.appendChild(card);
    });

    renderCommunicationList();
    renderConclusionPanel();
    renderAgentStatusList();
    renderMemoryGraphList();

    $brainEventList.innerHTML = '';
    const events = brainState.events.length ? brainState.events : [{ stamp: '--:--:--', text: '暂无运行事件' }];
    events.forEach((event) => {
        const row = document.createElement('div');
        row.className = 'brain-event';
        row.innerHTML = `<span>${escapeHtml(event.stamp)}</span><p>${escapeHtml(event.text)}</p>`;
        $brainEventList.appendChild(row);
    });

    requestAnimationFrame(renderBrainLinks);
}

function renderBrainLinks() {
    if (!$brainLinks || !$brainNodes) return;

    const mapRect = document.getElementById('brain-map').getBoundingClientRect();
    if (mapRect.width <= 0 || mapRect.height <= 0) return;
    $brainLinks.setAttribute('viewBox', `0 0 ${mapRect.width} ${mapRect.height}`);
    $brainLinks.innerHTML = '';
    appendArrowDefs($brainLinks);

    const latestLinks = [];
    const seenPairs = new Set();
    [...brainState.links]
        .sort((a, b) => (b.lastSeen || 0) - (a.lastSeen || 0))
        .forEach((link) => {
            const pairKey = [link.from, link.to].sort().join('|');
            if (seenPairs.has(pairKey)) return;
            seenPairs.add(pairKey);
            latestLinks.push(link);
        });

    latestLinks.forEach((link) => {
        const nodes = Array.from($brainNodes.querySelectorAll('.brain-node'));
        const fromEl = nodes.find((node) => node.dataset.brain === link.from);
        const toEl = nodes.find((node) => node.dataset.brain === link.to);
        if (!fromEl || !toEl) return;

        const fromRect = fromEl.getBoundingClientRect();
        const toRect = toEl.getBoundingClientRect();
        if (fromRect.width <= 0 || fromRect.height <= 0 || toRect.width <= 0 || toRect.height <= 0) return;
        const fromCenter = rectCenter(fromRect, mapRect);
        const toCenter = rectCenter(toRect, mapRect);
        const geometry = linkGeometry(fromRect, toRect, mapRect, fromCenter, toCenter);

        const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
        path.setAttribute('d', geometry.path);
        path.setAttribute('class', link.active ? 'brain-link active' : 'brain-link');
        path.setAttribute('marker-end', link.active ? 'url(#arrow-active)' : 'url(#arrow-idle)');
        $brainLinks.appendChild(path);

        const label = document.createElementNS('http://www.w3.org/2000/svg', 'text');
        label.setAttribute('x', geometry.labelX);
        label.setAttribute('y', geometry.labelY);
        label.setAttribute('text-anchor', geometry.labelAnchor);
        label.setAttribute('class', 'brain-link-label');
        label.textContent = shortLinkLabel(link.label);
        $brainLinks.appendChild(label);
    });
}

function shortLinkLabel(label) {
    const value = String(label || '');
    if (value.includes('记忆') || value.includes('注入')) return '记忆';
    if (value.includes('评估')) return '评估';
    if (value.includes('写入')) return '写入';
    if (value.includes('读取')) return '读取';
    if (value.includes('结果') || value.includes('完成') || value.includes('返回')) return '结果';
    if (value.includes('任务') || value.includes('发布')) return '任务';
    if (value.includes('调用')) return '调用';
    return truncate(value, 4);
}

function linkGeometry(fromRect, toRect, mapRect, fromCenter, toCenter) {
    const sameColumn = Math.abs(fromCenter.x - toCenter.x) < 36;
    const crossesAnotherRow = Math.abs(fromCenter.y - toCenter.y)
        > Math.max(fromRect.height, toRect.height) + 28;
    if (sameColumn && crossesAnotherRow) {
        const useRight = fromCenter.x >= mapRect.width / 2;
        const sideX = useRight ? mapRect.width - 9 : 9;
        const startX = useRight
            ? fromRect.right - mapRect.left + 3
            : fromRect.left - mapRect.left - 3;
        const endX = useRight
            ? toRect.right - mapRect.left + 10
            : toRect.left - mapRect.left - 10;
        return {
            path: `M ${startX} ${fromCenter.y} C ${sideX} ${fromCenter.y}, ${sideX} ${toCenter.y}, ${endX} ${toCenter.y}`,
            labelX: useRight ? sideX - 5 : sideX + 5,
            labelY: (fromCenter.y + toCenter.y) / 2 - 5,
            labelAnchor: useRight ? 'end' : 'start',
        };
    }

    const start = rectEdgePoint(fromRect, mapRect, toCenter, -3);
    const end = rectEdgePoint(toRect, mapRect, fromCenter, 10);
    return {
        path: `M ${start.x} ${start.y} L ${end.x} ${end.y}`,
        labelX: (start.x + end.x) / 2,
        labelY: (start.y + end.y) / 2 - 7,
        labelAnchor: 'middle',
    };
}

function appendArrowDefs(svg) {
    const ns = 'http://www.w3.org/2000/svg';
    const defs = document.createElementNS(ns, 'defs');
    [['arrow-idle', 'brain-arrow idle'], ['arrow-active', 'brain-arrow active']].forEach(([id, className]) => {
        const marker = document.createElementNS(ns, 'marker');
        marker.setAttribute('id', id);
        marker.setAttribute('viewBox', '0 0 10 10');
        marker.setAttribute('refX', '8');
        marker.setAttribute('refY', '5');
        marker.setAttribute('markerWidth', '7');
        marker.setAttribute('markerHeight', '7');
        marker.setAttribute('markerUnits', 'userSpaceOnUse');
        marker.setAttribute('orient', 'auto-start-reverse');
        const path = document.createElementNS(ns, 'path');
        path.setAttribute('d', 'M 0 0 L 10 5 L 0 10 z');
        path.setAttribute('class', className);
        marker.appendChild(path);
        defs.appendChild(marker);
    });
    svg.appendChild(defs);
}

function rectCenter(rect, containerRect) {
    return {
        x: rect.left + rect.width / 2 - containerRect.left,
        y: rect.top + rect.height / 2 - containerRect.top,
    };
}

function rectEdgePoint(rect, containerRect, toward, gap) {
    const center = rectCenter(rect, containerRect);
    const dx = toward.x - center.x;
    const dy = toward.y - center.y;
    const halfWidth = rect.width / 2;
    const halfHeight = rect.height / 2;
    const tx = Math.abs(dx) > 0.001 ? halfWidth / Math.abs(dx) : Infinity;
    const ty = Math.abs(dy) > 0.001 ? halfHeight / Math.abs(dy) : Infinity;
    const scale = Math.min(tx, ty);
    const distance = Math.hypot(dx, dy) || 1;
    return {
        x: center.x + dx * scale - (dx / distance) * gap,
        y: center.y + dy * scale - (dy / distance) * gap,
    };
}

function statusText(status) {
    switch (status) {
        case 'active': return '工作中';
        case 'done': return '完成';
        case 'error': return '异常';
        case 'connecting': return '连接中';
        default: return '待命';
    }
}

function initializeCoreBrainNodes() {
    ['client', 'instance', 'memory', 'graph', 'tool'].forEach(ensureBrainNode);
}

function cockpitIdleSummary() {
    if (brainState.currentModel) {
        return `已连接 ${brainState.currentModel}，等待任务进入`;
    }
    return '所有成员与服务待命，等待任务进入';
}

function setConnectionState(state) {
    brainState.connection = state;
    renderConnectionState();
}

function renderConnectionState() {
    if (!$cockpitConnection) return;
    const label = {
        online: '已连接',
        connecting: '连接中',
        error: '异常',
        offline: '离线',
    }[brainState.connection] || '未知';
    $cockpitConnection.className = `connection-pill ${brainState.connection}`;
    const text = $cockpitConnection.querySelector('strong');
    if (text) text.textContent = label;
}

function renderCockpitMetrics(activeCount) {
    if ($metricActiveBrains) $metricActiveBrains.textContent = String(activeCount);
    if ($metricTools) $metricTools.textContent = String(brainState.metrics.toolCalls);
    if ($metricMemory) $metricMemory.textContent = String(brainState.metrics.memoryRefs);
    if ($metricGraphReads) $metricGraphReads.textContent = String(brainState.metrics.graphReads);
    if ($metricGraphWrites) $metricGraphWrites.textContent = String(brainState.metrics.graphWrites);
    if ($metricErrors) $metricErrors.textContent = String(brainState.metrics.errors + brainState.metrics.retries);
}

function renderRuntime() {
    if (!$cockpitRuntime) return;
    if (!brainState.activeSince) {
        $cockpitRuntime.textContent = '空闲';
        return;
    }
    const seconds = Math.max(0, Math.floor((Date.now() - brainState.activeSince) / 1000));
    $cockpitRuntime.textContent = `运行 ${seconds}s`;
}

function renderCommunicationList() {
    if (!$communicationList) return;
    $communicationList.querySelectorAll('details[open][data-record-id]').forEach((details) => {
        expandedCommunicationIds.add(details.dataset.recordId);
    });
    $communicationList.innerHTML = '';
    if (!brainState.communications.length) {
        const empty = document.createElement('div');
        empty.className = 'communication-empty';
        empty.textContent = '暂无通信记录';
        $communicationList.appendChild(empty);
        return;
    }

    brainState.communications.forEach((item) => {
        if (item.synthetic) {
            $communicationList.appendChild(renderSyntheticCommunication(item));
            return;
        }
        $communicationList.appendChild(renderExchangeCommunication(item));
    });
}

function addBrainConclusion(brain, content, title = '结论', id = null, shouldRender = true) {
    const brainKey = normalizeBrainKey(brain || 'main');
    const conclusionId = id || `conclusion-${Date.now()}-${brainState.conclusions.length}`;
    const existing = brainState.conclusions.find((item) => item.id === conclusionId);
    const conclusion = {
        id: conclusionId,
        brain: brainKey,
        title,
        content: String(content || ''),
        stamp: new Date().toLocaleTimeString(),
        updatedAt: Date.now(),
    };
    if (existing) Object.assign(existing, conclusion);
    else brainState.conclusions.unshift(conclusion);
    brainState.conclusions.sort((a, b) => b.updatedAt - a.updatedAt);
    brainState.conclusions = brainState.conclusions.slice(0, 100);
    if (shouldRender) renderConclusionPanel();
}

function renderConclusionPanel() {
    if (!$conclusionTabs || !$conclusionList) return;
    const brains = [...new Set(brainState.conclusions.map((item) => item.brain))];
    if (brainState.conclusionBrain !== 'all' && !brains.includes(brainState.conclusionBrain)) {
        brainState.conclusionBrain = 'all';
    }
    $conclusionTabs.innerHTML = '';
    [['all', '全部'], ...brains.map((brain) => [brain, displayBrainName(brain)])].forEach(([key, label]) => {
        const count = key === 'all'
            ? brainState.conclusions.length
            : brainState.conclusions.filter((item) => item.brain === key).length;
        const button = document.createElement('button');
        button.type = 'button';
        button.className = `conclusion-tab${brainState.conclusionBrain === key ? ' active' : ''}`;
        button.setAttribute('role', 'tab');
        button.setAttribute('aria-selected', String(brainState.conclusionBrain === key));
        button.innerHTML = `<span>${escapeHtml(label)}</span><em>${count}</em>`;
        button.addEventListener('click', () => {
            brainState.conclusionBrain = key;
            renderConclusionPanel();
        });
        $conclusionTabs.appendChild(button);
    });

    const visible = brainState.conclusionBrain === 'all'
        ? brainState.conclusions
        : brainState.conclusions.filter((item) => item.brain === brainState.conclusionBrain);
    if ($conclusionCount) $conclusionCount.textContent = `${visible.length} 条`;
    $conclusionList.innerHTML = '';
    if (!visible.length) {
        const empty = document.createElement('div');
        empty.className = 'communication-empty';
        empty.textContent = '暂无脑区结论';
        $conclusionList.appendChild(empty);
        return;
    }
    visible.forEach((item) => {
        const article = document.createElement('article');
        article.className = 'conclusion-card';
        const heading = document.createElement('header');
        heading.innerHTML = `
            <strong>${escapeHtml(displayBrainName(item.brain))}</strong>
            <span>${escapeHtml(item.title)}</span>
            <time>${escapeHtml(item.stamp)}</time>
        `;
        heading.appendChild(createPreviewButton(
            `${displayBrainName(item.brain)} · ${item.title}`,
            displayBrainName(item.brain),
            item.content,
        ));
        const body = document.createElement('div');
        body.className = 'conclusion-content markdown-body';
        renderMarkdown(body, item.content);
        article.append(heading, body);
        $conclusionList.appendChild(article);
    });
    refreshIcons();
}

function renderSyntheticCommunication(item) {
    const row = document.createElement('div');
    row.className = `communication-row ${item.kind || 'info'}`;
    row.innerHTML = `
        <span class="comm-time">${escapeHtml(item.stamp)}</span>
        <span class="comm-node">${escapeHtml(displayBrainName(item.from))}</span>
        <span class="comm-arrow">→</span>
        <span class="comm-node">${escapeHtml(displayBrainName(item.to))}</span>
        <p>${escapeHtml(item.label)}</p>
    `;
    return row;
}

function renderExchangeCommunication(item) {
    const request = item.request;
    const response = item.response;
    const primary = request || response;
    const status = response?.status || request?.status || 'running';
    const details = document.createElement('details');
    details.className = `communication-exchange ${status}`;
    details.dataset.recordId = item.id;
    details.open = expandedCommunicationIds.has(item.id)
        || (!response && !collapsedCommunicationIds.has(item.id));

    const summary = document.createElement('summary');
    const routeFrom = request?.sender_label || response?.receiver_label || displayBrainName(primary.sender);
    const routeTo = request?.receiver_label || response?.sender_label || displayBrainName(primary.receiver);
    summary.innerHTML = `
        <span class="trace-chevron" aria-hidden="true"></span>
        <span class="comm-route">
            <strong>${escapeHtml(routeFrom)}</strong>
            <span aria-hidden="true">→</span>
            <strong>${escapeHtml(routeTo)}</strong>
        </span>
        <span class="comm-status">${escapeHtml(exchangeStatusLabel(status))}</span>
        <span class="comm-time">${escapeHtml(exchangeTime(primary.occurred_at))}</span>
        <p>${escapeHtml(request?.title || response?.title || '通信记录')}</p>
    `;
    details.appendChild(summary);

    const body = document.createElement('div');
    body.className = 'communication-body';
    if (request) appendExchangePayload(body, '任务原文', request);
    if (response) {
        appendExchangePayload(body, response.status === 'failed' ? '失败信息' : '最终结果', response);
    } else {
        const pending = document.createElement('p');
        pending.className = 'communication-pending';
        pending.textContent = '等待接收方返回完整结果...';
        body.appendChild(pending);
    }
    details.appendChild(body);
    details.addEventListener('toggle', () => {
        if (details.open) {
            expandedCommunicationIds.add(item.id);
            collapsedCommunicationIds.delete(item.id);
        } else {
            expandedCommunicationIds.delete(item.id);
            collapsedCommunicationIds.add(item.id);
        }
    });
    return details;
}

function appendExchangePayload(container, label, exchange) {
    const section = document.createElement('section');
    const heading = document.createElement('div');
    const content = document.createElement('pre');
    heading.innerHTML = `
        <strong>${escapeHtml(label)}</strong>
        <span>${escapeHtml(exchange.sender_label)} → ${escapeHtml(exchange.receiver_label)}</span>
        ${exchange.duration_ms == null ? '' : `<em>${escapeHtml(String(exchange.duration_ms))}ms</em>`}
    `;
    content.textContent = exchange.content || '无内容';
    const preview = createPreviewButton(
        `${exchange.sender_label} · ${label}`,
        `${exchange.sender_label} → ${exchange.receiver_label}`,
        exchange.content || '',
    );
    heading.appendChild(preview);
    section.append(heading, content);
    container.appendChild(section);
}

function exchangeStatusLabel(status) {
    return {
        running: '进行中',
        completed: '已返回',
        failed: '失败',
        empty: '无结果',
    }[status] || status;
}

function exchangeTime(value) {
    if (!value) return '--:--:--';
    const time = new Date(value);
    return Number.isNaN(time.getTime()) ? '--:--:--' : time.toLocaleTimeString();
}

function renderMemoryGraphList() {
    if (!$memoryGraphList) return;
    $memoryGraphList.innerHTML = '';
    const rows = brainState.memoryGraphOps.length
        ? brainState.memoryGraphOps
        : [{ stamp: '--:--:--', source: 'memory', mode: 'idle', title: '暂无记忆或图谱 I/O', detail: '等待记忆召回、知识图谱读取或写入事件' }];

    rows.forEach((item) => {
        const row = document.createElement('div');
        row.className = `memory-graph-row ${item.source} ${item.mode}`;
        row.innerHTML = `
            <span class="mg-time">${escapeHtml(item.stamp)}</span>
            <strong>${escapeHtml(item.source === 'graph' ? '知识图谱' : '记忆脑')}</strong>
            <em>${escapeHtml(memoryGraphModeLabel(item.mode))}</em>
            <div>
                <p>${escapeHtml(item.title)}</p>
                <small>${escapeHtml(item.detail || '无详情')}</small>
            </div>
        `;
        $memoryGraphList.appendChild(row);
    });
}

function memoryGraphModeLabel(mode) {
    switch (mode) {
        case 'read': return '读取';
        case 'write': return '写入';
        case 'idle': return '待命';
        default: return mode || '事件';
    }
}

function updateAgentStatus(key, state, detail) {
    const normalized = normalizeBrainKey(key);
    ensureBrainNode(normalized);
    brainState.agents[normalized] = {
        label: brainState.nodes[normalized].label,
        state,
        detail: detail || '',
        updatedAt: Date.now(),
    };
}

function renderAgentStatusList() {
    if (!$agentStatusList) return;
    const rows = [
        {
            label: '当前人格',
            state: brainState.activePersona || '未加载',
            detail: '会话人格配置',
            updatedAt: null,
        },
        {
            label: '当前会话',
            state: brainState.activeSessionTitle || '未选择',
            detail: activeSessionId || '',
            updatedAt: null,
        },
        ...Object.values(brainState.agents),
    ];

    $agentStatusList.innerHTML = '';
    rows.forEach((item) => {
        const row = document.createElement('div');
        row.className = 'agent-row';
        row.innerHTML = `
            <div>
                <strong>${escapeHtml(item.label)}</strong>
                <p>${escapeHtml(truncate(item.detail || '无详情', 90))}</p>
            </div>
            <span>${escapeHtml(truncate(item.state || '待命', 28))}</span>
        `;
        $agentStatusList.appendChild(row);
    });
}

function formatLastSeen(timestamp) {
    if (!timestamp) return '未启动';
    const seconds = Math.max(0, Math.floor((Date.now() - timestamp) / 1000));
    if (seconds < 3) return '刚刚';
    if (seconds < 60) return `${seconds}s 前`;
    return `${Math.floor(seconds / 60)}m 前`;
}

function personaLabel(personas, activeId) {
    const item = (personas || []).find((p) => p.id === activeId);
    return item ? item.name : activeId || '';
}

function updateActiveSessionTitle() {
    const item = sessionCache.find((s) => s.id === activeSessionId);
    brainState.activeSessionTitle = item ? item.title : activeSessionId || '';
}

// ── Session List ────────────────────────────────────────────────
function renderSessionList(sessions) {
    $sessionList.innerHTML = '';
    sessions.forEach((s) => {
        const el = document.createElement('div');
        el.className = 'session-item' + (s.id === activeSessionId ? ' active' : '');
        const canDelete = s.id !== activeSessionId && sessions.length > 1;
        el.innerHTML = `
            <div class="session-main">
                <div class="session-title">${escapeHtml(s.title)}</div>
                <div class="session-meta">${s.message_count} 条消息</div>
            </div>
            ${canDelete ? '<button class="session-delete" type="button" title="删除会话">×</button>' : ''}
        `;
        el.addEventListener('click', () => {
            send('switch_session', { session_id: s.id });
        });
        const deleteBtn = el.querySelector('.session-delete');
        if (deleteBtn) {
            deleteBtn.addEventListener('click', (e) => {
                e.stopPropagation();
                if (isGenerating) {
                    addSystemMessage('生成中暂不能删除会话');
                    return;
                }
                const ok = window.confirm(`删除会话「${s.title}」？`);
                if (!ok) return;
                send('delete_session', { session_id: s.id });
            });
        }
        $sessionList.appendChild(el);
    });
}

function renderMessages(messages) {
    $messages.innerHTML = '';
    brainState.conclusions = [];
    brainState.conclusionBrain = 'all';
    toolItems.clear();
    chatExchangeItems.clear();
    currentStreamingEl = null;
    currentThinkingEl = null;
    currentThinkingDetails = null;
    currentToolGroup = null;
    resetThinkingState();
    if (messages.length === 0) {
        showWelcome();
        return;
    }
    const lastUserIndex = messages.reduce(
        (latest, message, index) => message.role === 'user' ? index : latest,
        -1,
    );
    let lastAssistant = null;
    messages.forEach((m, index) => {
        if (m.role === 'user') {
            addUserMessage(m.content, {
                messageIndex: index,
                messageId: m.id || null,
                isLastUser: index === lastUserIndex,
            });
        } else if (m.role === 'assistant') {
            const el = document.createElement('div');
            el.className = 'msg assistant';
            renderMarkdown(el, m.content);
            attachPreviewAction(el, '主脑历史回复', '主脑', m.content);
            attachDeleteAction(el, index);
            $messages.appendChild(el);
            lastAssistant = el;
            addBrainConclusion('main', m.content, '历史回复', `history-main-${index}`, false);
        } else if (m.role === 'brain_communication' && m.exchange) {
            const request = m.exchange.request || null;
            const response = m.exchange.response || null;
            renderChatExchange({
                id: `exchange-${m.exchange.exchange_id}`,
                exchangeId: m.exchange.exchange_id,
                synthetic: false,
                kind: request?.kind || response?.kind || 'delegation',
                request,
                response,
                updatedAt: Date.parse(response?.occurred_at || request?.occurred_at || m.timestamp) || 0,
            }, false);
            if (response?.content) {
                addBrainConclusion(
                    response.sender || 'agent',
                    response.content,
                    response.title || '最终结果',
                    `exchange-${m.exchange.exchange_id}`,
                    false,
                );
            }
        } else {
            const el = addSystemMessage(m.content);
            attachDeleteAction(el, index);
        }
    });
    if (lastAssistant) appendModifiedFiles(lastAssistant, sessionFiles);
    renderConclusionPanel();
    refreshIcons();
    scrollToBottom();
}

function appendModifiedFiles(container, files, runId = null) {
    container.querySelector(':scope > .modified-files')?.remove();
    const normalizedFiles = Array.from(new Map((files || [])
        .filter((file) => typeof file?.path === 'string' && file.path.trim())
        .map((file) => {
            const path = file.path.trim();
            const changeKind = file.change_kind === 'added' ? 'added' : 'modified';
            return [path, {
                name: file.name || localFileName(path),
                path,
                change_kind: changeKind,
                run_id: file.run_id || runId || null,
            }];
        })).values());
    if (!normalizedFiles.length) return;
    const section = document.createElement('section');
    section.className = 'modified-files';
    const heading = document.createElement('div');
    heading.className = 'modified-files-heading';
    heading.innerHTML = `<strong>本轮变更文件</strong><span>${normalizedFiles.length} 个</span>`;
    section.appendChild(heading);
    const list = document.createElement('div');
    list.className = 'modified-file-list';
    normalizedFiles.forEach((file) => {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = `modified-file-link ${file.change_kind}`;
        button.dataset.filePath = file.path;
        button.title = `预览并编辑 ${file.path}`;
        button.innerHTML = `
            <i data-lucide="${file.change_kind === 'added' ? 'file-plus-2' : 'file-pen-line'}" aria-hidden="true"></i>
            <span class="modified-file-path">${escapeHtml(file.path)}</span>
            <span class="modified-file-kind">${file.change_kind === 'added' ? '新增' : '修改'}</span>
        `;
        button.addEventListener('click', async () => {
            await openLocalFileEditor(file, button);
        });
        list.appendChild(button);
    });
    section.appendChild(list);
    container.appendChild(section);
    refreshIcons();
}

function localFileName(path) {
    return String(path || '').split(/[\\/]/).filter(Boolean).at(-1) || '文件';
}

function localFileLanguage(name) {
    const extension = String(name || '').split('.').at(-1)?.toLowerCase();
    return ({
        c: 'C', cpp: 'C++', css: 'CSS', go: 'Go', html: 'HTML', java: 'Java',
        js: 'JavaScript', json: 'JSON', jsx: 'JSX', md: 'Markdown', py: 'Python',
        rs: 'Rust', sh: 'Shell', sql: 'SQL', toml: 'TOML', ts: 'TypeScript',
        tsx: 'TSX', xml: 'XML', yaml: 'YAML', yml: 'YAML',
    })[extension] || '文本';
}

function localFileExtensionKind(file) {
    const candidate = String(file?.name || file?.path || '').trim();
    if (/\.(?:md|markdown)$/iu.test(candidate)) return 'markdown';
    return /(?:^|[/\\])[^./\\]+$/u.test(candidate) ? 'extensionless' : 'other';
}

function extensionlessContentIsMarkdown(content) {
    if (typeof marked === 'undefined' || typeof marked.lexer !== 'function') return false;
    try {
        const markdownTokens = new Set([
            'blockquote', 'code', 'def', 'heading', 'hr', 'list', 'table',
        ]);
        const inlineMarkdownTokens = new Set([
            'codespan', 'del', 'em', 'image', 'link', 'strong',
        ]);
        const containsMarkdown = (tokens) => (tokens || []).some((token) => (
            markdownTokens.has(token.type)
            || inlineMarkdownTokens.has(token.type)
            || containsMarkdown(token.tokens)
            || containsMarkdown(token.items)
        ));
        return containsMarkdown(marked.lexer(String(content || '')));
    } catch (_error) {
        return false;
    }
}

function renderLocalFileMarkdown(content) {
    const source = String(content || '');
    if (typeof marked === 'undefined' || typeof DOMPurify === 'undefined') {
        $previewContent.innerHTML = '';
        $previewContent.textContent = source;
        return;
    }
    $previewContent.innerHTML = DOMPurify.sanitize(marked.parse(source), {
        USE_PROFILES: { html: true },
    });
    const codeBlocks = $previewContent.querySelectorAll?.('pre code') || [];
    codeBlocks.forEach((block) => {
        if (typeof hljs !== 'undefined') hljs.highlightElement(block);
    });
}

function focusLocalFileSurface() {
    if (fileEditorMode === 'preview') $previewContent.focus();
    else $fileEditorContent.focus();
}

function setLocalFileEditorMode(mode, options = {}) {
    if (!openedLocalFile) return false;
    const supportsPreview = fileEditorSupportsMarkdown;
    fileEditorMode = supportsPreview && mode === 'preview' ? 'preview' : 'edit';
    const previewing = fileEditorMode === 'preview';
    $fileEditorModeSwitch.classList.toggle('hidden', !supportsPreview);
    $fileEditorPreviewMode.classList.toggle('active', previewing);
    $fileEditorEditMode.classList.toggle('active', !previewing);
    $fileEditorPreviewMode.setAttribute('aria-pressed', String(previewing));
    $fileEditorEditMode.setAttribute('aria-pressed', String(!previewing));
    $previewContent.classList.toggle('hidden', !previewing);
    $fileEditorFrame.classList.toggle('hidden', previewing);
    $fileEditorQuote.disabled = previewing
        || $fileEditorContent.selectionStart === $fileEditorContent.selectionEnd;
    if (previewing) {
        const content = $fileEditorContent.value;
        currentPreview = buildPreviewMetadata(
            openedLocalFile.name,
            openedLocalFile.path,
            content,
            { fileName: openedLocalFile.name, path: openedLocalFile.path },
        );
        renderLocalFileMarkdown(content);
    } else {
        $previewSelectionTools.classList.add('hidden');
        currentPreviewSelection = null;
    }
    if (options.focus !== false) focusLocalFileSurface();
    return true;
}

function shortFileRevision(revision) {
    return revision ? String(revision).slice(0, 8) : '-';
}

function setFileEditorSaveState(state, message) {
    $fileEditorSaveState.dataset.state = state;
    $fileEditorStatus.textContent = message;
}

function setFileEditorDirty(dirty) {
    fileEditorDirty = dirty;
    $fileEditorTab.classList.toggle('is-dirty', dirty);
}

function updateFileEditorCaret() {
    const beforeCaret = $fileEditorContent.value.slice(0, $fileEditorContent.selectionStart);
    const rows = beforeCaret.split('\n');
    $fileEditorCaret.textContent = `行 ${rows.length}，列 ${rows.at(-1).length + 1}`;
}

function updateFileEditorMetrics() {
    const totalLines = Math.max(1, $fileEditorContent.value.split('\n').length);
    $fileEditorLineNumbers.textContent = Array.from(
        { length: totalLines },
        (_, index) => index + 1,
    ).join('\n');
    $fileEditorLines.textContent = `${totalLines} 行`;
    $fileEditorLineEnding.textContent = $fileEditorContent.value.includes('\r\n') ? 'CRLF' : 'LF';
    updateFileEditorCaret();
}

function syncFileEditorLineNumbers() {
    $fileEditorLineNumbers.style.transform = `translateY(${-$fileEditorContent.scrollTop}px)`;
}

function setFileEditorContent(content, preserveView = false) {
    const view = preserveView ? {
        start: $fileEditorContent.selectionStart,
        end: $fileEditorContent.selectionEnd,
        top: $fileEditorContent.scrollTop,
        left: $fileEditorContent.scrollLeft,
    } : null;
    $fileEditorContent.value = String(content || '');
    updateFileEditorMetrics();
    if (view) {
        const max = $fileEditorContent.value.length;
        $fileEditorContent.setSelectionRange(Math.min(view.start, max), Math.min(view.end, max));
        $fileEditorContent.scrollTop = view.top;
        $fileEditorContent.scrollLeft = view.left;
    } else {
        $fileEditorContent.setSelectionRange(0, 0);
        $fileEditorContent.scrollTop = 0;
        $fileEditorContent.scrollLeft = 0;
    }
    syncFileEditorLineNumbers();
}

function showLocalFileEditorSurface() {
    $previewSelectionTools.classList.add('hidden');
    $fileEditorTabs.classList.remove('hidden');
    $fileEditorToolbar.classList.remove('hidden');
    $fileEditorStatusbar.classList.remove('hidden');
    $fileEditorRefresh.classList.remove('hidden');
    $fileEditorCopyPath.classList.remove('hidden');
    $markdownPreview.classList.add('open');
    $markdownPreview.setAttribute('aria-hidden', 'false');
    $previewBackdrop.classList.remove('hidden');
    document.body.classList.add('preview-open');
    setLocalFileEditorMode(fileEditorMode, { focus: false });
}

function applyLocalFileSnapshot(snapshot, options = {}) {
    if (!openedLocalFile) return;
    const initialLoad = !openedLocalFile.revision;
    openedLocalFile.name = snapshot.name || openedLocalFile.name;
    openedLocalFile.path = snapshot.path || openedLocalFile.path;
    openedLocalFile.revision = snapshot.revision;
    const extensionKind = localFileExtensionKind(openedLocalFile);
    fileEditorSupportsMarkdown = extensionKind === 'markdown'
        || (extensionKind === 'extensionless'
            && extensionlessContentIsMarkdown(snapshot.content));
    if (initialLoad || !fileEditorSupportsMarkdown) {
        fileEditorMode = fileEditorSupportsMarkdown ? 'preview' : 'edit';
    }
    fileEditorConflictSnapshot = null;
    $fileEditorConflict.classList.add('hidden');
    $previewTitle.textContent = openedLocalFile.name;
    $previewSource.textContent = openedLocalFile.path;
    $fileEditorTabName.textContent = openedLocalFile.name;
    $fileEditorLanguage.textContent = localFileLanguage(openedLocalFile.name);
    $fileEditorRevision.textContent = `版本 ${shortFileRevision(snapshot.revision)}`;
    $fileEditorRevision.title = snapshot.revision || '';
    setFileEditorContent(snapshot.content, Boolean(options.preserveView));
    setFileEditorDirty(false);
    $fileEditorContent.disabled = false;
    $fileEditorQuote.disabled = true;
    currentPreview = buildPreviewMetadata(
        openedLocalFile.name,
        openedLocalFile.path,
        snapshot.content,
        { fileName: openedLocalFile.name, path: openedLocalFile.path },
    );
    setLocalFileEditorMode(fileEditorMode, { focus: false });
    setFileEditorSaveState('saved', options.message || '已加载最新内容');
}

function enterLocalFileConflict(snapshot) {
    if (!openedLocalFile || !snapshot?.revision) return;
    clearTimeout(fileSaveTimer);
    fileEditorConflictSnapshot = snapshot;
    setFileEditorDirty(true);
    $fileEditorConflict.classList.remove('hidden');
    $fileEditorRevision.textContent = `版本 ${shortFileRevision(openedLocalFile.revision)} → ${shortFileRevision(snapshot.revision)}`;
    $fileEditorRevision.title = snapshot.revision;
    setFileEditorSaveState('conflict', '检测到磁盘冲突');
}

function scheduleLocalFileSave() {
    clearTimeout(fileSaveTimer);
    if (!openedLocalFile || !fileEditorDirty || fileEditorConflictSnapshot) return;
    fileSaveTimer = setTimeout(() => saveOpenedLocalFile(), 700);
}

async function fetchLocalFileSnapshot(path, runId = null) {
    const query = new URLSearchParams({ path });
    if (runId) query.set('run_id', runId);
    const response = await fetch(`/api/local-file?${query}`);
    const data = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(data.error || '文件读取失败');
    if (!data.revision) throw new Error('文件读取结果缺少版本信息');
    return data;
}

async function openLocalFileEditor(file, trigger = null) {
    const requestedPath = String(file?.path || '').trim();
    if (!requestedPath) return false;
    if (openedLocalFile?.path === requestedPath && $markdownPreview.classList.contains('open')) {
        await checkLatestLocalFile({ announce: true });
        focusLocalFileSurface();
        return true;
    }
    if (openedLocalFile) {
        const flushed = await flushOpenedLocalFile();
        if (!flushed) {
            showToast('请先处理当前文件的保存冲突');
            $fileEditorContent.focus();
            return false;
        }
        resetLocalFileEditorSession();
    }

    const requestId = ++fileEditorRequestId;
    stopLocalFileWatcher();
    clearTimeout(fileSaveTimer);
    openedLocalFile = {
        name: file.name || localFileName(requestedPath),
        path: requestedPath,
        revision: null,
        change_kind: file.change_kind === 'added' ? 'added' : 'modified',
        run_id: file.run_id || null,
    };
    fileEditorSupportsMarkdown = localFileExtensionKind(openedLocalFile) === 'markdown';
    fileEditorMode = fileEditorSupportsMarkdown ? 'preview' : 'edit';
    openedLocalFileTrigger = trigger;
    document.querySelectorAll('.modified-file-link').forEach((button) => {
        button.classList.toggle('active', button === trigger);
    });
    fileEditorConflictSnapshot = null;
    $fileEditorConflict.classList.add('hidden');
    $previewTitle.textContent = openedLocalFile.name;
    $previewSource.textContent = openedLocalFile.path;
    $fileEditorTabName.textContent = openedLocalFile.name;
    $fileEditorLanguage.textContent = localFileLanguage(openedLocalFile.name);
    $fileEditorRevision.textContent = '版本 -';
    $fileEditorContent.disabled = true;
    $fileEditorQuote.disabled = true;
    setFileEditorDirty(false);
    setFileEditorContent('');
    setFileEditorSaveState('loading', '正在加载最新内容...');
    showLocalFileEditorSurface();
    refreshIcons();

    try {
        const snapshot = await fetchLocalFileSnapshot(requestedPath, openedLocalFile.run_id);
        if (requestId !== fileEditorRequestId || !openedLocalFile) return false;
        applyLocalFileSnapshot(snapshot);
        startLocalFileWatcher();
        focusLocalFileSurface();
        return true;
    } catch (error) {
        if (requestId !== fileEditorRequestId) return false;
        resetLocalFileEditorSession();
        $markdownPreview.classList.remove('open');
        $markdownPreview.setAttribute('aria-hidden', 'true');
        $previewBackdrop.classList.add('hidden');
        document.body.classList.remove('preview-open');
        addSystemMessage(`无法预览文件：${error.message}`);
        return false;
    }
}

async function performOpenedLocalFileSave(options = {}) {
    if (!openedLocalFile || !fileEditorDirty) return true;
    if (fileEditorConflictSnapshot && !options.adoptLatest) return false;
    clearTimeout(fileSaveTimer);
    if (options.adoptLatest && fileEditorConflictSnapshot) {
        openedLocalFile.revision = fileEditorConflictSnapshot.revision;
        fileEditorConflictSnapshot = null;
        $fileEditorConflict.classList.add('hidden');
    }
    const requestId = fileEditorRequestId;
    const savingPath = openedLocalFile.path;
    const savingContent = $fileEditorContent.value;
    const expectedRevision = openedLocalFile.revision;
    if (!expectedRevision) return false;
    setFileEditorSaveState('saving', '正在自动保存...');
    try {
        const response = await fetch('/api/local-file', {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({
                path: savingPath,
                content: savingContent,
                expected_revision: expectedRevision,
                run_id: openedLocalFile.run_id,
            }),
        });
        const data = await response.json().catch(() => ({}));
        if (requestId !== fileEditorRequestId || openedLocalFile?.path !== savingPath) return true;
        if (response.status === 409) {
            enterLocalFileConflict(data);
            return false;
        }
        if (!response.ok) throw new Error(data.error || '保存失败');
        openedLocalFile.revision = data.revision;
        $fileEditorRevision.textContent = `版本 ${shortFileRevision(data.revision)}`;
        $fileEditorRevision.title = data.revision || '';
        if ($fileEditorContent.value === savingContent) {
            setFileEditorDirty(false);
            setFileEditorSaveState('saved', `已自动保存 ${new Date().toLocaleTimeString('zh-CN', { hour12: false })}`);
        } else {
            setFileEditorDirty(true);
            setFileEditorSaveState('dirty', '有新的未保存修改');
            scheduleLocalFileSave();
        }
        return true;
    } catch (error) {
        if (requestId !== fileEditorRequestId) return false;
        setFileEditorDirty(true);
        setFileEditorSaveState('error', `保存失败：${error.message}`);
        return false;
    }
}

function saveOpenedLocalFile(options = {}) {
    if (fileSavePromise) {
        return fileSavePromise.then((saved) => {
            if (options.adoptLatest && fileEditorConflictSnapshot) {
                return saveOpenedLocalFile(options);
            }
            return saved;
        });
    }
    fileSavePromise = performOpenedLocalFileSave(options).finally(() => {
        fileSavePromise = null;
    });
    return fileSavePromise;
}

async function flushOpenedLocalFile() {
    clearTimeout(fileSaveTimer);
    for (let attempt = 0; attempt < 4; attempt += 1) {
        if (fileEditorConflictSnapshot) return false;
        if (fileSavePromise) {
            const saved = await fileSavePromise;
            if (!saved) return false;
            continue;
        }
        if (!fileEditorDirty) return true;
        const saved = await saveOpenedLocalFile();
        if (!saved) return false;
    }
    return !fileEditorDirty;
}

async function checkLatestLocalFile(options = {}) {
    if (!openedLocalFile || !$markdownPreview.classList.contains('open')) return false;
    const requestId = fileEditorRequestId;
    const checkingPath = openedLocalFile.path;
    try {
        const latest = await fetchLocalFileSnapshot(checkingPath, openedLocalFile.run_id);
        if (requestId !== fileEditorRequestId || openedLocalFile?.path !== checkingPath) return false;
        // The matching save response is the authoritative acknowledgement for our own write.
        // A concurrent refresh can otherwise observe that write first and report a false conflict.
        if (fileSavePromise) return false;
        if (latest.revision === openedLocalFile.revision) {
            if (options.announce) showToast('当前已是磁盘最新内容');
            return false;
        }
        if (fileEditorDirty || fileSavePromise) {
            enterLocalFileConflict(latest);
            return false;
        }
        applyLocalFileSnapshot(latest, {
            preserveView: true,
            message: '已自动加载磁盘最新版本',
        });
        if (options.announce) showToast('已加载磁盘最新内容');
        return true;
    } catch (error) {
        if (options.announce) showToast(`检查最新内容失败：${error.message}`);
        return false;
    }
}

function startLocalFileWatcher() {
    stopLocalFileWatcher();
    fileEditorWatcherTimer = setInterval(() => checkLatestLocalFile(), 4000);
}

function stopLocalFileWatcher() {
    clearInterval(fileEditorWatcherTimer);
    fileEditorWatcherTimer = null;
}

function resetLocalFileEditorSession() {
    clearTimeout(fileSaveTimer);
    stopLocalFileWatcher();
    fileEditorRequestId += 1;
    fileEditorConflictSnapshot = null;
    fileEditorDirty = false;
    fileEditorMode = 'edit';
    fileEditorSupportsMarkdown = false;
    openedLocalFile = null;
    openedLocalFileTrigger = null;
    document.querySelectorAll('.modified-file-link.active').forEach((button) => {
        button.classList.remove('active');
    });
    $fileEditorTab.classList.remove('is-dirty');
    $fileEditorConflict.classList.add('hidden');
    $fileEditorTabs.classList.add('hidden');
    $fileEditorToolbar.classList.add('hidden');
    $fileEditorModeSwitch.classList.add('hidden');
    $fileEditorFrame.classList.add('hidden');
    $fileEditorStatusbar.classList.add('hidden');
    $fileEditorRefresh.classList.add('hidden');
    $fileEditorCopyPath.classList.add('hidden');
    $previewContent.classList.add('hidden');
    $previewContent.innerHTML = '';
    $previewContent.textContent = '';
}

function loadConflictingDiskVersion() {
    if (!fileEditorConflictSnapshot) return;
    applyLocalFileSnapshot(fileEditorConflictSnapshot, { message: '已加载磁盘版本' });
    showToast('已加载磁盘版本');
    focusLocalFileSurface();
}

async function keepAndSaveLocalFile() {
    if (!fileEditorConflictSnapshot) return;
    setFileEditorSaveState('dirty', '准备保存当前修改...');
    const saved = await saveOpenedLocalFile({ adoptLatest: true });
    if (saved) showToast('当前修改已保存为最新版本');
    focusLocalFileSurface();
}

async function copyOpenedLocalFilePath() {
    if (!openedLocalFile) return;
    try {
        await navigator.clipboard.writeText(openedLocalFile.path);
    } catch (_) {
        const helper = document.createElement('textarea');
        helper.value = openedLocalFile.path;
        helper.style.position = 'fixed';
        helper.style.opacity = '0';
        document.body.appendChild(helper);
        helper.select();
        document.execCommand('copy');
        helper.remove();
    }
    showToast('绝对路径已复制');
}

function quoteFileEditorSelection() {
    if (!openedLocalFile) return;
    const start = $fileEditorContent.selectionStart;
    const end = $fileEditorContent.selectionEnd;
    if (start === end) return;
    const text = $fileEditorContent.value.slice(start, end);
    const startLine = $fileEditorContent.value.slice(0, start).split('\n').length;
    const endLine = startLine + text.split('\n').length - 1;
    const lines = startLine === endLine ? `${startLine}` : `${startLine}-${endLine}`;
    const quoted = text.split('\n').map((line) => `> ${line}`).join('\n');
    const reference = `[引用文件: ${openedLocalFile.name} | 路径: ${openedLocalFile.path} | 行号: ${lines}]\n${quoted}\n\n`;
    $input.setRangeText(reference, $input.selectionStart, $input.selectionEnd, 'end');
    switchView('chat');
    $input.focus();
}

// ── Persona List ────────────────────────────────────────────────
function renderPersonaList(personas, activeId) {
    $personaSelect.innerHTML = '';
    personas.forEach((p) => {
        const opt = document.createElement('option');
        opt.value = p.id;
        opt.textContent = p.name;
        if (p.id === activeId) opt.selected = true;
        $personaSelect.appendChild(opt);
    });
}

// ── Ask Modal ───────────────────────────────────────────────────
function showAskModal(question, options) {
    $askQuestion.textContent = question;
    $askOptions.innerHTML = '';

    if (options && options.length > 0) {
        options.forEach((opt) => {
            const btn = document.createElement('button');
            btn.textContent = opt;
            btn.addEventListener('click', () => {
                send('ask_response', { response: opt });
                $askModal.classList.add('hidden');
            });
            $askOptions.appendChild(btn);
        });
    } else {
        const input = document.createElement('input');
        input.type = 'text';
        input.style.cssText = 'background:var(--bg-primary);color:var(--text-primary);border:1px solid var(--border);border-radius:6px;padding:8px 12px;width:100%;font-size:14px;';
        input.placeholder = '输入回复...';
        const btn = document.createElement('button');
        btn.textContent = '发送';
        btn.addEventListener('click', () => {
            send('ask_response', { response: input.value });
            $askModal.classList.add('hidden');
        });
        input.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') {
                e.preventDefault();
                btn.click();
            }
        });
        $askOptions.appendChild(input);
        $askOptions.appendChild(btn);
        setTimeout(() => input.focus(), 100);
    }

    $askModal.classList.remove('hidden');
}

// ── Stop / Cancel ───────────────────────────────────────────────
function stopGenerating() {
    if (!isGenerating) return;
    send('cancel');
    isGenerating = false;
    removeSpinner();
    addSystemMessage('已停止生成');
    finalizeStreaming();
    setInputEnabled(true);
}

// ── Submit Query ────────────────────────────────────────────────
function beginHistoryRegeneration(label) {
    currentTurnFiles = [];
    turnFileBaseline = new Map(sessionFiles.map((file) => [file.path, file.updated_at]));
    brainState.activeSince = Date.now();
    markBrain('main', 'active', label);
    addCommunication('client', 'main', label, 'query');
    addBrainEvent(label);
    isGenerating = true;
    setInputEnabled(true);
    updateSendButton();

    currentStreamingEl = null;
    currentThinkingEl = null;
    currentThinkingDetails = null;
    currentToolGroup = null;
    toolItems.clear();
    resetThinkingState();
}

function submitQuery() {
    if (roomSnapshot?.room?.room_id === activeSessionId) {
        submitCollaborationMessage();
        return;
    }
    const text = $input.value.trim();
    if (!text) return;

    addUserMessage(text);
    beginHistoryRegeneration('用户提交新任务');
    send('query', { input: text });
    $input.value = '';
    resetComposerHeight();
}

// ── Event Bindings ──────────────────────────────────────────────

// IME 组合态：中文输入法开始组合时不触发提交
$input.addEventListener('compositionstart', () => {
    isComposing = true;
});

$input.addEventListener('compositionend', () => {
    isComposing = false;
});

$input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
        // 正在 IME 组合态（如中文候选窗），不拦截，让输入法处理确认
        if (isComposing) return;
        e.preventDefault();
        if (isGenerating && !roomSnapshot) return; // 旧查询生成中不重复提交
        submitQuery();
    }
    // 生成中 ESC 停止
    if (e.key === 'Escape' && isGenerating) {
        e.preventDefault();
        stopGenerating();
    }
});

// 全局快捷键：生成中 Ctrl+C 停止
document.addEventListener('keydown', (e) => {
    if (isGenerating && e.ctrlKey && e.key === 'c') {
        // 仅在输入框无选区时拦截，避免影响正常复制
        const sel = window.getSelection();
        if (!sel || sel.toString().length === 0) {
            e.preventDefault();
            stopGenerating();
        }
    }
});

// 发送按钮：根据状态切换发送/停止
$sendBtn.addEventListener('click', () => {
    if (isGenerating && !roomSnapshot) {
        stopGenerating();
    } else {
        submitQuery();
    }
});

// Auto-resize textarea
$input.addEventListener('input', () => {
    scheduleComposerResize();
    syncMentionRecipients();
});

$messages.addEventListener('scroll', () => {
    roomTimelineNearBottom = RoomReply.isTimelineNearBottom($messages);
}, { passive: true });

$roomMode.querySelectorAll('[data-mode]').forEach((button) => {
    button.addEventListener('click', () => {
        roomMode = button.dataset.mode;
        $roomMode.querySelectorAll('[data-mode]').forEach((candidate) => {
            candidate.classList.toggle('active', candidate === button);
        });
    });
});

$addMemberBtn.addEventListener('click', () => openMemberModal());
$roomRefreshBtn.addEventListener('click', () => send('request_room_snapshot'));
$roomDirectoryBtn.addEventListener('click', openRoomDirectoryModal);
$loadEarlierEvents.addEventListener('click', () => {
    if (!roomSnapshot || !hasEarlierRoomEvents) return;
    const beforeSequence = Number(roomSnapshot.events[0]?.sequence || 1);
    if (!beginPendingRoomOperation({
        type: 'pagination',
        roomId: roomSnapshot.room.room_id,
        beforeSequence,
    })) {
        showToast('请等待当前房间操作完成');
        return;
    }
    const sent = send('load_room_events_before', {
        before_sequence: beforeSequence,
        limit: 100,
    });
    if (!sent) {
        failPendingRoomOperation();
        showToast('连接不可用，无法加载较早消息');
    }
});
$replyCancel.addEventListener('click', () => {
    replyState = null;
    renderReplyPreview();
});
$memberModalClose.addEventListener('click', closeMemberModal);
$memberModalCancel.addEventListener('click', closeMemberModal);
$memberModal.addEventListener('click', (event) => {
    if (event.target === $memberModal) closeMemberModal();
});
$memberForm.addEventListener('submit', (event) => {
    event.preventDefault();
    submitMemberForm();
});
$roomDirectoryModalClose.addEventListener('click', closeRoomDirectoryModal);
$roomDirectoryModalCancel.addEventListener('click', closeRoomDirectoryModal);
$roomDirectoryModal.addEventListener('click', (event) => {
    if (event.target === $roomDirectoryModal) closeRoomDirectoryModal();
});
$roomDirectoryForm.addEventListener('submit', (event) => {
    event.preventDefault();
    submitRoomDirectoryForm();
});

$personaSelect.addEventListener('change', () => {
    send('switch_persona', { persona_id: $personaSelect.value });
});

$newSessionBtn.addEventListener('click', () => {
    send('new_session');
});

$chatTab.addEventListener('click', () => {
    switchView('chat');
});

$cockpitTab.addEventListener('click', () => {
    switchView('cockpit');
});

$cockpitReset.addEventListener('click', () => {
    brainState.links = [];
    brainState.events = [];
    brainState.communications = [];
    brainState.conclusions = [];
    brainState.conclusionBrain = 'all';
    brainState.memoryGraphOps = [];
    brainState.agents = {};
    brainState.metrics = {
        toolCalls: 0,
        memoryRefs: 0,
        graphReads: 0,
        graphWrites: 0,
        errors: 0,
        retries: 0,
    };
    brainState.activeSince = null;
    expandedCommunicationIds.clear();
    collapsedCommunicationIds.clear();
    Object.values(brainState.nodes).forEach((node) => {
        node.status = 'idle';
        node.detail = '待命';
        node.active = false;
    });
    renderCockpit();
});

$sidebarToggle.addEventListener('click', () => {
    $sidebar.classList.toggle('open');
});

$previewClose.addEventListener('click', closeMarkdownPreview);
$previewBackdrop.addEventListener('click', closeMarkdownPreview);
$previewContent.addEventListener('mouseup', updatePreviewSelection);
$previewContent.addEventListener('keyup', updatePreviewSelection);
$previewQuote.addEventListener('click', quotePreviewSelection);
$fileEditorContent.addEventListener('input', () => {
    setFileEditorDirty(true);
    setFileEditorSaveState(
        'dirty',
        fileEditorConflictSnapshot ? '等待处理磁盘冲突' : '有未保存修改',
    );
    updateFileEditorMetrics();
    scheduleLocalFileSave();
});
$fileEditorContent.addEventListener('scroll', syncFileEditorLineNumbers);
function updateFileEditorSelection() {
    $fileEditorQuote.disabled = $fileEditorContent.selectionStart === $fileEditorContent.selectionEnd;
    updateFileEditorCaret();
}
$fileEditorContent.addEventListener('select', updateFileEditorSelection);
$fileEditorContent.addEventListener('click', updateFileEditorSelection);
$fileEditorContent.addEventListener('keyup', updateFileEditorSelection);
$fileEditorContent.addEventListener('keydown', (event) => {
    if (event.key !== 'Tab') return;
    event.preventDefault();
    $fileEditorContent.setRangeText(
        '    ',
        $fileEditorContent.selectionStart,
        $fileEditorContent.selectionEnd,
        'end',
    );
    $fileEditorContent.dispatchEvent(new Event('input', { bubbles: true }));
});
$fileEditorQuote.addEventListener('click', quoteFileEditorSelection);
$fileEditorPreviewMode.addEventListener('click', () => setLocalFileEditorMode('preview'));
$fileEditorEditMode.addEventListener('click', () => setLocalFileEditorMode('edit'));
$fileEditorRefresh.addEventListener('click', () => checkLatestLocalFile({ announce: true }));
$fileEditorCopyPath.addEventListener('click', copyOpenedLocalFilePath);
$fileEditorKeepLocal.addEventListener('click', keepAndSaveLocalFile);
$fileEditorLoadDisk.addEventListener('click', loadConflictingDiskVersion);
$fileEditorTab.addEventListener('click', focusLocalFileSurface);
document.addEventListener('keydown', (event) => {
    if ((event.metaKey || event.ctrlKey)
        && event.key.toLowerCase() === 's'
        && openedLocalFile
        && $markdownPreview.classList.contains('open')) {
        event.preventDefault();
        saveOpenedLocalFile();
        return;
    }
    if (event.key === 'Escape' && $markdownPreview.classList.contains('open')) {
        closeMarkdownPreview();
    }
});
window.addEventListener('focus', () => checkLatestLocalFile());
document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') {
        checkLatestLocalFile();
    }
});
window.addEventListener('beforeunload', (event) => {
    if (!fileEditorDirty) return;
    event.preventDefault();
    event.returnValue = '';
});

// Close sidebar when clicking outside on mobile
document.addEventListener('click', (e) => {
    if (window.innerWidth <= 768 &&
        !$sidebar.contains(e.target) &&
        e.target !== $sidebarToggle) {
        $sidebar.classList.remove('open');
    }
});

$thinkingToggle.addEventListener('click', () => {
    showThinking = !showThinking;
    $thinkingToggle.classList.toggle('active', showThinking);
    $thinkingToggle.setAttribute('aria-pressed', String(showThinking));
    $thinkingToggle.title = showThinking ? '收起全部思考' : '展开全部思考';
    $thinkingToggle.setAttribute('aria-label', $thinkingToggle.title);
    document.querySelectorAll('details.reasoning-trace').forEach((el) => {
        el.open = showThinking;
    });
});

// Close ask modal on backdrop click
$askModal.addEventListener('click', (e) => {
    if (e.target === $askModal) {
        // Don't close - user must answer
    }
});

function switchView(view) {
    currentView = view;
    const showCockpit = view === 'cockpit';
    $chatArea.classList.toggle('active', !showCockpit);
    $cockpitArea.classList.toggle('active', showCockpit);
    $chatTab.classList.toggle('active', !showCockpit);
    $cockpitTab.classList.toggle('active', showCockpit);
    if (showCockpit) {
        renderCockpit();
    }
}

// ── Init ────────────────────────────────────────────────────────
connect();
renderCockpit();
refreshIcons();
setInterval(() => {
    if (currentView === 'cockpit' || brainState.activeSince) {
        renderCockpit();
    }
}, 1000);
$input.focus();
