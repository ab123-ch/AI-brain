const test = require('node:test');
const assert = require('node:assert/strict');

const {
    appendMention,
    recipientMemberIds,
    removeMention,
} = require('./mentions.js');

const members = [
    { member_id: 'member-a', display_name: '智脑 A', availability: 'active' },
    { member_id: 'member-b', display_name: '智脑 B', availability: 'active' },
    { member_id: 'member-c', display_name: '智脑 C', availability: 'sleeping' },
];

test('文本 @ 只选择明确提及的活动成员且按成员 ID 去重', () => {
    const recipients = recipientMemberIds('请 @智脑 A 和 @智脑 B，再问一次 @智脑 A。', members);
    assert.deepEqual([...recipients].sort(), ['member-a', 'member-b']);
});

test('选择器写入和取消对应的 @，不会重复插入', () => {
    const inserted = appendMention('请分析这个方案', '智脑 A');
    assert.equal(inserted, '请分析这个方案 @智脑 A ');
    assert.equal(appendMention(inserted, '智脑 A'), inserted);
    assert.equal(removeMention(inserted, '智脑 A').trim(), '请分析这个方案');
});

test('休眠成员和名称前缀不会被文本 @ 误选中', () => {
    const recipients = recipientMemberIds('@智脑 C @智脑 AB', members);
    assert.deepEqual([...recipients], []);
});
