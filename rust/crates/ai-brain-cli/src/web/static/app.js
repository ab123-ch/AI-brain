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
            removeSpinner();
            currentSpinnerEl = addSystemMessage(`连接 ${data.brain} (${data.model})`);
            break;

        case 'thinking':
            removeSpinner();
            currentSpinnerEl = addSpinner(`${data.brain} 思考中...`);
            break;

        case 'text_delta':
            removeSpinner();
            appendStreamingText(data.text);
            break;

        case 'thinking_delta':
            appendThinking(data.content);
            break;

        case 'tool_start':
            addToolStart(data.brain, data.tool_name, data.input);
            break;

        case 'tool_done':
            updateToolDone(data.tool_name, data.duration_ms, data.output_preview, data.is_error);
            break;

        case 'memory_injected':
            addMemoryIndicator(data.count, data.preview);
            break;

        case 'evaluation_start':
            addSystemMessage('评估脑开始评估...');
            break;

        case 'evaluation_result':
            const icon = data.passed ? '[PASS]' : '[FAIL]';
            addSystemMessage(`评估结果 ${icon}: ${data.feedback}`);
            break;

        case 'evaluating':
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
            finalizeStreaming();
            removeSpinner();
            setInputEnabled(true);
            break;

        case 'session_list':
            renderSessionList(data.sessions);
            break;

        case 'session_switched':
            activeSessionId = data.session_id;
            renderMessages(data.messages);
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
            setInputEnabled(true);
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

function addUserMessage(text) {
    const el = document.createElement('div');
    el.className = 'msg user';
    el.textContent = text;
    $messages.appendChild(el);
    scrollToBottom();
}

function setInputEnabled(enabled) {
    $input.disabled = !enabled;
    $sendBtn.disabled = !enabled;
    if (enabled) {
        $input.focus();
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
    messages.forEach((m) => {
        if (m.role === 'user') {
            addUserMessage(m.content);
        } else if (m.role === 'assistant') {
            const el = document.createElement('div');
            el.className = 'msg assistant';
            renderMarkdown(el, m.content);
            $messages.appendChild(el);
        } else {
            addSystemMessage(m.content);
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

// ── Submit Query ────────────────────────────────────────────────
function submitQuery() {
    const text = $input.value.trim();
    if (!text) return;

    addUserMessage(text);
    send('query', { input: text });
    $input.value = '';
    $input.style.height = 'auto';
    setInputEnabled(false);

    // Reset streaming state
    currentStreamingEl = null;
    currentThinkingEl = null;
    currentToolGroup = null;
    resetThinkingState();
}

// ── Event Bindings ──────────────────────────────────────────────
$input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        submitQuery();
    }
});

// Auto-resize textarea
$input.addEventListener('input', () => {
    $input.style.height = 'auto';
    $input.style.height = Math.min($input.scrollHeight, 120) + 'px';
});

$sendBtn.addEventListener('click', submitQuery);

$personaSelect.addEventListener('change', () => {
    send('switch_persona', { persona_id: $personaSelect.value });
});

$newSessionBtn.addEventListener('click', () => {
    send('new_session');
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

// ── Init ────────────────────────────────────────────────────────
connect();
$input.focus();
