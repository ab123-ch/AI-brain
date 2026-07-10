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
const toolItems = new Map();
const chatExchangeItems = new Map();
const expandedCommunicationIds = new Set();
const collapsedCommunicationIds = new Set();

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
const $agentStatusList = document.getElementById('agent-status-list');
const $memoryGraphList = document.getElementById('memory-graph-list');

// ── Cockpit State ───────────────────────────────────────────────
const brainProfiles = {
    client: { label: '用户端', role: '输入 / WebSocket 会话', icon: 'U' },
    main: { label: '主脑', role: '任务规划 / 汇总输出', icon: 'M' },
    memory: { label: '记忆脑', role: '历史召回 / 上下文注入', icon: 'R' },
    graph: { label: '知识图谱', role: '语义节点 / 关系读写', icon: 'G' },
    eval: { label: '评估脑', role: '质量检查 / 结果评估', icon: 'E' },
    tool: { label: '工具执行器', role: '本地命令 / 文件 / 外部工具', icon: 'T' },
    agent: { label: '子代理池', role: '并行专题处理', icon: 'A' },
    novel: { label: '小说脑', role: '创作型子代理', icon: 'N' },
    explore: { label: '探索脑', role: '检索 / 方案探索', icon: 'X' },
};

const brainState = {
    nodes: {},
    links: [],
    events: [],
    communications: [],
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
    }
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

        case 'session_list':
            sessionCache = data.sessions || [];
            renderSessionList(data.sessions);
            updateActiveSessionTitle();
            renderCockpit();
            break;

        case 'session_switched':
            activeSessionId = data.session_id;
            updateActiveSessionTitle();
            renderMessages(data.messages);
            renderCockpit();
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
            brainState.metrics.errors += 1;
            addBrainEvent(`错误: ${data.message}`);
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

// ── Streaming Text (Typewriter) ─────────────────────────────────
function appendStreamingText(text) {
    currentToolGroup = null;
    if (!currentStreamingEl) {
        currentStreamingEl = document.createElement('div');
        currentStreamingEl.className = 'msg assistant';
        currentStreamingEl.dataset.rawText = '';
        $messages.appendChild(currentStreamingEl);
    }

    currentStreamingEl.dataset.rawText += text;
    renderMarkdown(currentStreamingEl, currentStreamingEl.dataset.rawText);
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
        currentToolGroup = null;
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
    const trace = createTraceDetails(
        'intermediate-block',
        `${displayBrainName(brain)} · 阶段结论`,
        new Date().toLocaleTimeString(),
        true,
    );
    trace.body.textContent = content;
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
        <h2>智脑 AI v2</h2>
        <p>一主二从架构 · 主脑 + 记忆脑 + 评估脑</p>
        <div class="welcome-tips">
            <div class="welcome-tip">输入查询开始对话</div>
            <div class="welcome-tip">Enter 发送 · Shift+Enter 换行</div>
        </div>
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

    if (!currentToolGroup) {
        currentToolGroup = document.createElement('div');
        currentToolGroup.className = 'tool-group';
        $messages.appendChild(currentToolGroup);
    }

    currentToolGroup.appendChild(item);
    toolItems.set(stableId, item);
    scrollToBottom();
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

function addUserMessage(text, messageIndex = null) {
    const el = document.createElement('div');
    el.className = 'msg user';
    el.textContent = text;
    attachDeleteAction(el, messageIndex);
    $messages.appendChild(el);
    scrollToBottom();
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
    $input.disabled = !enabled;
    if (enabled) {
        $input.focus();
    }
}

function updateSendButton() {
    if (isGenerating) {
        $sendBtn.classList.add('stop-mode');
        $sendBtn.innerHTML = '<i data-lucide="square"></i>';
        $sendBtn.title = '停止生成 (Esc / Ctrl+C)';
        $sendBtn.setAttribute('aria-label', '停止生成');
    } else {
        $sendBtn.classList.remove('stop-mode');
        $sendBtn.innerHTML = '<i data-lucide="send-horizontal"></i>';
        $sendBtn.title = '发送';
        $sendBtn.setAttribute('aria-label', '发送');
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

function renderChatExchange(item) {
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
        currentToolGroup = null;
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
    if (isNew) scrollToBottom();
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
    ['client', 'main', 'memory', 'graph', 'eval', 'tool', 'agent'].forEach(ensureBrainNode);
}

function cockpitIdleSummary() {
    if (brainState.currentModel) {
        return `已连接 ${brainState.currentModel}，等待任务进入`;
    }
    return '所有脑区待命，等待任务进入';
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
    messages.forEach((m, index) => {
        if (m.role === 'user') {
            addUserMessage(m.content, index);
        } else if (m.role === 'assistant') {
            const el = document.createElement('div');
            el.className = 'msg assistant';
            renderMarkdown(el, m.content);
            attachDeleteAction(el, index);
            $messages.appendChild(el);
        } else {
            const el = addSystemMessage(m.content);
            attachDeleteAction(el, index);
        }
    });
    scrollToBottom();
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
function submitQuery() {
    const text = $input.value.trim();
    if (!text) return;

    addUserMessage(text);
    brainState.activeSince = Date.now();
    markBrain('main', 'active', '接收用户任务');
    addCommunication('client', 'main', '提交新任务', 'query');
    addBrainEvent('用户提交新任务');
    send('query', { input: text });
    $input.value = '';
    $input.style.height = 'auto';
    isGenerating = true;
    setInputEnabled(true); // 不禁用输入框，只切换按钮状态
    updateSendButton();

    // Reset streaming state
    currentStreamingEl = null;
    currentThinkingEl = null;
    currentThinkingDetails = null;
    currentToolGroup = null;
    toolItems.clear();
    resetThinkingState();
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
        if (isGenerating) return; // 生成中不重复提交
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
    if (isGenerating) {
        stopGenerating();
    } else {
        submitQuery();
    }
});

// Auto-resize textarea
$input.addEventListener('input', () => {
    $input.style.height = 'auto';
    $input.style.height = Math.min($input.scrollHeight, 120) + 'px';
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
