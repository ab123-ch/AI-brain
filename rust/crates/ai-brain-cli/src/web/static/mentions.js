(function exposeMentionRecipients(root, factory) {
    const api = factory();
    if (typeof module !== 'undefined' && module.exports) module.exports = api;
    if (root) root.MentionRecipients = api;
}(typeof globalThis === 'undefined' ? null : globalThis, () => {
    const MENTION_BOUNDARY = '(?=$|\\s|[，,。.!?！？；;:：、】【()（）{}\\[\\]])';

    function escapeRegExp(value) {
        return String(value).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    }

    function mentionPattern(displayName, flags = '') {
        return new RegExp(`@${escapeRegExp(displayName)}${MENTION_BOUNDARY}`, flags);
    }

    function containsMention(content, displayName) {
        return mentionPattern(displayName, 'u').test(String(content || ''));
    }

    function recipientMemberIds(content, members) {
        const recipients = new Set();
        (members || []).forEach((member) => {
            if (member.availability !== 'active') return;
            if (containsMention(content, member.display_name)) {
                recipients.add(member.member_id);
            }
        });
        return recipients;
    }

    function appendMention(content, displayName) {
        const current = String(content || '');
        if (containsMention(current, displayName)) return current;
        const prefix = current.trimEnd();
        return `${prefix}${prefix ? ' ' : ''}@${displayName} `;
    }

    function removeMention(content, displayName) {
        return String(content || '')
            .replace(mentionPattern(displayName, 'gu'), '')
            .replace(/[ \t]{2,}/g, ' ');
    }

    return {
        appendMention,
        recipientMemberIds,
        removeMention,
    };
}));
