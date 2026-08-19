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

    function appendRoomEvent(existing, event, authoritativeSequence = 0) {
        const eventSequence = Number(event?.sequence);
        const highWater = Number(authoritativeSequence);
        if (Number.isFinite(eventSequence)
            && Number.isFinite(highWater)
            && eventSequence <= highWater) {
            return { events: existing, appended: false };
        }
        if ((existing || []).some((candidate) => candidate.event_id === event?.event_id)) {
            return { events: existing, appended: false };
        }
        return {
            events: mergeEventsBySequence(existing, [event]),
            appended: true,
        };
    }

    function mergeSnapshotWindow(
        existing,
        authoritativeWindow,
        throughSequence,
        preservePagedPrefix = true,
    ) {
        const authoritative = authoritativeWindow || [];
        const through = Number(throughSequence);
        const liveSuffix = Number.isFinite(through)
            ? (existing || []).filter((event) => Number(event.sequence) > through)
            : [];
        if (authoritative.length === 0) return mergeEventsBySequence([], liveSuffix);

        const firstSequence = Math.min(
            ...authoritative.map((event) => Number(event.sequence)),
        );
        const authoritativeIds = new Set(authoritative.map((event) => event.event_id));
        const pagedPrefix = preservePagedPrefix
            ? (existing || []).filter((event) => (
                Number(event.sequence) < firstSequence
                && !authoritativeIds.has(event.event_id)
            ))
            : [];
        return mergeEventsBySequence([...pagedPrefix, ...liveSuffix], authoritative);
    }

    function shouldApplyRoomSnapshot(current, snapshot) {
        if (!snapshot?.room) return false;
        if (!current || current.roomId !== snapshot.room.room_id) return true;
        const incoming = [
            Number(snapshot.room.state_revision || 0),
            Number(snapshot.room.version || 0),
            Number(snapshot.room.latest_event_seq || 0),
        ];
        const authoritative = [
            Number(current.stateRevision || 0),
            Number(current.version || 0),
            Number(current.eventSequence || 0),
        ];
        return incoming.every((value, index) => value >= authoritative[index])
            && incoming.some((value, index) => value > authoritative[index]);
    }

    function mergeVersionedEntity(items, incoming, idKey, appendMissing) {
        const currentItems = Array.isArray(items) ? items : [];
        const index = currentItems.findIndex((item) => item[idKey] === incoming[idKey]);
        if (index < 0) {
            if (!appendMissing) return { items: currentItems, changed: false };
            return { items: [...currentItems, incoming], changed: true };
        }
        if (Number(incoming.version) <= Number(currentItems[index].version)) {
            return { items: currentItems, changed: false };
        }
        const merged = [...currentItems];
        merged[index] = incoming;
        return { items: merged, changed: true };
    }

    function reconcileReplyState(replyState, events) {
        if (!replyState) return null;
        return (events || []).some((event) => event.event_id === replyState.event_id)
            ? replyState
            : null;
    }

    function buildRoomPostPayload({
        recipients,
        content,
        mode,
        expectedRoomVersion,
        commandId,
        replyState,
    }) {
        return {
            recipients,
            content,
            mode,
            thread_key: 'room',
            expected_room_version: Number(expectedRoomVersion),
            command_id: commandId,
            reply_to_event_id: replyState?.event_id || null,
        };
    }

    function matchesPendingRoomPost(pending, accepted) {
        return Boolean(pending)
            && accepted?.type === 'room_message_accepted'
            && pending.roomId === accepted.room_id
            && pending.commandId === accepted.command_id;
    }

    function beginRoomOperation(current, next) {
        if (current) return { started: false, pending: current };
        return { started: true, pending: { ...next } };
    }

    function roomOperationControls(pending) {
        const disabled = Boolean(pending);
        return {
            postDisabled: disabled,
            directoryDisabled: disabled,
            paginationDisabled: disabled,
        };
    }

    function buildRoomOperationError(pending) {
        if (!pending) return { type: 'error' };
        const error = {
            type: 'error',
            room_operation: pending.type,
            room_id: pending.roomId,
        };
        if (pending.type === 'post') {
            error.command_id = pending.commandId;
        } else if (pending.type === 'directory') {
            error.command_id = pending.commandId;
        } else if (pending.type === 'pagination') {
            error.before_sequence = Number(pending.beforeSequence);
        }
        return error;
    }

    function matchesRoomOperationError(pending, event) {
        if (event?.type !== 'error') return false;
        if (event.room_operation !== pending?.type || event.room_id !== pending.roomId) return false;
        if (pending.type === 'post') {
            return event.command_id === pending.commandId;
        }
        if (pending.type === 'directory') {
            return event.command_id === pending.commandId;
        }
        if (pending.type === 'pagination') {
            return Number(event.before_sequence) === Number(pending.beforeSequence);
        }
        return false;
    }

    function shouldHandleRoomError(error, activeRoomId) {
        if (!error?.room_operation) return true;
        return Boolean(activeRoomId) && error.room_id === activeRoomId;
    }

    function settleRoomOperation(pending, event) {
        const unsettled = {
            settled: false,
            pending,
            operationType: null,
            clearComposer: false,
            directoryConfirmed: null,
            snapshot: null,
        };
        if (!pending) return unsettled;

        if (event?.type === 'error') {
            if (!matchesRoomOperationError(pending, event)) return unsettled;
            return {
                settled: true,
                pending: null,
                operationType: pending.type,
                clearComposer: false,
                directoryConfirmed: pending.type === 'directory' ? false : null,
            };
        }

        let matches = false;
        if (pending.type === 'post') {
            matches = matchesPendingRoomPost(pending, event);
        } else if (pending.type === 'directory') {
            matches = event?.type === 'room_working_directory_accepted'
                && pending.roomId === event.room_id
                && pending.commandId === event.command_id;
        } else if (pending.type === 'pagination') {
            matches = event?.type === 'room_events_loaded_before'
                && pending.roomId === event.room_id
                && Number(pending.beforeSequence) === Number(event.before_sequence);
        }

        if (!matches) return unsettled;
        return {
            settled: true,
            pending: null,
            operationType: pending.type,
            clearComposer: pending.type === 'post',
            directoryConfirmed: null,
            snapshot: pending.type === 'directory' ? event.snapshot : null,
        };
    }

    function resetRoomUiState() {
        return {
            replyState: null,
            pendingRoomPost: null,
            hasEarlierRoomEvents: false,
            hasLoadedEarlierRoomEvents: false,
            preserveTimelineAnchor: false,
            pendingRoomOperation: null,
            authoritativeRoomEventSequence: 0,
            authoritativeRoomSnapshotVersion: 0,
            authoritativeRoomStateRevision: 0,
        };
    }

    function resolveHasEarlierEvents({
        snapshotHasEarlier,
        currentHasEarlier,
        hasLoadedEarlier,
    }) {
        return hasLoadedEarlier
            ? Boolean(currentHasEarlier)
            : Boolean(snapshotHasEarlier);
    }

    function isTimelineNearBottom(container, threshold = 48) {
        if (!container) return false;
        const distance = Number(container.scrollHeight)
            - Number(container.clientHeight)
            - Number(container.scrollTop);
        return Number.isFinite(distance) && distance <= Number(threshold);
    }

    function captureTimelineViewport(container, threshold = 48) {
        return {
            scrollHeight: Number(container?.scrollHeight || 0),
            scrollTop: Number(container?.scrollTop || 0),
            clientHeight: Number(container?.clientHeight || 0),
            nearBottom: isTimelineNearBottom(container, threshold),
        };
    }

    function timelineScrollTarget(viewport, nextScrollHeight, policy = 'preserve') {
        const nextHeight = Number(nextScrollHeight || 0);
        if (policy === 'latest') return nextHeight;
        if (policy === 'follow-if-near-bottom' && viewport?.nearBottom) return nextHeight;
        if (policy === 'prepend') {
            return Number(viewport?.scrollTop || 0)
                + Math.max(0, nextHeight - Number(viewport?.scrollHeight || 0));
        }
        return Number(viewport?.scrollTop || 0);
    }

    function createFrameScheduler(callback, requestFrame, cancelFrame = null) {
        if (typeof callback !== 'function' || typeof requestFrame !== 'function') {
            throw new TypeError('frame scheduler requires callback and requestFrame functions');
        }
        let frameId = null;
        return {
            schedule() {
                if (frameId !== null) return false;
                frameId = requestFrame(() => {
                    frameId = null;
                    callback();
                });
                return true;
            },
            cancel() {
                if (frameId === null) return false;
                if (typeof cancelFrame === 'function') cancelFrame(frameId);
                frameId = null;
                return true;
            },
            isPending() {
                return frameId !== null;
            },
        };
    }

    function createTimelineBottomFollower(
        scrollToBottom,
        requestFrame,
        cancelFrame = null,
        settleFrameCount = 3,
    ) {
        if (typeof scrollToBottom !== 'function' || typeof requestFrame !== 'function') {
            throw new TypeError('timeline follower requires scroll and frame functions');
        }
        const frameCount = Number(settleFrameCount);
        if (!Number.isInteger(frameCount) || frameCount < 1) {
            throw new TypeError('timeline follower frame count must be a positive integer');
        }

        let active = false;
        let frameId = null;
        let framesRemaining = 0;
        let generation = 0;

        function scheduleNextFrame() {
            if (!active || frameId !== null) return;
            const scheduledGeneration = generation;
            frameId = requestFrame(() => {
                if (scheduledGeneration !== generation) return;
                frameId = null;
                if (!active) return;
                scrollToBottom();
                framesRemaining -= 1;
                if (framesRemaining > 0) scheduleNextFrame();
                else active = false;
            });
        }

        return {
            follow() {
                active = true;
                framesRemaining = frameCount;
                scrollToBottom();
                scheduleNextFrame();
            },
            cancel() {
                const wasActive = active || frameId !== null;
                active = false;
                framesRemaining = 0;
                generation += 1;
                if (frameId !== null && typeof cancelFrame === 'function') cancelFrame(frameId);
                frameId = null;
                return wasActive;
            },
            isActive() {
                return active;
            },
        };
    }

    function shouldFocusComposer(enabled, modalOpen) {
        return Boolean(enabled) && !modalOpen;
    }

    return {
        appendRoomEvent,
        beginRoomOperation,
        beginReply,
        buildRoomOperationError,
        buildRoomPostPayload,
        canReplyToEvent,
        captureTimelineViewport,
        createFrameScheduler,
        createTimelineBottomFollower,
        isTimelineNearBottom,
        matchesPendingRoomPost,
        mergeVersionedEntity,
        mergeEventsBySequence,
        mergeSnapshotWindow,
        reconcileReplyState,
        resetRoomUiState,
        roomOperationControls,
        shouldApplyRoomSnapshot,
        shouldFocusComposer,
        shouldHandleRoomError,
        settleRoomOperation,
        timelineScrollTarget,
        resolveHasEarlierEvents,
    };
}));
