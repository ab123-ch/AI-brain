const test = require('node:test');
const assert = require('node:assert/strict');

const {
    beginReply,
    canReplyToEvent,
    mergeEventsBySequence,
} = require('./room_reply.js');

test('只有带事件 ID 的用户或成员消息可以回复', () => {
    assert.equal(canReplyToEvent({
        event_id: 'u1',
        kind: 'user_message',
        sender_kind: 'user',
    }), true);
    assert.equal(canReplyToEvent({
        event_id: 'e1',
        kind: 'member_message',
        sender_kind: 'member',
    }), true);

    [
        null,
        {},
        { event_id: '', kind: 'member_message', sender_kind: 'member' },
        { event_id: 'e2', kind: 'system_message', sender_kind: 'member' },
        { event_id: 'e3', kind: 'member_message', sender_kind: 'system' },
    ].forEach((event) => assert.equal(canReplyToEvent(event), false));
});

test('回复活动实例消息会返回自动收件人', () => {
    const result = beginReply(
        {
            event_id: 'e1',
            sender_kind: 'member',
            sender_id: 'a',
            sender_name: '智脑 A',
            kind: 'member_message',
            content: '答案',
        },
        [{ member_id: 'a', display_name: '智脑 A', availability: 'active' }],
    );

    assert.deepEqual(result.reply, {
        event_id: 'e1',
        sender_name: '智脑 A',
        sender_kind: 'member',
        content: '答案',
    });
    assert.equal(result.auto_recipient_id, 'a');
    assert.equal(result.warning, null);
});

test('回复用户消息不自动选择实例', () => {
    const result = beginReply(
        {
            event_id: 'u1',
            sender_kind: 'user',
            sender_id: 'user',
            sender_name: '用户',
            kind: 'user_message',
            content: '问题',
        },
        [{ member_id: 'user', display_name: '同名实例', availability: 'active' }],
    );

    assert.equal(result.auto_recipient_id, null);
    assert.equal(result.warning, null);
});

test('休眠或归档实例不会被自动唤醒并返回不可用提示', () => {
    for (const availability of ['sleeping', 'archived']) {
        const result = beginReply(
            {
                event_id: `e-${availability}`,
                sender_kind: 'member',
                sender_id: 'a',
                sender_name: '智脑 A',
                kind: 'member_message',
                content: '答案',
            },
            [{ member_id: 'a', display_name: '智脑 A', availability }],
        );

        assert.equal(result.auto_recipient_id, null);
        assert.match(result.warning, /智脑 A.*不可用.*不会自动唤醒/);
    }
});

test('回复已不在成员列表中的实例时不会自动唤醒并返回提示', () => {
    const result = beginReply(
        {
            event_id: 'e-missing',
            sender_kind: 'member',
            sender_id: 'missing',
            sender_name: '旧智脑',
            kind: 'member_message',
            content: '历史消息',
        },
        [],
    );

    assert.equal(result.auto_recipient_id, null);
    assert.match(result.warning, /旧智脑.*不在.*成员列表.*不会自动唤醒/);
});

test('不能回复的事件会被拒绝', () => {
    assert.throws(
        () => beginReply({ event_id: 'e1', kind: 'system_message', sender_kind: 'system' }, []),
        /该消息不能被回复/,
    );
});

test('分页合并按事件 ID 去重、以新页为准并保持 sequence 升序', () => {
    const merged = mergeEventsBySequence(
        [
            { event_id: 'e3', sequence: 3, content: 'stale' },
            { event_id: 'e4', sequence: 4 },
        ],
        [
            { event_id: 'e1', sequence: 1 },
            { event_id: 'e3', sequence: 3, content: 'authoritative' },
        ],
    );

    assert.deepEqual(merged.map((event) => event.event_id), ['e1', 'e3', 'e4']);
    assert.equal(merged[1].content, 'authoritative');
});

test('分页合并不会突变输入数组或事件', () => {
    const existing = [
        { event_id: 'e3', sequence: 3, content: 'existing' },
        { event_id: 'e4', sequence: 4 },
    ];
    const incoming = [
        { event_id: 'e1', sequence: 1 },
        { event_id: 'e3', sequence: 3, content: 'incoming' },
    ];
    const existingBefore = structuredClone(existing);
    const incomingBefore = structuredClone(incoming);

    mergeEventsBySequence(existing, incoming);

    assert.deepEqual(existing, existingBefore);
    assert.deepEqual(incoming, incomingBefore);
});
