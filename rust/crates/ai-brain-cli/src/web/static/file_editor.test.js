const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

class ClassList {
    constructor(...names) {
        this.names = new Set(names);
    }

    add(...names) {
        names.forEach((name) => this.names.add(name));
    }

    remove(...names) {
        names.forEach((name) => this.names.delete(name));
    }

    contains(name) {
        return this.names.has(name);
    }

    toggle(name, force) {
        const enabled = force === undefined ? !this.contains(name) : force;
        if (enabled) this.add(name);
        else this.remove(name);
        return enabled;
    }
}

function element(...classes) {
    return {
        classList: new ClassList(...classes),
        dataset: {},
        style: {},
        innerHTML: '',
        textContent: '',
        title: '',
        value: '',
        disabled: false,
        selectionStart: 0,
        selectionEnd: 0,
        scrollTop: 0,
        scrollLeft: 0,
        isConnected: true,
        focus() {},
        setAttribute() {},
        setSelectionRange(start, end) {
            this.selectionStart = start;
            this.selectionEnd = end;
        },
    };
}

function response(status, body) {
    return {
        ok: status >= 200 && status < 300,
        status,
        async json() {
            return structuredClone(body);
        },
    };
}

function createServer(options = {}) {
    let revisionNumber = 1;
    let saveDelayMs = 0;
    let saveAckDelayMs = 0;
    const disk = {
        name: options.name || 'demo.js',
        path: options.path || '/workspace/demo.js',
        content: options.content || 'const value = 1;\n',
        revision: `revision-${revisionNumber}`,
    };
    return {
        disk,
        setSaveDelay(delayMs) {
            saveDelayMs = delayMs;
        },
        setSaveAckDelay(delayMs) {
            saveAckDelayMs = delayMs;
        },
        externalUpdate(content) {
            revisionNumber += 1;
            disk.content = content;
            disk.revision = `revision-${revisionNumber}`;
        },
        async fetch(url, options = {}) {
            if (!options.method) return response(200, disk);
            const request = JSON.parse(options.body);
            if (saveDelayMs) {
                await new Promise((resolve) => setTimeout(resolve, saveDelayMs));
            }
            if (request.expected_revision !== disk.revision) {
                return response(409, {
                    error: 'conflict',
                    ...disk,
                });
            }
            revisionNumber += 1;
            disk.content = request.content;
            disk.revision = `revision-${revisionNumber}`;
            if (saveAckDelayMs) {
                await new Promise((resolve) => setTimeout(resolve, saveAckDelayMs));
            }
            return response(200, {
                saved: true,
                name: disk.name,
                path: disk.path,
                revision: disk.revision,
            });
        },
    };
}

function loadProductionEditor(server) {
    const appPath = path.join(__dirname, 'app.js');
    const appSource = fs.readFileSync(appPath, 'utf8');
    const start = appSource.indexOf('function localFileName(path)');
    const end = appSource.indexOf('function quoteFileEditorSelection()');
    assert.ok(start >= 0 && end > start, 'production editor function block should be extractable');
    const editorSource = appSource.slice(start, end);
    const elements = {
        $markdownPreview: element('open'),
        $previewBackdrop: element('hidden'),
        $previewContent: element(),
        $previewSelectionTools: element(),
        $previewTitle: element(),
        $previewSource: element(),
        $fileEditorToolbar: element('hidden'),
        $fileEditorModeSwitch: element('hidden'),
        $fileEditorPreviewMode: element(),
        $fileEditorEditMode: element(),
        $fileEditorStatus: element(),
        $fileEditorQuote: element(),
        $fileEditorContent: element(),
        $fileEditorRefresh: element('hidden'),
        $fileEditorCopyPath: element('hidden'),
        $fileEditorTabs: element('hidden'),
        $fileEditorTab: element(),
        $fileEditorTabName: element(),
        $fileEditorSaveState: element(),
        $fileEditorConflict: element('hidden'),
        $fileEditorFrame: element('hidden'),
        $fileEditorLineNumbers: element(),
        $fileEditorStatusbar: element('hidden'),
        $fileEditorRevision: element(),
        $fileEditorCaret: element(),
        $fileEditorLines: element(),
        $fileEditorLineEnding: element(),
        $fileEditorLanguage: element(),
    };
    const context = vm.createContext({
        ...elements,
        console,
        Date,
        Map,
        Array,
        String,
        Promise,
        URLSearchParams,
        encodeURIComponent,
        setTimeout,
        clearTimeout,
        setInterval: () => 1,
        clearInterval: () => {},
        fetch: server.fetch.bind(server),
        navigator: {},
        marked: {
            lexer(text) {
                const tokens = [];
                const source = String(text || '');
                if (/^#{1,6}\s+\S/mu.test(source)) tokens.push({ type: 'heading' });
                if (/^\s*(?:[-*+]\s+|\d+[.)]\s+)/mu.test(source)) {
                    tokens.push({ type: 'list' });
                }
                if (tokens.length === 0 && source.trim()) tokens.push({ type: 'paragraph' });
                return tokens;
            },
            parse(text) {
                return `<article>${text}</article><script>unsafe()</script>`;
            },
        },
        DOMPurify: {
            sanitize(html) {
                return html.replace(/<script>[\s\S]*?<\/script>/gu, '');
            },
        },
        document: {
            body: element(),
            querySelectorAll: () => [],
            createElement: () => element(),
            execCommand: () => true,
        },
        refreshIcons() {},
        addSystemMessage(message) {
            throw new Error(message);
        },
        showToast() {},
        buildPreviewMetadata(title, source, content, metadata) {
            return { title, source, content, ...metadata };
        },
    });
    const statePrelude = `
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
    `;
    const testApi = `
        globalThis.editorApi = {
            open: (file) => openLocalFileEditor(file),
            check: (options) => checkLatestLocalFile(options),
            save: (options) => saveOpenedLocalFile(options),
            flush: () => flushOpenedLocalFile(),
            loadDisk: () => loadConflictingDiskVersion(),
            keepLocal: () => keepAndSaveLocalFile(),
            reset: () => resetLocalFileEditorSession(),
            setMode: (mode) => setLocalFileEditorMode(mode),
            input: (content, schedule = true) => {
                $fileEditorContent.value = content;
                $fileEditorContent.selectionStart = content.length;
                $fileEditorContent.selectionEnd = content.length;
                setFileEditorDirty(true);
                setFileEditorSaveState('dirty', '有未保存修改');
                updateFileEditorMetrics();
                if (schedule) scheduleLocalFileSave();
            },
            state: () => ({
                path: openedLocalFile?.path || null,
                revision: openedLocalFile?.revision || null,
                content: $fileEditorContent.value,
                dirty: fileEditorDirty,
                conflictRevision: fileEditorConflictSnapshot?.revision || null,
                saveState: $fileEditorSaveState.dataset.state,
                mode: fileEditorMode,
                previewHidden: $previewContent.classList.contains('hidden'),
                editorHidden: $fileEditorFrame.classList.contains('hidden'),
                previewHtml: $previewContent.innerHTML,
            }),
        };
    `;
    vm.runInContext(`${statePrelude}\n${editorSource}\n${testApi}`, context);
    return context.editorApi;
}

test('production file editor preserves autosave and external-change invariants', async () => {
    const server = createServer();
    const editor = loadProductionEditor(server);
    await editor.open({ path: server.disk.path, change_kind: 'modified' });
    assert.deepEqual({ ...editor.state() }, {
        path: server.disk.path,
        revision: 'revision-1',
        content: 'const value = 1;\n',
        dirty: false,
        conflictRevision: null,
        saveState: 'saved',
        mode: 'edit',
        previewHidden: true,
        editorHidden: false,
        previewHtml: '',
    });

    editor.input('const value = 2;\n');
    await new Promise((resolve) => setTimeout(resolve, 760));
    assert.equal(server.disk.content, 'const value = 2;\n');
    assert.equal(editor.state().dirty, false);

    server.externalUpdate('const value = 3;\n');
    await editor.check();
    assert.equal(editor.state().content, 'const value = 3;\n');
    assert.equal(editor.state().dirty, false);

    editor.input('const local = 4;\n', false);
    server.externalUpdate('const external = 5;\n');
    await editor.check();
    assert.equal(editor.state().content, 'const local = 4;\n');
    assert.equal(editor.state().conflictRevision, server.disk.revision);
    editor.loadDisk();
    assert.equal(editor.state().content, 'const external = 5;\n');
    assert.equal(editor.state().dirty, false);

    editor.input('const keep = 6;\n', false);
    server.externalUpdate('const external = 7;\n');
    await editor.check();
    await editor.keepLocal();
    assert.equal(server.disk.content, 'const keep = 6;\n');
    assert.equal(editor.state().dirty, false);

    server.setSaveDelay(60);
    editor.input('const first = 8;\n', false);
    const firstSave = editor.save();
    await new Promise((resolve) => setTimeout(resolve, 10));
    editor.input('const second = 9;\n', false);
    await firstSave;
    assert.equal(editor.state().dirty, true);
    assert.equal(await editor.flush(), true);
    assert.equal(server.disk.content, 'const second = 9;\n');
    assert.equal(editor.state().dirty, false);

    server.setSaveDelay(0);
    server.setSaveAckDelay(60);
    editor.input('const ownWrite = 10;\n', false);
    const saveWithDelayedAck = editor.save();
    await new Promise((resolve) => setTimeout(resolve, 10));
    await editor.check();
    assert.equal(editor.state().conflictRevision, null);
    await saveWithDelayedAck;
    assert.equal(server.disk.content, 'const ownWrite = 10;\n');
    assert.equal(editor.state().dirty, false);
    editor.reset();
});

test('production markdown file defaults to a sanitized preview and can switch to editing', async () => {
    const server = createServer({
        name: 'guide.md',
        path: '/workspace/guide.md',
        content: '# Guide\n\n<script>unsafe()</script>\n',
    });
    const editor = loadProductionEditor(server);

    await editor.open({ path: server.disk.path, name: server.disk.name, change_kind: 'modified' });

    assert.equal(editor.state().mode, 'preview');
    assert.equal(editor.state().previewHidden, false);
    assert.equal(editor.state().editorHidden, true);
    assert.match(editor.state().previewHtml, /<article># Guide/u);
    assert.doesNotMatch(editor.state().previewHtml, /<script>/u);

    editor.setMode('edit');
    assert.equal(editor.state().mode, 'edit');
    assert.equal(editor.state().previewHidden, true);
    assert.equal(editor.state().editorHidden, false);
    assert.equal(editor.state().content, server.disk.content);
});

test('extensionless files preview only when marked finds markdown structure', async () => {
    const markdownServer = createServer({
        name: '章节细纲',
        path: '/workspace/章节细纲',
        content: '# 第一章\n\n- 冲突\n',
    });
    const markdown = loadProductionEditor(markdownServer);
    await markdown.open({ path: markdownServer.disk.path, name: markdownServer.disk.name });
    assert.equal(markdown.state().mode, 'preview');

    const plainServer = createServer({
        name: 'README',
        path: '/workspace/README',
        content: '这是一段没有 Markdown 结构的普通文本。\n',
    });
    const plain = loadProductionEditor(plainServer);
    await plain.open({ path: plainServer.disk.path, name: plainServer.disk.name });
    assert.equal(plain.state().mode, 'edit');

    const explicitTextServer = createServer({
        name: 'notes.txt',
        path: '/workspace/notes.txt',
        content: '# 仍然按文本打开\n',
    });
    const explicitText = loadProductionEditor(explicitTextServer);
    await explicitText.open({
        path: explicitTextServer.disk.path,
        name: explicitTextServer.disk.name,
    });
    assert.equal(explicitText.state().mode, 'edit');
});

test('file editor assets expose markdown mode controls and load the sanitizer before app code', () => {
    const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');

    assert.match(html, /id="file-editor-preview-mode"/u);
    assert.match(html, /id="file-editor-edit-mode"/u);
    assert.match(html, /dompurify@3\.4\.13\/dist\/purify\.min\.js/u);
    assert.ok(
        html.indexOf('purify.min.js') < html.indexOf('/app.js'),
        'DOMPurify must load before the application script',
    );
});
