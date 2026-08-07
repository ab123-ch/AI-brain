const test = require('node:test');
const assert = require('node:assert/strict');

const {
    compactLabel,
    optionText,
    optionTextOrFallback,
} = require('./model_catalog.js');

test('模型目录文案优先展示配置标签', () => {
    const policy = {
        policy_id: 'gemini-2-5-flash',
        label: 'Gemini 2.5 Flash',
        provider: 'gemini',
        model: 'gemini-2.5-flash',
    };

    assert.equal(compactLabel(policy), 'Gemini 2.5 Flash');
});

test('缺少标签时回退到模型标识', () => {
    const policy = {
        policy_id: 'deepseek-v4-pro',
        provider: 'deepseek',
        model: 'deepseek-v4-pro',
    };

    assert.equal(compactLabel(policy), 'deepseek-v4-pro');
});

test('下拉选项文案拼接标签、提供商和模型', () => {
    const policy = {
        policy_id: 'gemini-2-5-pro',
        label: 'Gemini 2.5 Pro',
        provider: 'gemini',
        model: 'gemini-2.5-pro',
    };

    assert.equal(optionText(policy), 'Gemini 2.5 Pro · gemini · gemini-2.5-pro');
});

test('下拉缺少标签时不重复展示同一个模型', () => {
    const policy = {
        policy_id: 'deepseek-v4-pro',
        provider: 'deepseek',
        model: 'deepseek-v4-pro',
    };

    assert.equal(optionText(policy), 'deepseek-v4-pro · deepseek');
});

test('旧快照缺少模型详情时使用策略 ID', () => {
    const legacyPolicy = { policy_id: 'main' };

    assert.equal(compactLabel(legacyPolicy), 'main');
    assert.equal(optionText(legacyPolicy), 'main');
    assert.equal(optionTextOrFallback(null, 'main'), 'main');
});
