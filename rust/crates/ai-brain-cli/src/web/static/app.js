// ── State ───────────────────────────────────────────────────────
let ws = null;
let reconnectAttempts = 0;
const MAX_RECONNECT = 5;
const HEARTBEAT_INTERVAL_MS = 25000; // 25秒心跳，服务端30秒Ping互补
let heartbeatTimer = null;
let showThinking = false;
let currentStreamingEl = null;
let currentThinkingEl = null;
let currentToolGroup = null;
let currentSpinnerEl = null;
let activeSessionId = null;
let isGenerating = false; // 是否正在生成回复
let isComposing = false;  // IME 组合态标记（中文输入法）
let currentView = 'chat';

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

// ── Cockpit State ───────────────────────────────────────────────
const brainState = {
    nodes: {
        main: { label: '主脑', status: 'idle', detail: '待命', active: false },
        memory: { label: '记忆脑', status: 'idle', detail: '待命', active: false },
        eval: { label: '评估脑', status: 'idle', detail: '待命', active: false },
        novel: { label: '小说脑', status: 'idle', detail: '待命', active: false },
    },
    links: [],
    events: [],
};

// ── WebSocket ───────────────────────────────────────────────────
function connect() {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    ws = new WebSocket(`${proto}//${location.host}/ws`);

    ws.onopen = () => {
        reconnectAttempts = 0;
        console.log('WebSocket connected');
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
        if (reconnectAttempts < MAX_RECONNECT) {
            const delay = Math.pow(2, reconnectAttempts) * 1000;
            reconnectAttempts++;
            setTimeout(connect, delay);
        }
    };

    ws.onerror = (err) => {
        console.error('WebSocket error:', err);
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
            markBrain(data.brain, 'active', `连接模型 ${data.model}`);
            addBrainEvent(`${displayBrainName(data.brain)} 连接 ${data.model}`);
            removeSpinner();
            currentSpinnerEl = addSystemMessage(`连接 ${data.brain} (${data.model})`);
            break;

        case 'thinking':
            markBrain(data.brain, 'active', '思考中');
            addBrainEvent(`${displayBrainName(data.brain)} 思考中`);
            removeSpinner();
            currentSpinnerEl = addSpinner(`${data.brain} 思考中...`);
            break;

        case 'text_delta':
            markBrain('main', 'active', '输出回复');
            removeSpinner();
            appendStreamingText(data.text);
            break;

        case 'thinking_delta':
            appendThinking(data.content);
            break;

        case 'tool_start':
            trackToolStart(data.brain, data.tool_name, data.input);
            addToolStart(data.brain, data.tool_name, data.input);
            break;

        case 'tool_done':
            trackToolDone(data.brain, data.tool_name, data.duration_ms, data.is_error);
            updateToolDone(data.tool_name, data.duration_ms, data.output_preview, data.is_error);
            break;

        case 'memory_injected':
            markBrain('memory', 'active', `召回 ${data.count} 条记忆`);
            setBrainLink('memory', 'main', `注入 ${data.count} 条记忆`);
            addBrainEvent(`记忆脑向主脑注入 ${data.count} 条记忆`);
            addMemoryIndicator(data.count, data.preview);
            break;

        case 'evaluation_start':
            markBrain('eval', 'active', '开始评估');
            addBrainEvent('评估脑开始评估');
            addSystemMessage('评估脑开始评估...');
            break;

        case 'evaluation_result':
            markBrain('eval', data.passed ? 'done' : 'error', data.feedback);
            addBrainEvent(`评估脑完成: ${data.passed ? '通过' : '未通过'}`);
            const icon = data.passed ? '[PASS]' : '[FAIL]';
            addSystemMessage(`评估结果 ${icon}: ${data.feedback}`);
            break;

        case 'evaluating':
            markBrain('eval', 'active', '评估中');
            removeSpinner();
            currentSpinnerEl = addSpinner('评估脑评估中...');
            break;

        case 'llm_retry':
            addSystemMessage(`LLM 重试 (${data.attempt}/${data.max_attempts}): ${data.error}`);
            break;

        case 'ask_user':
            showAskModal(data.question, data.options);
            break;

        case 'done':
            finishActiveBrains();
            finalizeStreaming();
            removeSpinner();
            isGenerating = false;
            setInputEnabled(true);
            updateSendButton();
            break;

        case 'session_list':
            renderSessionList(data.sessions);
            break;

        case 'session_switched':
            activeSessionId = data.session_id;
            renderMessages(data.messages);
            break;

        case 'session_messages_updated':
            if (data.session_id === activeSessionId) {
                renderMessages(data.messages);
            }
            break;

        case 'persona_list':
            renderPersonaList(data.personas, data.active);
            break;

        case 'persona_switched':
            addSystemMessage(`已切换人格: ${data.persona_id}`);
            break;

        case 'error':
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
    currentThinkingEl = null;
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

// ── Thinking (Typewriter) ───────────────────────────────────────
function appendThinking(content) {
    if (!showThinking) return;

    if (!currentThinkingEl) {
        currentThinkingEl = document.createElement('div');
        currentThinkingEl.className = 'thinking-block';
        $messages.appendChild(currentThinkingEl);
    }

    thinkingFullContent += content;

    if (!thinkingRaf) {
        thinkingRaf = requestAnimationFrame(typeThinkingFrame);
    }
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
function addToolStart(brain, toolName, input) {
    const item = document.createElement('div');
    item.className = 'tool-item';
    item.dataset.toolName = toolName;

    const header = document.createElement('div');
    header.className = 'tool-header';
    header.innerHTML = `
        <span class="tool-status" style="background:var(--warning)"></span>
        <span class="tool-name">${escapeHtml(brain)}/${escapeHtml(toolName)}</span>
        <span class="tool-duration">运行中...</span>
    `;

    const body = document.createElement('div');
    body.className = 'tool-body';
    body.textContent = input ? truncate(input, 500) : '';

    header.addEventListener('click', () => {
        item.classList.toggle('expanded');
    });

    item.appendChild(header);
    item.appendChild(body);

    if (!currentToolGroup) {
        currentToolGroup = document.createElement('div');
        currentToolGroup.className = 'tool-group';
        $messages.appendChild(currentToolGroup);
    }

    currentToolGroup.appendChild(item);
    scrollToBottom();
}

function updateToolDone(toolName, durationMs, outputPreview, isError) {
    // Find the matching tool item (may be in current group or elsewhere)
    const items = document.querySelectorAll('.tool-item');
    for (const item of items) {
        if (item.dataset.toolName === toolName) {
            const status = item.querySelector('.tool-status');
            const duration = item.querySelector('.tool-duration');
            const body = item.querySelector('.tool-body');

            status.style.background = isError ? 'var(--error)' : 'var(--success)';
            duration.textContent = `${durationMs}ms`;

            if (outputPreview) {
                const existing = body.textContent;
                body.textContent = existing
                    ? existing + '\n---\n' + truncate(outputPreview, 500)
                    : truncate(outputPreview, 500);
            }
            break;
        }
    }
    currentToolGroup = null;
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
        $sendBtn.innerHTML = '&#9632;'; // ■ 停止图标
        $sendBtn.title = '停止生成 (Esc / Ctrl+C)';
    } else {
        $sendBtn.classList.remove('stop-mode');
        $sendBtn.innerHTML = '&#10148;'; // ➤ 发送图标
        $sendBtn.title = '发送';
    }
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
    const value = String(brain || '').toLowerCase();
    if (value.includes('memory') || value.includes('记忆')) return 'memory';
    if (value.includes('eval') || value.includes('评估')) return 'eval';
    if (value.includes('novel') || value.includes('小说')) return 'novel';
    if (value.includes('explore')) return 'explore';
    if (value.includes('agent')) return 'agent';
    return 'main';
}

function displayBrainName(brain) {
    const key = normalizeBrainKey(brain);
    ensureBrainNode(key);
    return brainState.nodes[key].label;
}

function ensureBrainNode(key) {
    if (!brainState.nodes[key]) {
        const label = key === 'explore' ? '探索脑' : key === 'agent' ? '子代理' : key;
        brainState.nodes[key] = { label, status: 'idle', detail: '待命', active: false };
    }
}

function markBrain(brain, status, detail) {
    const key = normalizeBrainKey(brain);
    ensureBrainNode(key);
    brainState.nodes[key].status = status;
    brainState.nodes[key].detail = detail || '';
    brainState.nodes[key].active = status === 'active';
    renderCockpit();
}

function setBrainLink(from, to, label) {
    const source = normalizeBrainKey(from);
    const target = normalizeBrainKey(to);
    ensureBrainNode(source);
    ensureBrainNode(target);
    const existing = brainState.links.find((l) => l.from === source && l.to === target);
    if (existing) {
        existing.label = label;
        existing.active = true;
    } else {
        brainState.links.push({ from: source, to: target, label, active: true });
    }
    renderCockpit();
}

function addBrainEvent(text) {
    const stamp = new Date().toLocaleTimeString();
    brainState.events.unshift({ stamp, text });
    brainState.events = brainState.events.slice(0, 30);
    renderCockpit();
}

function finishActiveBrains() {
    Object.values(brainState.nodes).forEach((node) => {
        if (node.active) {
            node.status = 'done';
            node.active = false;
            node.detail = node.detail || '完成';
        }
    });
    brainState.links.forEach((link) => {
        link.active = false;
    });
    renderCockpit();
}

function trackToolStart(brain, toolName, input) {
    const owner = normalizeBrainKey(brain);
    markBrain(owner, 'active', `调用 ${toolName}`);
    addBrainEvent(`${displayBrainName(brain)} 调用工具 ${toolName}`);

    if (toolName === 'Agent') {
        const subagent = extractSubagentType(input);
        const target = normalizeBrainKey(subagent || 'agent');
        const label = target === 'novel' ? '发布小说任务' : '发布任务';
        setBrainLink(owner, target, label);
        markBrain(target, 'active', label);
        addBrainEvent(`${displayBrainName(brain)} -> ${displayBrainName(target)}: ${label}`);
    }
}

function trackToolDone(brain, toolName, durationMs, isError) {
    const owner = normalizeBrainKey(brain);
    markBrain(owner, isError ? 'error' : 'done', `${toolName} ${durationMs}ms`);
    addBrainEvent(`${displayBrainName(brain)} 完成工具 ${toolName}`);
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

    const activeNames = Object.values(brainState.nodes)
        .filter((node) => node.active)
        .map((node) => node.label);
    $cockpitSummary.textContent = activeNames.length > 0
        ? `${activeNames.join('、')} 正在工作`
        : '所有脑处于待命或已完成状态';

    $brainNodes.innerHTML = '';
    Object.entries(brainState.nodes).forEach(([key, node]) => {
        const card = document.createElement('div');
        card.className = `brain-node ${node.status}`;
        card.dataset.brain = key;
        card.innerHTML = `
            <div class="brain-node-title">${escapeHtml(node.label)}</div>
            <div class="brain-node-status">${escapeHtml(statusText(node.status))}</div>
            <div class="brain-node-detail">${escapeHtml(truncate(node.detail || '待命', 80))}</div>
        `;
        $brainNodes.appendChild(card);
    });

    $brainEventList.innerHTML = '';
    brainState.events.forEach((event) => {
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
    $brainLinks.setAttribute('viewBox', `0 0 ${mapRect.width} ${mapRect.height}`);
    $brainLinks.innerHTML = '';

    brainState.links.forEach((link) => {
        const fromEl = $brainNodes.querySelector(`[data-brain="${link.from}"]`);
        const toEl = $brainNodes.querySelector(`[data-brain="${link.to}"]`);
        if (!fromEl || !toEl) return;

        const fromRect = fromEl.getBoundingClientRect();
        const toRect = toEl.getBoundingClientRect();
        const x1 = fromRect.left + fromRect.width / 2 - mapRect.left;
        const y1 = fromRect.top + fromRect.height / 2 - mapRect.top;
        const x2 = toRect.left + toRect.width / 2 - mapRect.left;
        const y2 = toRect.top + toRect.height / 2 - mapRect.top;
        const mx = (x1 + x2) / 2;
        const my = (y1 + y2) / 2;

        const line = document.createElementNS('http://www.w3.org/2000/svg', 'line');
        line.setAttribute('x1', x1);
        line.setAttribute('y1', y1);
        line.setAttribute('x2', x2);
        line.setAttribute('y2', y2);
        line.setAttribute('class', link.active ? 'brain-link active' : 'brain-link');
        $brainLinks.appendChild(line);

        const label = document.createElementNS('http://www.w3.org/2000/svg', 'text');
        label.setAttribute('x', mx);
        label.setAttribute('y', my - 6);
        label.setAttribute('class', 'brain-link-label');
        label.textContent = link.label;
        $brainLinks.appendChild(label);
    });
}

function statusText(status) {
    switch (status) {
        case 'active': return '工作中';
        case 'done': return '完成';
        case 'error': return '异常';
        default: return '待命';
    }
}

// ── Session List ────────────────────────────────────────────────
function renderSessionList(sessions) {
    $sessionList.innerHTML = '';
    sessions.forEach((s) => {
        const el = document.createElement('div');
        el.className = 'session-item' + (s.id === activeSessionId ? ' active' : '');
        el.innerHTML = `
            <div class="session-title">${escapeHtml(s.title)}</div>
            <div class="session-meta">${s.message_count} 条消息</div>
        `;
        el.addEventListener('click', () => {
            send('switch_session', { session_id: s.id });
        });
        $sessionList.appendChild(el);
    });
}

function renderMessages(messages) {
    $messages.innerHTML = '';
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
    send('query', { input: text });
    $input.value = '';
    $input.style.height = 'auto';
    isGenerating = true;
    setInputEnabled(true); // 不禁用输入框，只切换按钮状态
    updateSendButton();

    // Reset streaming state
    currentStreamingEl = null;
    currentThinkingEl = null;
    currentToolGroup = null;
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
    $thinkingToggle.style.background = showThinking ? 'var(--accent)' : '';
    $thinkingToggle.style.color = showThinking ? '#fff' : '';
    document.querySelectorAll('.thinking-block').forEach((el) => {
        el.style.display = showThinking ? '' : 'none';
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
$input.focus();
