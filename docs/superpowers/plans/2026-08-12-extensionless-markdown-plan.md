# Extensionless Markdown Preview Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让无后缀 Markdown 内容默认进入现有安全预览，同时让普通无后缀文本和有明确非 Markdown 后缀文件继续进入编辑模式。

**Architecture:** 文件名只负责区分明确 Markdown、明确非 Markdown和无后缀三类；无后缀文件在快照正文加载后用现有 `marked.lexer` 做保守结构识别。识别结果保存在当前编辑会话状态中，现有 DOMPurify 渲染、模式切换、自动保存和冲突逻辑保持不变。

**Tech Stack:** 浏览器 JavaScript、Node.js `node:test`、marked、DOMPurify、现有静态 HTML/CSS。

---

### Task 1: 内容感知 Markdown 能力

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/static/app.js`
- Modify: `rust/crates/ai-brain-cli/src/web/static/file_editor.test.js`
- Preserve existing changes: `rust/crates/ai-brain-cli/src/web/static/index.html`
- Preserve existing changes: `rust/crates/ai-brain-cli/src/web/static/style.css`

- [ ] **Step 1: 写关键失败测试**

在现有生产编辑器测试中给 marked stub 增加 `lexer`，并用一个测试覆盖三种关键输入：

```javascript
test('extensionless files preview only when marked finds markdown structure', async () => {
    const markdown = loadProductionEditor(createServer({
        name: '章节细纲',
        path: '/workspace/章节细纲',
        content: '# 第一章\n\n- 冲突\n',
    }));
    await markdown.open({ path: '/workspace/章节细纲', name: '章节细纲' });
    assert.equal(markdown.state().mode, 'preview');

    const plain = loadProductionEditor(createServer({
        name: 'README',
        path: '/workspace/README',
        content: '这是一段没有 Markdown 结构的普通文本。\n',
    }));
    await plain.open({ path: '/workspace/README', name: 'README' });
    assert.equal(plain.state().mode, 'edit');

    const explicitText = loadProductionEditor(createServer({
        name: 'notes.txt',
        path: '/workspace/notes.txt',
        content: '# 仍然按文本打开\n',
    }));
    await explicitText.open({ path: '/workspace/notes.txt', name: 'notes.txt' });
    assert.equal(explicitText.state().mode, 'edit');
});
```

- [ ] **Step 2: 运行测试并确认 RED**

Run: `node --test rust/crates/ai-brain-cli/src/web/static/file_editor.test.js`

Expected: 新测试失败，因为 `localFileIsMarkdown` 只识别 `.md/.markdown`，无后缀结构化正文仍为 `edit`。

- [ ] **Step 3: 最小实现正文识别**

在 `app.js` 增加会话级 `fileEditorSupportsMarkdown`，并把判断拆为：

```javascript
function localFileExtensionKind(file) {
    const candidate = String(file?.name || file?.path || '').trim();
    if (/\.(?:md|markdown)$/iu.test(candidate)) return 'markdown';
    return /(?:^|[/\\])[^./\\]+$/u.test(candidate) ? 'extensionless' : 'other';
}

function extensionlessContentIsMarkdown(content) {
    if (typeof marked?.lexer !== 'function') return false;
    try {
        return marked.lexer(String(content || '')).some((token) =>
            !['space', 'paragraph', 'text'].includes(token.type));
    } catch (_error) {
        return false;
    }
}
```

`applyLocalFileSnapshot` 在正文写入后计算能力：明确 Markdown 为 true；无后缀按 lexer；其他为 false。打开时只有明确 Markdown 预设 preview，无后缀待快照应用后再切到 preview。`setLocalFileEditorMode` 只依赖会话能力，reset 时清回 false。

- [ ] **Step 4: 运行定向与静态测试确认 GREEN**

Run:

```powershell
node --test rust/crates/ai-brain-cli/src/web/static/file_editor.test.js
node --test rust/crates/ai-brain-cli/src/web/static/*.test.js
node --check rust/crates/ai-brain-cli/src/web/static/app.js
```

Expected: 全部 exit 0；现有 Markdown 清洗、编辑切换、自动保存和冲突测试保持通过。

- [ ] **Step 5: 提交文件预览变更**

仅暂存四个文件并核对 staged diff：

```powershell
git add -- rust/crates/ai-brain-cli/src/web/static/app.js rust/crates/ai-brain-cli/src/web/static/file_editor.test.js rust/crates/ai-brain-cli/src/web/static/index.html rust/crates/ai-brain-cli/src/web/static/style.css
git diff --cached --check
git commit -m "feat(web): preview extensionless markdown files"
```

