#!/bin/bash
# AI Brain 快速验证脚本
# 用法: ./fast_check.sh
set -euo pipefail
cd "$(dirname "$0")"

echo "=== 1/3 编译 release ==="
cargo build --release -p ai-brain-cli 2>&1 | grep -E 'error|Compiling ai-brain|^$' || true

echo ""
echo "=== 2/3 单元测试 ==="
cargo test -p brain-core -p brain-llm 2>&1 | tail -5

echo ""
echo "=== 3/3 启动验证 (status) ==="
timeout 3 ./target/release/ai-brain status 2>&1 | grep -E '状态|失败|panic|LLM|回声' || true

echo ""
echo "✅ 验证完成"
