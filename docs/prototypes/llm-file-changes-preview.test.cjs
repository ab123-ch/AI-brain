const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const prototypePath = path.join(__dirname, 'llm-file-changes-preview.html');
const html = fs.readFileSync(prototypePath, 'utf8');
const inlineScript = [...html.matchAll(/<script(?:\s[^>]*)?>([\s\S]*?)<\/script>/g)]
    .map((match) => match[1])
    .find((source) => source.includes("const projectRoot ="));

assert.ok(inlineScript, 'prototype inline script should exist');

class FakeClassList {
    constructor(classes = '') {
        this.values = new Set(classes.split(/\s+/).filter(Boolean));
    }

    add(...classes) {
        classes.forEach((className) => this.values.add(className));
    }

    remove(...classes) {
        classes.forEach((className) => this.values.delete(className));
    }

    contains(className) {
        return this.values.has(className);
    }

    toggle(className, force) {
        const enabled = force === undefined ? !this.contains(className) : Boolean(force);
        if (enabled) this.add(className);
        else this.remove(className);
        return enabled;
    }
}

class FakeEvent {
    constructor(type, options = {}) {
        this.type = type;
        Object.assign(this, options);
    }

    preventDefault() {
        this.defaultPrevented = true;
    }
}

let fakeDocument = null;

class FakeElement {
    constructor(id = '', classes = '') {
        this.id = id;
        this.classList = new FakeClassList(classes);
        this.dataset = {};
        this.style = {};
        this.listeners = new Map();
        this.attributes = new Map();
        this.textContent = '';
        this.value = '';
        this.disabled = false;
        this.selectionStart = 0;
        this.selectionEnd = 0;
        this.scrollTop = 0;
        this.scrollLeft = 0;
    }

    addEventListener(type, listener) {
        if (!this.listeners.has(type)) this.listeners.set(type, []);
        this.listeners.get(type).push(listener);
    }

    dispatchEvent(event) {
        for (const listener of this.listeners.get(event.type) || []) listener(event);
        return !event.defaultPrevented;
    }

    setAttribute(name, value) {
        this.attributes.set(name, String(value));
    }

    focus() {
        fakeDocument.activeElement = this;
    }

    select() {
        this.selectionStart = 0;
        this.selectionEnd = this.value.length;
    }

    setSelectionRange(start, end) {
        this.selectionStart = start;
        this.selectionEnd = end;
    }

    setRangeText(replacement, start, end) {
        this.value = `${this.value.slice(0, start)}${replacement}${this.value.slice(end)}`;
        const caret = start + replacement.length;
        this.setSelectionRange(caret, caret);
    }

    remove() {}
}

const elements = new Map();
for (const match of html.matchAll(/<[^>]+\bid="([^"]+)"[^>]*>/g)) {
    const tag = match[0];
    const id = match[1];
    const classes = tag.match(/\bclass="([^"]*)"/)?.[1] || '';
    elements.set(id, new FakeElement(id, classes));
}

const fileBubbles = [...html.matchAll(/<button class="file-bubble"[^>]*data-file-id="([^"]+)"[^>]*>/g)]
    .map((match) => {
        const element = new FakeElement('', 'file-bubble');
        element.dataset.fileId = match[1];
        return element;
    });

const documentListeners = new Map();
fakeDocument = {
    activeElement: null,
    visibilityState: 'visible',
    body: { appendChild() {} },
    getElementById(id) {
        return elements.get(id) || null;
    },
    querySelectorAll(selector) {
        return selector === '.file-bubble' ? fileBubbles : [];
    },
    createElement() {
        return new FakeElement();
    },
    execCommand() {
        return true;
    },
    addEventListener(type, listener) {
        if (!documentListeners.has(type)) documentListeners.set(type, []);
        documentListeners.get(type).push(listener);
    },
};

const windowListeners = new Map();
const context = {
    Array,
    Date,
    Event: FakeEvent,
    Map,
    Math,
    Promise,
    Set,
    String,
    clearInterval,
    clearTimeout,
    console,
    document: fakeDocument,
    navigator: { clipboard: { writeText: async () => {} } },
    setInterval,
    setTimeout,
};
context.window = context;
context.globalThis = context;
context.addEventListener = (type, listener) => {
    if (!windowListeners.has(type)) windowListeners.set(type, []);
    windowListeners.get(type).push(listener);
};

vm.createContext(context);
vm.runInContext(inlineScript, context, { filename: prototypePath });

const editor = context.prototypeFileEditor;
assert.ok(editor, 'prototype editor test API should exist');

const wait = (duration) => new Promise((resolve) => setTimeout(resolve, duration));

async function run() {
    await editor.openFile('app');
    assert.deepEqual(
        { ...editor.getState() },
        {
            fileId: 'app',
            dirty: false,
            state: 'saved',
            acknowledgedRevision: 1,
            diskRevision: 1,
        },
        'open should load the latest clean revision',
    );

    editor.edit(`${editor.content()}\n// autosave local edit`);
    assert.equal(editor.getState().state, 'dirty');
    await wait(1150);
    assert.equal(editor.getState().dirty, false, 'debounced autosave should clear dirty state');
    assert.equal(editor.getState().acknowledgedRevision, 2);

    await editor.simulateDiskUpdate();
    assert.equal(editor.getState().state, 'saved');
    assert.equal(editor.getState().acknowledgedRevision, 3);
    assert.match(editor.content(), /^\/\/ 磁盘外部更新/u, 'clean editor should auto-reload disk');

    const localDraft = `${editor.content()}\n// keep this local draft`;
    editor.edit(localDraft);
    await editor.simulateDiskUpdate();
    assert.equal(editor.getState().state, 'conflict');
    assert.equal(editor.getState().dirty, true);
    assert.equal(editor.content(), localDraft, 'conflict must not replace local content');

    editor.loadDiskVersion();
    assert.equal(editor.getState().dirty, false);
    assert.equal(editor.getState().acknowledgedRevision, 4);
    assert.notEqual(editor.content(), localDraft, 'load-disk choice should accept disk content');

    editor.edit(`${editor.content()}\n// explicitly keep local`);
    await editor.simulateDiskUpdate();
    assert.equal(editor.getState().state, 'conflict');
    await editor.keepAndSaveLocalVersion();
    assert.equal(editor.getState().state, 'saved');
    assert.equal(editor.getState().dirty, false);
    assert.match(editor.content(), /explicitly keep local/u);

    editor.edit(`${editor.content()}\n// first in-flight edit`);
    const flushing = editor.flush();
    await wait(40);
    editor.edit(`${editor.content()}\n// second in-flight edit`);
    assert.equal(await flushing, true);
    assert.equal(editor.getState().dirty, false, 'flush should include edits made during first save');
    await editor.close();

    await editor.openFile('app');
    assert.match(editor.content(), /second in-flight edit/u, 'reopen should load the fully flushed content');
    await editor.close();
    console.log('editable file preview prototype: all state transitions passed');
}

run()
    .then(() => process.exit(0))
    .catch((error) => {
        console.error(error);
        process.exit(1);
    });
