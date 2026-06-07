#!/bin/bash
# export-patches.sh — 导出当前分支未推送的 commit 为补丁文件
# 用法: ./export-patches.sh [输出目录]
#
# 默认输出到 /tmp/brain-patches/
#
# 自动处理「隔天同步」场景：
#   如果远程分支比本地新（昨晚从另一台电脑推送过），
#   会自动 fetch + reset 对齐，然后只导出真正新的 commit。

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

BRANCH=$(git branch --show-current)
if [ -z "$BRANCH" ]; then
    echo "错误: 不在任何分支上（可能处于 detached HEAD）"
    exit 1
fi

REMOTE_BRANCH="origin/$BRANCH"
OUTPUT_DIR="${1:-/tmp/brain-patches}"

# ── 拉取远程最新状态 ──────────────────────────────────
echo "fetch 远程..."
git fetch origin 2>/dev/null || true

# 检查远程分支是否存在
if ! git rev-parse --verify "$REMOTE_BRANCH" >/dev/null 2>&1; then
    echo "远程分支 $REMOTE_BRANCH 不存在，将导出全部 commit"
    REMOTE_BRANCH="origin/main"
    if ! git rev-parse --verify "$REMOTE_BRANCH" >/dev/null 2>&1; then
        # 连 main 都没有，导出所有 commit
        MERGE_BASE=$(git rev-list --max-parents=0 HEAD)
    else
        MERGE_BASE=$(git merge-base "$REMOTE_BRANCH" HEAD 2>/dev/null)
    fi
else
    # ── 检测「隔天同步」：远程是否有本地没有的 commit ──
    LOCAL_AHEAD=$(git log "$REMOTE_BRANCH"..HEAD --oneline 2>/dev/null | wc -l | tr -d ' ')
    REMOTE_AHEAD=$(git log HEAD.."$REMOTE_BRANCH" --oneline 2>/dev/null | wc -l | tr -d ' ')

    if [ "$REMOTE_AHEAD" -gt 0 ] && [ "$LOCAL_AHEAD" -gt 0 ]; then
        echo ""
        echo "⚠ 检测到远程有 $REMOTE_AHEAD 个新 commit（昨晚推送的），本地有 $LOCAL_AHEAD 个新 commit"
        echo "  自动同步：fetch + reset 对齐远程，然后只导出本地新增部分"
        echo ""

        # 保存当前工作区变更（如果有未提交的修改）
        STASH_NEEDED=false
        if ! git diff --quiet 2>/dev/null || ! git diff --cached --quiet 2>/dev/null; then
            echo "暂存未提交的工作区变更..."
            git stash push -m "auto-stash-before-sync-$(date +%Y%m%d%H%M%S)"
            STASH_NEEDED=true
        fi

        # reset 到远程（丢弃本地已导出过的旧 commit，保留远程推送的）
        git reset --hard "$REMOTE_BRANCH"
        echo "已同步到远程 $REMOTE_BRANCH"

        # 恢复工作区变更
        if [ "$STASH_NEEDED" = true ]; then
            echo "恢复工作区变更..."
            git stash pop
        fi

        echo ""
    elif [ "$REMOTE_AHEAD" -gt 0 ] && [ "$LOCAL_AHEAD" -eq 0 ]; then
        echo ""
        echo "远程有 $REMOTE_AHEAD 个新 commit，本地没有新 commit，自动同步"
        git reset --hard "$REMOTE_BRANCH"
        echo "已同步到远程 $REMOTE_BRANCH，无需导出"
        exit 0
    fi

    MERGE_BASE=$(git merge-base "$REMOTE_BRANCH" HEAD 2>/dev/null)
fi

if [ -z "$MERGE_BASE" ]; then
    echo "错误: 无法找到合并基点"
    exit 1
fi

# 统计未推送的 commit 数量
COMMIT_COUNT=$(git log "$MERGE_BASE"..HEAD --oneline | wc -l | tr -d ' ')

if [ "$COMMIT_COUNT" -eq 0 ]; then
    echo "没有未推送的 commit，无需导出"
    exit 0
fi

echo "分支: $BRANCH"
echo "未推送 commit: $COMMIT_COUNT 个"
echo "输出目录: $OUTPUT_DIR"
echo ""

# 创建输出目录
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

# 导出补丁
git format-patch "$MERGE_BASE" -o "$OUTPUT_DIR" --start-number=1

# 写入分支信息（导入脚本需要知道目标分支）
echo "$BRANCH" > "$OUTPUT_DIR/.branch"

# 保存远程仓库地址
REMOTE_URL=$(git remote get-url origin)
echo "$REMOTE_URL" > "$OUTPUT_DIR/.remote"

echo ""
echo "=== 导出完成 ==="
echo "补丁文件:"
ls -lh "$OUTPUT_DIR"/*.patch 2>/dev/null
echo ""
echo "目标分支: $BRANCH"
echo "远程仓库: $REMOTE_URL"
echo ""
echo "将 $OUTPUT_DIR 整个目录拷贝到另一台电脑，运行 import-patches.sh 即可"
