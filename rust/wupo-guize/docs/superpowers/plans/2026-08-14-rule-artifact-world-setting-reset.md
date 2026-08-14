# 规则法器版世界观重整实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 清除旧人物与旧副本设定，将 `settings/world_setting.md` 重写为唯一有效的规则法器版基础世界观。

**Architecture:** 采用单一权威文件，基础世界规律全部集中在 `settings/world_setting.md`；具体人物和副本以后另建文件。通过删除旧文件、扫描废弃关键词和检查必备机制，防止旧版域外体系、旧金手指和“午夜图书馆”继续污染新设定。

**Tech Stack:** Markdown、PowerShell、ripgrep、Git。

---

### Task 1: 建立唯一权威世界观文件

**Files:**
- Modify: `settings/world_setting.md`
- Reference: `docs/superpowers/specs/2026-08-14-rule-artifact-world-setting-design.md`

- [ ] **Step 1: 用新版基础设定完整替换旧世界观**

将 `settings/world_setting.md` 的旧内容全部删除，按下列固定结构重写：

```markdown
# 世界观：随机副本与气血武道

## 1. 世界基础
## 2. 副本征召
### 2.1 进入
### 2.2 返回
### 2.3 死亡
## 3. 全民气血武道
### 3.1 社会基础
### 3.2 武道本质
### 3.3 气血如龙
## 4. 副本物品
### 4.1 普通物品
### 4.2 永久强化物品
### 4.3 规则法器
## 5. 主角的金手指
### 5.1 单一刻录槽
### 5.2 掠夺式抽取
### 5.3 环境豁免
## 6. 世界阶段变化
### 6.1 前中期
### 6.2 后期：副本降临现实
## 7. 核心限制速查
```

正文必须逐项写明：

- 副本在全球随机抽人，毫无预警，不能拒绝或规避。
- 穿戴、手持、衣袋和随身背包中的物品一起进入副本。
- 通关者能够携带物品返回；死者、尸体和携带物永不返回，但现实中的档案与记忆保留。
- 全民武道源于武者在副本中更高的统计存活率，现代社会、法律、科技与政府继续存在。
- 气血武道以纯肉身为根基；顶尖强者达到气血如龙，可以与最低级副本中的低级鬼怪正面对抗，但不能无条件硬抗规则杀。
- 永久强化物品必须在副本内使用，强化结果能够永久带回现实。
- 规则法器在现实中是普通物品，在任意副本环境中恢复规则效果，可以跨副本携带和使用。
- 金手指来源、名称和载体不作解释。
- 金手指只有一个规则槽；刻录新规则永久覆盖旧规则。
- 刻录必须从有效规则法器中抽取；抽取后原法器永久变成普通物品。
- 刻录规则在现实与副本中均能使用。
- 前中期主角是唯一能在现实使用规则力量的人。
- 后期副本降临现实，普通规则法器也能在现实生效；主角仍保留抽取、剥夺和单槽覆盖能力。
- 不建立具体组织、人物、副本、武道等级表或幕后起源。

- [ ] **Step 2: 检查新版文件的标题结构**

Run:

```powershell
rg -n '^#{1,3} ' settings/world_setting.md
```

Expected: 依次输出上述 7 个一级章节及其二、三级标题，不出现“域外”“铭文体系”或具体副本标题。

- [ ] **Step 3: 检查废弃设定没有残留**

Run:

```powershell
rg -n '蓝星|穿越|域外|邪神|种子|铭文|上古|黑色金属片|雷击|武道共鸣|午夜图书馆|亡者医院|厉锋|林知言' settings/world_setting.md
```

Expected: 无输出，命令退出码为 `1`。

- [ ] **Step 4: 检查核心机制全部出现**

Run:

```powershell
$terms = @(
  '毫无预警',
  '随身背包',
  '气血如龙',
  '永久强化',
  '任何副本',
  '一个规则',
  '永久覆盖',
  '永久变成普通物品',
  '现实中也能使用',
  '副本降临现实'
)
foreach ($term in $terms) {
  if (-not (Select-String -LiteralPath 'settings/world_setting.md' -SimpleMatch $term)) {
    throw "缺少核心机制：$term"
  }
}
Write-Output '核心机制检查通过'
```

Expected: 输出 `核心机制检查通过`，退出码为 `0`。

- [ ] **Step 5: 提交唯一世界观文件**

```powershell
git add -- rust/wupo-guize/settings/world_setting.md
git diff --cached --check -- rust/wupo-guize/settings/world_setting.md
git commit --only -m "docs: reset world setting around rule artifacts" -- rust/wupo-guize/settings/world_setting.md
```

Expected: 提交只包含 `settings/world_setting.md`。

### Task 2: 删除旧人物与旧副本设定

**Files:**
- Delete: `settings/character_card.md`
- Delete: `settings/outline_golden_three.md`
- Delete: `settings/outline_course_1.md`

- [ ] **Step 1: 删除三份已废弃文件**

使用补丁删除以下文件的全部内容及文件本身：

```text
settings/character_card.md
settings/outline_golden_three.md
settings/outline_course_1.md
```

- [ ] **Step 2: 验证 `settings/` 只保留总设定**

Run:

```powershell
$files = @(Get-ChildItem -File -LiteralPath 'settings' | Select-Object -ExpandProperty Name)
if ($files.Count -ne 1 -or $files[0] -ne 'world_setting.md') {
  throw "settings 文件不符合预期：$($files -join ', ')"
}
Write-Output 'settings 文件结构检查通过'
```

Expected: 输出 `settings 文件结构检查通过`，退出码为 `0`。

- [ ] **Step 3: 对整个设定目录扫描旧内容**

Run:

```powershell
rg -n '午夜图书馆|亡者医院|厉锋|林知言|域外邪神|播种势力|黑色金属片|武道共鸣' settings
```

Expected: 无输出，命令退出码为 `1`。

- [ ] **Step 4: 提交旧文件清理**

```powershell
git add -A -- rust/wupo-guize/settings
git diff --cached --check -- rust/wupo-guize/settings
git commit --only -m "docs: remove obsolete character and dungeon settings" -- rust/wupo-guize/settings
```

Expected: 提交只包含三份旧文件的删除；如果旧文件在执行前仍未被 Git 跟踪，则 Git 不会产生删除提交，此时记录“旧文件已从工作区移除”并继续最终验证。

### Task 3: 最终一致性验证

**Files:**
- Verify: `settings/world_setting.md`
- Verify: `settings/`

- [ ] **Step 1: 检查 Markdown 与工作区差异**

Run:

```powershell
git diff --check -- rust/wupo-guize/settings rust/wupo-guize/docs/superpowers
```

Expected: 无空白错误，退出码为 `0`。

- [ ] **Step 2: 运行完整设定验收脚本**

Run:

```powershell
$settingPath = 'settings/world_setting.md'
$files = @(Get-ChildItem -File -LiteralPath 'settings')
if ($files.Count -ne 1 -or $files[0].Name -ne 'world_setting.md') {
  throw 'settings 目录必须只包含 world_setting.md'
}

$required = @(
  '毫无预警',
  '气血武道',
  '气血如龙',
  '永久强化物品',
  '规则法器',
  '一个规则刻录槽',
  '永久覆盖',
  '掠夺式抽取',
  '副本降临现实'
)
$forbidden = @(
  '域外邪神',
  '播种势力',
  '黑色金属片',
  '武道共鸣',
  '午夜图书馆',
  '亡者医院',
  '厉锋',
  '林知言'
)

$content = Get-Content -Raw -LiteralPath $settingPath
foreach ($term in $required) {
  if (-not $content.Contains($term)) { throw "缺少：$term" }
}
foreach ($term in $forbidden) {
  if ($content.Contains($term)) { throw "仍有旧设定：$term" }
}
Write-Output '规则法器版世界观验收通过'
```

Expected: 输出 `规则法器版世界观验收通过`，退出码为 `0`。

- [ ] **Step 3: 审阅最终差异和最近提交**

Run:

```powershell
git status --short
git log -3 --oneline --decorate
```

Expected: `settings/` 不再包含待处理的旧文件；其他原有工作区改动仍被保留且未混入本任务提交。
