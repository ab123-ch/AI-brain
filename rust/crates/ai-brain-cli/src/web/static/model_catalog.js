(function exposeModelCatalog(root, factory) {
    const api = factory();
    if (typeof module !== 'undefined' && module.exports) module.exports = api;
    if (root) root.ModelCatalog = api;
}(typeof globalThis === 'undefined' ? null : globalThis, () => {
    function text(value) {
        const normalized = String(value || '').trim();
        return normalized || null;
    }

    function compactLabel(policy = {}) {
        return text(policy.label) || text(policy.model) || text(policy.policy_id) || '';
    }

    function optionText(policy = {}) {
        const label = text(policy.label) || text(policy.policy_id) || text(policy.model);
        return [...new Set([
            label,
            text(policy.provider),
            text(policy.model),
        ].filter(Boolean))].join(' · ');
    }

    function optionTextOrFallback(policy, policyId) {
        return policy ? optionText(policy) : text(policyId) || '';
    }

    return {
        compactLabel,
        optionText,
        optionTextOrFallback,
    };
}));
