const test = require('node:test');
const assert = require('node:assert/strict');

const {
    appendRoomEvent,
    beginRoomOperation,
    beginReply,
    buildRoomOperationError,
    buildRoomPostPayload,
    captureTimelineViewport,
    canReplyToEvent,
    matchesPendingRoomPost,
    mergeVersionedEntity,
    mergeEventsBySequence,
    mergeSnapshotWindow,
    isTimelineNearBottom,
    reconcileReplyState,
    resetRoomUiState,
    roomOperationControls,
    shouldHandleRoomError,
    shouldFocusComposer,
    settleRoomOperation,
    shouldApplyRoomSnapshot,
    timelineScrollTarget,
    resolveHasEarlierEvents,
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

test('实时房间事件仅在新 ID 到达时标记为追加', () => {
    const existing = [{ event_id: 'e1', sequence: 1, content: '权威内容' }];
    const duplicate = appendRoomEvent(
        existing,
        { event_id: 'e1', sequence: 1, content: '迟到旧内容' },
        3,
    );
    const invalidated = appendRoomEvent(existing, { event_id: 'e2', sequence: 2 }, 3);
    const appended = appendRoomEvent(existing, { event_id: 'e4', sequence: 4 }, 3);

    assert.equal(duplicate.appended, false);
    assert.equal(duplicate.events, existing);
    assert.equal(duplicate.events[0].content, '权威内容');
    assert.equal(invalidated.appended, false);
    assert.equal(invalidated.events, existing);
    assert.equal(appended.appended, true);
    assert.deepEqual(appended.events.map((event) => event.event_id), ['e1', 'e4']);
    assert.deepEqual(existing.map((event) => event.event_id), ['e1']);
});

test('实时 append 不推进 snapshot 权威水位，乱序的更大 sequence 均保留', () => {
    const snapshotWater = 9;
    const afterEleven = appendRoomEvent([], { event_id: 'e11', sequence: 11 }, snapshotWater);
    const afterTen = appendRoomEvent(
        afterEleven.events,
        { event_id: 'e10', sequence: 10 },
        snapshotWater,
    );
    assert.equal(afterEleven.appended, true);
    assert.equal(afterTen.appended, true);
    assert.deepEqual(afterTen.events.map((event) => event.event_id), ['e10', 'e11']);

    const invalidatedTen = appendRoomEvent([], { event_id: 'e10', sequence: 10 }, 10);
    assert.equal(invalidatedTen.appended, false);
    assert.deepEqual(invalidatedTen.events, []);
});

test('房间发帖 payload 携带命令 ID、版本和可选回复目标', () => {
    const payload = buildRoomPostPayload({
        recipients: [{ member_id: 'member-a', expected_version: 4 }],
        content: '继续处理',
        mode: 'task',
        expectedRoomVersion: 9,
        commandId: 'command-1',
        replyState: { event_id: 'event-7' },
    });

    assert.deepEqual(payload, {
        recipients: [{ member_id: 'member-a', expected_version: 4 }],
        content: '继续处理',
        mode: 'task',
        thread_key: 'room',
        expected_room_version: 9,
        command_id: 'command-1',
        reply_to_event_id: 'event-7',
    });
    assert.equal(buildRoomPostPayload({
        recipients: [],
        content: '普通消息',
        mode: 'chat',
        expectedRoomVersion: 1,
        commandId: 'command-2',
        replyState: null,
    }).reply_to_event_id, null);
});

test('只有 room 和 command 都匹配的 accepted 才结算待发送消息', () => {
    const pending = { roomId: 'room-a', commandId: 'command-1' };
    assert.equal(matchesPendingRoomPost(pending, {
        type: 'room_message_accepted',
        room_id: 'room-a',
        command_id: 'command-1',
    }), true);
    assert.equal(matchesPendingRoomPost(pending, {
        type: 'room_message_accepted',
        room_id: 'room-b',
        command_id: 'command-1',
    }), false);
    assert.equal(matchesPendingRoomPost(pending, {
        type: 'room_message_accepted',
        room_id: 'room-a',
        command_id: 'command-2',
    }), false);
    assert.equal(matchesPendingRoomPost(null, {
        type: 'room_message_accepted',
        room_id: 'room-a',
        command_id: 'command-1',
    }), false);
});

test('目录更新只由 room 和 command 都匹配的 accepted 结算', () => {
    const pending = {
        type: 'directory',
        roomId: 'room-a',
        commandId: 'directory-command-1',
        expectedVersion: 1,
        previousDirectory: 'D:\\workspace\\old',
        requestedDirectory: 'nested/workspace',
        failed: false,
    };
    const canonicalSnapshot = {
        room: {
            room_id: 'room-a',
            version: 2,
            working_directory: 'D:\\root\\nested\\workspace',
        },
    };

    const accepted = settleRoomOperation(pending, {
        type: 'room_working_directory_accepted',
        room_id: 'room-a',
        command_id: 'directory-command-1',
        snapshot: canonicalSnapshot,
    });
    assert.equal(accepted.settled, true);
    assert.equal(accepted.pending, null);
    assert.equal(accepted.operationType, 'directory');
    assert.deepEqual(accepted.snapshot, canonicalSnapshot);

    assert.equal(settleRoomOperation(pending, {
        type: 'room_working_directory_accepted',
        room_id: 'room-b',
        command_id: 'directory-command-1',
        snapshot: canonicalSnapshot,
    }).settled, false);
    assert.equal(settleRoomOperation(pending, {
        type: 'room_working_directory_accepted',
        room_id: 'room-a',
        command_id: 'directory-command-2',
        snapshot: canonicalSnapshot,
    }).settled, false);
    assert.equal(settleRoomOperation(pending, {
        type: 'room_snapshot',
        snapshot: canonicalSnapshot,
    }).settled, false);
});

test('单一 room operation 阻止跨类型并发且精确 error 不要求清草稿', () => {
    const started = beginRoomOperation(null, {
        type: 'post',
        roomId: 'room-a',
        commandId: 'command-1',
    });
    assert.equal(started.started, true);
    assert.deepEqual(roomOperationControls(started.pending), {
        postDisabled: true,
        directoryDisabled: true,
        paginationDisabled: true,
    });

    const blocked = beginRoomOperation(started.pending, {
        type: 'directory',
        roomId: 'room-a',
    });
    assert.equal(blocked.started, false);
    assert.equal(blocked.pending, started.pending);

    const wrongType = settleRoomOperation(started.pending, {
        type: 'room_events_loaded_before',
        room_id: 'room-a',
        before_sequence: 10,
    });
    assert.equal(wrongType.settled, false);
    assert.equal(wrongType.pending, started.pending);

    const failed = settleRoomOperation(started.pending, {
        type: 'error',
        room_operation: 'post',
        room_id: 'room-a',
        command_id: 'command-1',
    });
    assert.equal(failed.settled, true);
    assert.equal(failed.operationType, 'post');
    assert.equal(failed.clearComposer, false);
    assert.equal(failed.directoryConfirmed, null);
    assert.equal(failed.pending, null);
    assert.deepEqual(roomOperationControls(failed.pending), {
        postDisabled: false,
        directoryDisabled: false,
        paginationDisabled: false,
    });
});

test('普通 error 和错误身份不会结算 room operation', () => {
    const pending = {
        type: 'post',
        roomId: 'room-a',
        commandId: 'command-1',
    };
    for (const error of [
        { type: 'error', message: '成员配置失败' },
        {
            type: 'error',
            room_operation: 'post',
            room_id: 'room-b',
            command_id: 'command-1',
        },
        {
            type: 'error',
            room_operation: 'post',
            room_id: 'room-a',
            command_id: 'command-2',
        },
        {
            type: 'error',
            room_operation: 'pagination',
            room_id: 'room-a',
            before_sequence: 10,
        },
    ]) {
        const result = settleRoomOperation(pending, error);
        assert.equal(result.settled, false);
        assert.equal(result.pending, pending);
    }
});

test('带房间身份的旧房间 error 不污染当前房间，通用 error 保持旧展示语义', () => {
    assert.equal(shouldHandleRoomError({
        type: 'error',
        room_operation: 'post',
        room_id: 'room-old',
        command_id: 'command-1',
    }, 'room-current'), false);
    assert.equal(shouldHandleRoomError({
        type: 'error',
        room_operation: 'post',
        room_id: 'room-current',
        command_id: 'command-1',
    }, 'room-current'), true);
    assert.equal(shouldHandleRoomError({
        type: 'error',
        message: '旧协议通用错误',
    }, 'room-current'), true);
});

test('三类 room operation 只由完整相关身份的 error 结算', () => {
    const directory = {
        type: 'directory',
        roomId: 'room-a',
        commandId: 'directory-command-1',
    };
    assert.equal(settleRoomOperation(directory, {
        type: 'error',
        room_operation: 'directory',
        room_id: 'room-a',
        command_id: 'directory-command-2',
    }).settled, false);
    const directoryError = settleRoomOperation(directory, {
        type: 'error',
        room_operation: 'directory',
        room_id: 'room-a',
        command_id: 'directory-command-1',
    });
    assert.equal(directoryError.settled, true);
    assert.equal(directoryError.clearComposer, false);
    assert.equal(directoryError.directoryConfirmed, false);

    const pagination = {
        type: 'pagination',
        roomId: 'room-a',
        beforeSequence: 10,
    };
    assert.equal(settleRoomOperation(pagination, {
        type: 'error',
        room_operation: 'pagination',
        room_id: 'room-a',
        before_sequence: 9,
    }).settled, false);
    const paginationError = settleRoomOperation(pagination, {
        type: 'error',
        room_operation: 'pagination',
        room_id: 'room-a',
        before_sequence: 10,
    });
    assert.equal(paginationError.settled, true);
    assert.equal(paginationError.clearComposer, false);
});

test('本地发送失败能为三类 operation 构造完整错误身份', () => {
    assert.deepEqual(buildRoomOperationError({
        type: 'post', roomId: 'room-a', commandId: 'command-1',
    }), {
        type: 'error',
        room_operation: 'post',
        room_id: 'room-a',
        command_id: 'command-1',
    });
    assert.deepEqual(buildRoomOperationError({
        type: 'directory',
        roomId: 'room-a',
        commandId: 'directory-command-1',
    }), {
        type: 'error',
        room_operation: 'directory',
        room_id: 'room-a',
        command_id: 'directory-command-1',
    });
    assert.deepEqual(buildRoomOperationError({
        type: 'pagination', roomId: 'room-a', beforeSequence: 10,
    }), {
        type: 'error',
        room_operation: 'pagination',
        room_id: 'room-a',
        before_sequence: 10,
    });
});

test('发帖和分页 operation 只由匹配类型及标识的权威响应结算', () => {
    const post = {
        type: 'post',
        roomId: 'room-a',
        commandId: 'command-1',
    };
    const wrongCommand = settleRoomOperation(post, {
        type: 'room_message_accepted',
        room_id: 'room-a',
        command_id: 'command-2',
    });
    assert.equal(wrongCommand.settled, false);
    const accepted = settleRoomOperation(post, {
        type: 'room_message_accepted',
        room_id: 'room-a',
        command_id: 'command-1',
    });
    assert.equal(accepted.settled, true);
    assert.equal(accepted.clearComposer, true);

    const pagination = {
        type: 'pagination',
        roomId: 'room-a',
        beforeSequence: 10,
    };
    assert.equal(settleRoomOperation(pagination, {
        type: 'room_events_loaded_before',
        room_id: 'room-a',
        before_sequence: 9,
    }).settled, false);
    assert.equal(settleRoomOperation(pagination, {
        type: 'room_events_loaded_before',
        room_id: 'room-a',
        before_sequence: 10,
    }).settled, true);

});

test('切换房间会重置全部 room-local 编辑器与分页状态', () => {
    assert.deepEqual(resetRoomUiState(), {
        replyState: null,
        pendingRoomPost: null,
        hasEarlierRoomEvents: false,
        hasLoadedEarlierRoomEvents: false,
        preserveTimelineAnchor: false,
        pendingRoomOperation: null,
        authoritativeRoomEventSequence: 0,
        authoritativeRoomSnapshotVersion: 0,
        authoritativeRoomStateRevision: 0,
    });
});

test('权威快照仅保留窗口之前的分页前缀并替换整个新窗口', () => {
    const existing = [
        { event_id: 'e1', sequence: 1, content: '已分页前缀' },
        { event_id: 'e2', sequence: 2, content: '已分页前缀 2' },
        { event_id: 'e8', sequence: 8, content: '旧版本' },
        { event_id: 'e9', sequence: 9, content: '应被失效' },
        { event_id: 'e11', sequence: 11, content: '快照捕获后已到达的实时事件' },
    ];
    const authoritativeWindow = [
        { event_id: 'e8', sequence: 8, content: '权威版本' },
        { event_id: 'e10', sequence: 10, content: '新事件' },
    ];

    const merged = mergeSnapshotWindow(existing, authoritativeWindow, 10, true);

    assert.deepEqual(merged.map((event) => event.event_id), ['e1', 'e2', 'e8', 'e10', 'e11']);
    assert.equal(merged[2].content, '权威版本');
    assert.deepEqual(existing.map((event) => event.event_id), ['e1', 'e2', 'e8', 'e9', 'e11']);
    assert.deepEqual(authoritativeWindow.map((event) => event.event_id), ['e8', 'e10']);
});

test('空权威窗口不会复活已有事件且失效引用会被清理', () => {
    assert.deepEqual(mergeSnapshotWindow(
        [
            { event_id: 'stale', sequence: 4 },
            { event_id: 'live', sequence: 11 },
        ],
        [],
        10,
        true,
    ), [{ event_id: 'live', sequence: 11 }]);
    assert.equal(reconcileReplyState(
        { event_id: 'stale', content: '旧引用' },
        [{ event_id: 'fresh', sequence: 5 }],
    ), null);
    const current = { event_id: 'fresh', content: '有效引用' };
    assert.equal(reconcileReplyState(current, [{ event_id: 'fresh', sequence: 5 }]), current);
});

test('协作快照水位和实体版本只允许单调前进', () => {
    assert.equal(shouldApplyRoomSnapshot(null, {
        room: { room_id: 'room-a', state_revision: 0, version: 1, latest_event_seq: 0 },
    }), true);
    const current = {
        roomId: 'room-a',
        stateRevision: 8,
        version: 7,
        eventSequence: 10,
    };
    [
        { state_revision: 7, version: 7, latest_event_seq: 10 },
        { state_revision: 8, version: 6, latest_event_seq: 10 },
        { state_revision: 8, version: 7, latest_event_seq: 9 },
    ].forEach((room) => assert.equal(shouldApplyRoomSnapshot(current, {
        room: { room_id: 'room-a', ...room },
    }), false));
    assert.equal(shouldApplyRoomSnapshot(current, {
        room: { room_id: 'room-a', state_revision: 8, version: 7, latest_event_seq: 10 },
    }), false);
    assert.equal(shouldApplyRoomSnapshot(current, {
        room: { room_id: 'room-a', state_revision: 9, version: 7, latest_event_seq: 10 },
    }), true);

    const original = [{ member_id: 'member-a', version: 2, display_name: '旧名称' }];
    const added = mergeVersionedEntity(
        original,
        { member_id: 'member-b', version: 1, display_name: '新成员' },
        'member_id',
    );
    assert.equal(added.changed, true);
    assert.deepEqual(added.items.map((item) => item.member_id), ['member-a', 'member-b']);
    assert.deepEqual(original, [{ member_id: 'member-a', version: 2, display_name: '旧名称' }]);

    [1, 2].forEach((version) => {
        const unchanged = mergeVersionedEntity(
            original,
            { member_id: 'member-a', version, display_name: '过期名称' },
            'member_id',
        );
        assert.equal(unchanged.changed, false);
        assert.equal(unchanged.items, original);
    });
    const replaced = mergeVersionedEntity(
        original,
        { member_id: 'member-a', version: 3, display_name: '新名称' },
        'member_id',
    );
    assert.equal(replaced.changed, true);
    assert.equal(replaced.items[0].display_name, '新名称');
    assert.deepEqual(original, [{ member_id: 'member-a', version: 2, display_name: '旧名称' }]);
});

test('加载过较早页后普通快照不会覆盖分页 has_more 结论', () => {
    assert.equal(resolveHasEarlierEvents({
        snapshotHasEarlier: true,
        currentHasEarlier: false,
        hasLoadedEarlier: true,
    }), false);
    assert.equal(resolveHasEarlierEvents({
        snapshotHasEarlier: true,
        currentHasEarlier: false,
        hasLoadedEarlier: false,
    }), true);
});

test('流式进度只在更新前处于底部跟随区时自动滚动', () => {
    assert.equal(isTimelineNearBottom({
        scrollHeight: 1000,
        scrollTop: 600,
        clientHeight: 400,
    }), true);
    assert.equal(isTimelineNearBottom({
        scrollHeight: 1000,
        scrollTop: 565,
        clientHeight: 400,
    }), true);
    assert.equal(isTimelineNearBottom({
        scrollHeight: 1000,
        scrollTop: 500,
        clientHeight: 400,
    }), false);
});

test('时间线重建统一区分保持视口、跟随底部和分页锚点', () => {
    const reading = captureTimelineViewport({
        scrollHeight: 1000,
        scrollTop: 500,
        clientHeight: 400,
    });
    const following = captureTimelineViewport({
        scrollHeight: 1000,
        scrollTop: 600,
        clientHeight: 400,
    });

    assert.equal(timelineScrollTarget(reading, 1200, 'preserve'), 500);
    assert.equal(timelineScrollTarget(reading, 1200, 'follow-if-near-bottom'), 500);
    assert.equal(timelineScrollTarget(following, 1200, 'follow-if-near-bottom'), 1200);
    assert.equal(timelineScrollTarget(reading, 1200, 'latest'), 1200);
    assert.equal(timelineScrollTarget(reading, 1200, 'prepend'), 700);
});

test('composer 仅在启用且没有可见 modal 时获取焦点', () => {
    assert.equal(shouldFocusComposer(true, false), true);
    assert.equal(shouldFocusComposer(true, true), false);
    assert.equal(shouldFocusComposer(false, false), false);
    assert.equal(shouldFocusComposer(false, true), false);
});
