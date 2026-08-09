(function exposeRoomReply(root, factory) {
    const api = factory();
    if (typeof module !== 'undefined' && module.exports) module.exports = api;
    if (root) root.RoomReply = api;
}(typeof globalThis === 'undefined' ? null : globalThis, () => {
    function canReplyToEvent(event) {
        return Boolean(event?.event_id)
            && ['user_message', 'member_message'].includes(event.kind)
            && ['user', 'member'].includes(event.sender_kind);
    }

    function beginReply(event, members) {
        if (!canReplyToEvent(event)) throw new Error('该消息不能被回复');

        const member = event.sender_kind === 'member'
            ? (members || []).find((candidate) => candidate.member_id === event.sender_id)
            : null;
        const active = member?.availability === 'active';
        let warning = null;

        if (event.sender_kind === 'member' && !member) {
            warning = `${event.sender_name || '该实例'} 当前不在房间成员列表中，不会自动唤醒`;
        } else if (member && !active) {
            warning = `${member.display_name || event.sender_name || '该实例'} 当前不可用，不会自动唤醒`;
        }

        return {
            reply: {
                event_id: event.event_id,
                sender_name: event.sender_name,
                sender_kind: event.sender_kind,
                content: String(event.content || ''),
            },
            auto_recipient_id: active ? member.member_id : null,
            warning,
        };
    }

    function mergeEventsBySequence(existing, incoming) {
        const byId = new Map((existing || []).map((event) => [event.event_id, event]));
        (incoming || []).forEach((event) => byId.set(event.event_id, event));
        return [...byId.values()].sort((left, right) => left.sequence - right.sequence);
    }

    return { beginReply, canReplyToEvent, mergeEventsBySequence };
}));
