#!/bin/bash
# import-patches.sh — 导入补丁文件并推送到 GitHub
# 支持 macOS / Linux / Windows (Git Bash / MSYS2)
#
# 用法:
#   ./import-patches.sh <补丁目录>                    # 自动读取 .branch 文件
#   ./import-patches.sh <补丁目录> <分支名>           # 手动指定分支
#
# Windows 用法:
#   bash import-patches.sh ./brain-patches
#

set -e

# ── 检查参数 ──────────────────────────────────────────
if [ -z "$1" ]; then
    echo "用法: $0 <补丁目录> [分支名]"
    echo ""
    echo "示例:"
    echo "  $0 ./brain-patches"
    echo "  $0 ./brain-patches featrue/20260404-nao"
    exit 1
fi

PATCH_DIR="$1"

# 兼容 Windows 路径 (D:\xxx -> /d/xxx)
if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "win32" || "$OSTYPE" == "cygwin" ]]; then
    # MSYS2/Git Bash 下自动转换 Windows 路径
    case "$PATCH_DIR" in
        [a-zA-Z]:*) PATCH_DIR="/$(echo "$PATCH_DIR" | sed 's|:\\|/|;s|\\|/|g')" ;;
    esac
fi

if [ ! -d "$PATCH_DIR" ]; then
    echo "错误: 目录不存在: $PATCH_DIR"
    exit 1
fi

# ── 读取分支名 ───────────────────────────────────────
if [ -n "$2" ]; then
    BRANCH="$2"
elif [ -f "$PATCH_DIR/.branch" ]; then
    BRANCH=$(cat "$PATCH_DIR/.branch" | tr -d '[:space:]')
else
    echo "错误: 未指定分支名，且补丁目录中没有 .branch 文件"
    echo "用法: $0 <补丁目录> <分支名>"
    exit 1
fi

# ── 统计补丁文件 ─────────────────────────────────────
PATCH_COUNT=$(ls "$PATCH_DIR"/*.patch 2>/dev/null | wc -l | tr -d ' ')
if [ "$PATCH_COUNT" -eq 0 ]; then
    echo "错误: 补丁目录中没有 .patch 文件: $PATCH_DIR"
    exit 1
fi

echo "=== Git 补丁导入工具 ==="
echo "补丁目录: $PATCH_DIR"
echo "补丁数量: $PATCH_COUNT"
echo "目标分支: $BRANCH"
echo ""

# ── 检查是否已有仓库 ─────────────────────────────────
REMOTE_URL=""
if [ -f "$PATCH_DIR/.remote" ]; then
    REMOTE_URL=$(cat "$PATCH_DIR/.remote" | tr -d '[:space:]')
fi

REPO_DIR="AI-brain"
CLONE_NEEDED=false

if [ -d "$REPO_DIR/.git" ]; then
    echo "检测到已有仓库: $REPO_DIR"
    cd "$REPO_DIR"
    git fetch origin 2>/dev/null || true
else
    if [ -z "$REMOTE_URL" ]; then
        echo "错误: 没有 .remote 文件且本地没有仓库，无法克隆"
        exit 1
    fi
    echo "克隆仓库: $REMOTE_URL"
    git clone "$REMOTE_URL" "$REPO_DIR"
    cd "$REPO_DIR"
    CLONE_NEEDED=true
fi

# ── 切换/创建分支 ────────────────────────────────────
if git show-ref --verify --quiet "refs/heads/$BRANCH"; then
    echo "切换到分支: $BRANCH"
    git checkout "$BRANCH"
elif git show-ref --verify --quiet "refs/remotes/origin/$BRANCH"; then
    echo "从远程创建本地分支: $BRANCH"
    git checkout -b "$BRANCH" "origin/$BRANCH"
else
    echo "创建新分支: $BRANCH"
    git checkout -b "$BRANCH"
fi

# ── 导入补丁 ─────────────────────────────────────────
echo ""
echo "开始导入补丁..."
PATCH_PATH=$(cd "$OLDPWD" && pwd)/$PATCH_DIR
# 修正: PATCH_DIR 可能是绝对路径
if [[ "$PATCH_DIR" = /* ]]; then
    PATCH_PATH="$PATCH_DIR"
else
    PATCH_PATH="$(cd "$OLDPWD/$PATCH_DIR" && pwd)"
fi

# 按编号排序导入
if ! git am --keep-cr "$PATCH_PATH"/*.patch; then
    echo ""
    echo "!!! 补丁导入失败 !!!"
    echo "可能原因: 本地分支已有相同 commit 或存在冲突"
    echo ""
    echo "运行以下命令之一:"
    echo "  git am --abort       # 放弃导入"
    echo "  git am --continue    # 解决冲突后继续"
    echo "  git am --skip        # 跳过当前补丁"
    exit 1
fi

echo ""
echo "=== 补丁导入成功 ==="

# ── 推送 ──────────────────────────────────────────────
echo "推送到 origin/$BRANCH ..."
if git push -u origin "$BRANCH"; then
    echo ""
    echo "=== 推送完成 ==="
else
    echo ""
    echo "推送失败，请手动执行: git push -u origin $BRANCH"
    exit 1
fi
