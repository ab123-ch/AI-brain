---
name: code-verification
description: "代码变更验证。当主脑修改代码时适用：检查编译、测试、空实现、日志输出、需求匹配。"
---

# 代码变更验证

## Overview
代码修改后必须验证可编译、可测试、已实现完整、日志合规。不验证 = 不报告问题。

## The Iron Law
```
NO CODE VERIFICATION CLAIMS WITHOUT ACTUAL TOOL EXECUTION

编译未执行 = 不知道能否编译
测试未执行 = 不知道能否通过
空实现/TODO = 未完成任务
逻辑与需求不符 = 核心缺陷
println 输出 = TUI 崩溃风险 = 必须报告
```

## The Gate Function
```
BEFORE 报告代码相关问题：

1. CHECK: 主脑本轮是否有代码文件变更？（查看文件修改记录）
   - 无变更 → 本 skill 不适用，退出

2. COMPILE: 执行编译验证
   - Rust: cargo check
   - 根据项目技术栈选择对应命令
   - 记录编译结果（成功/失败/警告）

3. TEST: 执行测试验证（如果项目有测试）
   - Rust: cargo test --workspace
   - 检查测试输出是否 "0 failures"
   - 测试失败 = 必须报告

4. SCAN: 扫描变更代码中的未实现模式
   - 空方法体: fn foo() { } 或 fn foo() {}
   - TODO/FIXME/HACK 注释
   - 只有注释没有实际代码
   - unimplemented!() / todo!() 宏

5. LOGGING: 检查日志输出方式
   - grep println! / print! / eprintln! / dbg!
   - 任何匹配 → 必须报告（会导致 TUI 崩溃）
   - 正确方式：tracing info/debug/warn/error 输出到文件

6. MATCH: 对比实现与需求/开发计划
   - 用户任务是什么？
   - 实现是否覆盖了所有需求点？
   - 实现逻辑是否与需求描述一致？

7. ONLY THEN: 综合判断并报告
```

## 日志规范检查细则

### 禁止模式（任何场景无豁免）

**Rust 禁止模式：**
- `println!(...)` — 标准输出，会污染 TUI
- `print!(...)` — 标准输出
- `eprintln!(...)` — 标准错误
- `dbg!(...)` — 调试宏，输出到 stderr

**为什么禁止：**
- AI Brain 使用 TUI 模式运行
- println 输出到 stdout 会干扰 ratatui 渲染
- 导致 TUI 界面崩溃、乱码、状态异常

**正确方式：**
- 使用 `tracing::info!` / `debug!` / `warn!` / `error!`
- tracing 配置了文件 appender，输出到日志文件

### 检查命令

使用 grep_search 搜索变更文件目录：
- pattern: `println!|print!|eprintln!|dbg!`
- 匹配到任何结果 → 必须报告问题

## Common Failures
| 声明 | 需要 | 不充分 |
|------|------|--------|
| "代码可以编译" | cargo check 无错误 | "看起来没问题" |
| "测试通过" | cargo test "0 failures" | "应该能通过" |
| "实现完整" | 无 TODO/空方法 | "代码写完了" |
| "日志合规" | grep println 结果为空 | "用了 tracing" |
| "满足需求" | 对比需求逐点验证 | "功能差不多" |

## Red Flags - STOP
- 主脑说"已修改代码"但没看到编译/测试工具调用
- 变更代码中看到 `println!`、`print!`、`eprintln!`、`dbg!`
- 主脑说"只是调试代码" — 没有豁免场景
- 变更代码中看到 `TODO`、`// implement later`、空方法体
- 主脑说"实现完成"但只写了注释或骨架代码
- 实现逻辑与用户需求描述明显不符
- 想着"编译应该没问题"就跳过验证
- 测试输出中有 failures 但主脑没报告

## Rationalization Prevention
| 借口 | Reality |
|------|---------|
| "cargo check 应该能过" | RUN cargo check，看实际输出 |
| "测试应该通过" | RUN cargo test，看 0 failures |
| "这个 TODO 是备注不是未实现" | TODO = 未实现，必须报告 |
| "println 只是临时调试" | 临时调试也会崩溃 TUI，无豁免 |
| "这个 println 在测试代码里" | 测试代码也会被 TUI 捕获 |
| "dbg! 是 Rust 官方调试宏" | dbg! 输出到 stderr，同样影响 |
| "小改动不需要测试" | 任何代码变更都需要验证 |
| "逻辑应该是对的" | 对比需求逐点检查 |

## Key Patterns
```
✅ [See file change] → [Run cargo check] [See: no errors] → [Run cargo test] [See: 34/34 pass] → [grep println] [See: 0 matches] → "验证通过"
❌ [See file change] → "应该没问题" / "代码看起来正确"

✅ [Scan code] [See: no TODO/空方法] → [Compare with requirements] → "实现完整且匹配需求"
❌ [See: fn foo() { }] → "实现了 foo 方法" / 没提空实现

✅ [grep "println!|print!|eprintln!|dbg!"] [See: 0 matches] → "日志输出合规"
❌ [grep "println!"] [See: 3 matches] → "应该没问题" / "只是调试"

✅ [See: println in code] → "评估结果-存在问题。具体问题：1.存在 println 输出，会导致 TUI 崩溃，应改为 tracing 输出到日志文件"
❌ [See: println in code] → "评估结果-正常" / "只是调试用"

✅ [See: TODO in code] → "评估结果-存在问题。具体问题：1.方法 foo 存在 TODO，未实现完整逻辑 需要理解根据问题和要求/需求继续修改。"
❌ [See: TODO in code] → "评估结果-正常"
```

## When To Apply
- 文件修改记录中有 `.rs`、`.py`、`.ts` 等代码文件
- 主脑声称"已完成代码修改"、"已实现功能"
- 用户任务涉及功能开发、bug 修复、代码重构

## When NOT To Apply
- 纯文档修改（README、docs）
- 纯配置文件修改（.toml、.json 的非代码部分）
- 用户明确说"先写骨架，后续实现"
- 闲聊、问答任务，无代码变更

## The Bottom Line
代码变更必须走编译→测试→扫描→日志→匹配五步。不执行验证工具就不能声称代码没问题。println 输出会导致 TUI 崩溃，任何场景无豁免。