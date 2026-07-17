# User Architecture Preferences Reference

## Purpose

This document records inferred architecture preferences from user requirements and corrections. It is a reference for future design discussions and implementation planning. It should guide how architecture proposals are shaped, challenged, and refined.

These are inferred preferences, not fixed rules. When a future task conflicts with this document, confirm the tradeoff with the user.

## High-Level Preferences

The user prefers architecture that preserves clear source-of-truth boundaries. Derived systems should index, summarize, relate, or project information, but should not duplicate canonical raw data unless there is a strong reason.

The user also prefers systems that help the model make progressive decisions instead of flooding the context with everything that might be relevant.

## Source Of Truth Separation

The user values this separation:

```text
raw data
  -> indexed / summarized / related views
  -> model-facing selection views
  -> explicit detail retrieval
```

For memory architecture, this means:

- Raw full text should stay in raw memory files.
- Graph should store relationships, summaries, directory entries, and references.
- The model should resolve original paragraphs only when needed.

Avoid designs where an index silently becomes a second full data store.

## Catalog Before Detail

The user strongly prefers a catalog-first workflow for large result sets.

When a keyword can match many records, the system should first return a sorted directory of possible entries. The directory should help the model choose where to look next.

Preferred first response shape:

```text
1. 红冲资费生成逻辑解释
2. 修改红冲原费用逻辑
3. 红冲资费推送 BMS 异步调用
4. ...
```

Less preferred:

- Returning all matching summaries.
- Returning clusters when the user expects concrete memory entries.
- Returning raw excerpts too early.
- Hiding the actual selectable entries behind vague grouping.

## Progressive Retrieval

The user favors multi-step retrieval:

```text
find candidate entries
  -> select exact entry
  -> inspect graph relationships
  -> resolve original evidence
```

The model should be able to decide what it needs at each stage. Retrieval APIs should therefore expose stable IDs and clear next-step handles.

## Relationship-Oriented Memory

The user is interested in graph memory because it can represent cause, process, result, upstream context, downstream consequences, and cross-links.

A good design should allow:

- Start from one memory node.
- See upstream source memories and reasons.
- See downstream decisions, pitfalls, fixes, and later reuse.
- Let the model understand the chain before reading raw files.

The user is not just asking for keyword search. The graph should support reasoning over relationships.

## Context Budget Awareness

The user is sensitive to context bloat. Designs should avoid returning large content by default.

Preferred techniques:

- Return directory titles before summaries.
- Return compact summaries before source text.
- Use explicit limits and budgets.
- Defer full paragraph/code retrieval.
- Sort and filter before exposing results to the model.

The user expects the system to handle broad keywords like `红冲` without dumping dozens of summaries into the context.

## Naming And Concrete Entries

The user prefers concrete, human-readable memory entry names over abstract group labels when selecting from results.

Good catalog entries:

- `红冲资费生成逻辑解释`
- `修改红冲原费用逻辑`
- `红冲资费推送 BMS 异步调用`

Less useful entries:

- `资费冲正 cluster`
- `红冲相关内容`
- `业务逻辑组`

Grouping can exist internally, but model-facing selection should expose actionable entries.

## Design Correction Style

The user may accept the general direction of a proposal but reject the implementation shape. When this happens, preserve the agreed principle and revise the mechanism.

Example pattern:

```text
User: 思路正确但做法不对
Meaning: Do not discard the whole direction. Rework the abstraction boundary or API shape.
```

Respond by identifying the corrected boundary precisely.

## Preferred Architecture Traits

Favor:

- Explicit layers.
- Clear ownership of data.
- Stable references to source files and paragraphs.
- Relationship graphs for derived memory.
- Directory/catalog properties as first-class fields.
- APIs that support selection, detail lookup, trace, then source resolution.
- Designs that can evolve incrementally without deleting working components too early.

Avoid:

- Storing full duplicated text in derived indexes.
- Returning all details at the first query step.
- Replacing a component before its external behavior is preserved.
- Overly generic grouping when concrete entries are more useful.
- Making caches or projections become independent truth sources.

## How To Use This Reference

When helping design architecture for the user:

1. Identify the source of truth.
2. Identify which layers are indexes, summaries, projections, or caches.
3. Keep broad query results catalog-like.
4. Provide explicit APIs for detail and source resolution.
5. Call out context-budget behavior.
6. Prefer incremental replacement through adapters or projections.
7. Use concrete examples from the user's domain when explaining the shape.

## Current Example: Memory Graph Direction

The memory graph should become the middle memory layer:

- It can replace L2/L3 storage over time.
- It should keep L1 raw memory as source text.
- It can generate L4 subconscious as a projection.
- It should expose catalog search before detail search.
- It should store relationships and references, not full memory text.
