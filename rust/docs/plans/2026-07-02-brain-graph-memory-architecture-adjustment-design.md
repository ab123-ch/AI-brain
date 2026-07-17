# Brain Graph Memory Architecture Adjustment

## Background

The main project is adding `crates/brain-graph` as a native knowledge graph capability for AI Brain. The current memory pyramid stores raw memory, task summaries, abstract experience, and subconscious triggers in separate files. This document adjusts that design so the graph becomes the relationship and index layer for memory, while raw text remains in the existing memory files.

This design applies only to the main project. The standalone `../novel-knowledge-graph` project is out of scope.

## Core Direction

The graph should not store complete memory text. It should store relationships, catalog entries, summaries, and references to the original memory files.

The graph becomes the middle layer of the memory system:

```text
L1 raw memory files
  <- referenced by
Memory graph
  - catalog entries
  - task summaries
  - concept summaries
  - experience abstractions
  - triggers
  - file/code/tool relationships
  -> projected into
L4 prompt injection cache
```

The raw memory layer remains the source of truth for full text. The graph stores enough information for the model to decide which memory item is relevant, understand the relationship context, and request exact source paragraphs only when needed.

## Replaced And Preserved Layers

Preserve:

- L1 raw memory files. They continue to store full conversation turns, tool results, and source text.
- Exact paragraph/file references. These point back into L1 or code files.

Move into graph:

- L2 task summaries.
- L3 experience abstractions.
- Concept-level memory summaries, such as "红冲资费生成逻辑解释".
- Relationships between memories, source tasks, code files, tools, decisions, and pitfalls.

Project from graph:

- L4 subconscious triggers and short narrative text. L4 should become a graph projection or cache, not an independent source of truth.

## Catalog-First Recall

High-frequency keywords can match many graph nodes. The first query must not return full summaries, all edges, or source references. It should return a sorted catalog view.

Each memory node should have catalog fields:

```json
{
  "catalog_title": "红冲资费生成逻辑解释",
  "catalog_keywords": ["红冲", "资费", "生成逻辑"],
  "catalog_type": "business_logic",
  "catalog_rank_hint": 0.91
}
```

Example catalog result for keyword `红冲`:

```text
1. 红冲资费生成逻辑解释
2. 修改红冲原费用逻辑
3. 红冲资费推送 BMS 异步调用
4. 红冲失败重试和幂等处理
5. 红冲和普通退款的区别
```

The model should use this catalog to choose specific node IDs for detail lookup. Catalog search is the first entry point; full graph recall is not.

## Three-Stage Query Flow

The graph memory tool should expose a three-stage flow:

```text
search_catalog(query)
  -> get_node_detail(node_id)
  -> resolve_refs(refs)
```

### 1. `search_catalog`

Purpose: discover possible memory entries with very low context cost.

Returns:

- node ID
- catalog title
- catalog type
- matched keywords
- score
- optional one-line hint

Does not return:

- full summary
- all relationships
- raw paragraph text
- large source reference lists

### 2. `get_node_detail`

Purpose: load one selected memory node and enough relationship context for reasoning.

Returns:

- center node summary
- upstream memories
- downstream memories
- directly related concepts, files, tools, decisions, pitfalls
- source references, still as references only

Example:

```text
Center: 红冲资费推送 BMS 异步调用
Summary: 红冲资费生成后通过异步流程推送到 BMS，重点关注消息状态、重试、幂等和失败补偿。

Upstream:
- 红冲资费生成逻辑解释
- 修改红冲原费用逻辑

Downstream:
- BMS 推送失败重试
- 异步调用幂等处理

Source refs:
- sess-xxx paragraphs 8-12
- sess-yyy paragraphs 3-5
```

### 3. `resolve_refs`

Purpose: read exact source paragraphs or code snippets only after the model decides they are needed.

Returns:

- source file path
- session ID if applicable
- paragraph indexes or line ranges
- selected text

This stage may load raw text. Earlier stages should not.

## Memory Node Types

Recommended `props.layer` values for `NodeKind::Memory`:

- `catalog_entry`: directory-facing memory entry.
- `task_summary`: former L2 task summary.
- `experience`: former L3 abstract experience.
- `concept_summary`: concept-level summary across multiple sessions.
- `trigger`: former L4 trigger.
- `l1_ref`: pointer to raw memory paragraphs.
- `decision`: architectural or implementation decision.
- `pitfall`: known issue, trap, or caution.

Example memory node:

```json
{
  "kind": "Memory",
  "graph_type": "Memory",
  "props": {
    "layer": "concept_summary",
    "catalog_title": "红冲资费生成逻辑解释",
    "catalog_keywords": ["红冲", "资费", "生成逻辑"],
    "catalog_type": "business_logic",
    "summary": "解释红冲资费如何生成、依赖哪些账单状态和原费用数据。",
    "l1_refs": [
      {
        "session": "sess-xxx",
        "file": "personas/default/pyramid/l1-raw/sess-xxx.jsonl",
        "paragraphs": [12, 13, 14]
      }
    ]
  }
}
```

## Relationship Semantics

Existing edge kinds can cover the first version:

- `MentionedIn`: a memory, concept, or entity appears in an L1 reference.
- `DerivedFrom`: a summary or experience was abstracted from another memory node.
- `DependsOn`: one memory depends on another to be understood.
- `CausedBy`: a pitfall, decision, or fix was caused by a previous issue.
- `RelatedTo`: general association.
- `SimilarTo`: similar memory entries.
- `Invokes`: memory involving a tool or skill.

Additional edge kinds may be useful later:

- `ResolvedBy`: problem solved by a solution.
- `Supersedes`: newer memory replaces older memory.
- `EvidenceOf`: source reference supports a conclusion.
- `Contradicts`: newer conclusion conflicts with an older one.

## Upstream And Downstream Trace

The graph should support tracing from one selected node:

```text
trace_memory(node_id, direction, depth, budget)
```

Directions:

- `upstream`: causes, sources, evidence, dependencies.
- `downstream`: consequences, reuse, decisions, fixes, later tasks.
- `both`: compact view of both sides.

The trace result should be model-readable and budget-limited. It should show the cause-process-result chain without resolving raw text automatically.

## Context Budget Rules

The graph tool must assume high-frequency keywords can match too much.

Default behavior:

- Catalog search returns titles and IDs only.
- Detail lookup expands one selected node at a time.
- Trace is depth-limited and token-budgeted.
- Source resolution is explicit and late.

Recommended budgets:

- Catalog view: small enough for quick model choice.
- Node detail: enough for local reasoning.
- Source resolution: only selected refs.

## Implementation Plan

1. Add catalog fields convention to memory graph node props.
2. Add `CatalogEntry` and `search_catalog` to `brain-graph`.
3. Add `MemoryRef` / `L1Ref` props convention for raw memory paragraph references.
4. Add `get_node_detail` that returns center node, compact neighbors, and unresolved refs.
5. Add `trace_memory` with direction, depth, and budget.
6. Add `resolve_refs` integration in `brain-memory`, because reading raw files belongs to the memory layer, not the graph storage crate.
7. Migrate L2 and L3 memory summaries into graph nodes.
8. Convert L4 subconscious into a graph projection or cache.

## Non-Goals

- Do not store full raw memory text inside the graph.
- Do not return all matching nodes with full summaries for broad keywords.
- Do not remove L1 raw memory files.
- Do not make L4 subconscious an independent source of truth after graph migration.
