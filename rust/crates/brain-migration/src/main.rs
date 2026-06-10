//! 旧数据迁移工具
//!
//! 将 ~/.ai-brain/ 下的旧格式数据迁移到新的金字塔目录结构。
//!
//! 迁移策略:
//! 1. L1: sessions/*.jsonl → personas/default/pyramid/l1-raw/*.jsonl (格式转换)
//! 2. L2/L3/L4: 不迁移，下次四步分析时 LLM 自动生成
//! 3. Profile: 重新生成(100字上限)
//! 4. EvalInfo: 合并 pitfall + evolution + eval-requirement → eval-info.json
//! 5. Persona Registry: 创建默认人格

use std::fs;
use std::path::PathBuf;

use brain_core::types::{EvolutionRule, PitfallRecord, TurnRecord};
use brain_memory::persona_types::PersonaRegistry;
use brain_memory::profile_eval::{EvalInfoStore, ProfileStore};
use brain_memory::pyramid_storage::PyramidStorage;
use brain_memory::raw_pool::RawTurn;
use chrono::Utc;

// ─── 迁移统计 ──────────────────────────────────────────────────────

#[derive(Default)]
struct MigrationStats {
    sessions_migrated: usize,
    sessions_skipped: usize,
    turns_converted: usize,
    pitfalls_merged: usize,
    rules_merged: usize,
    requirements_merged: usize,
    profile_migrated: bool,
}

// ─── 入口 ──────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let base_dir = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        dirs_home()
    };

    println!("=== 记忆金字塔数据迁移 ===");
    println!("源目录: {}", base_dir.display());
    println!();

    if !base_dir.exists() {
        eprintln!("错误: 目录不存在 {}", base_dir.display());
        std::process::exit(1);
    }

    let mut stats = MigrationStats::default();

    // Step 1: 创建默认人格（如果不存在）
    migrate_persona(&base_dir);

    // Step 2: 迁移 L1 sessions（格式转换）
    migrate_l1_sessions(&base_dir, &mut stats);

    // Step 3: 迁移 EvalInfo (pitfall + evolution + eval-requirement → eval-info.json)
    migrate_eval_info(&base_dir, &mut stats);

    // Step 4: 迁移 Profile (user_profile → profile.json)
    migrate_profile(&base_dir, &mut stats);

    // Step 5: 打印迁移摘要
    print_summary(&stats);
}

// ─── 默认目录 ──────────────────────────────────────────────────────

fn dirs_home() -> PathBuf {
    // ~/.ai-brain/
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".ai-brain")
}

// ─── Step 1: 创建默认人格 ──────────────────────────────────────────

fn migrate_persona(base_dir: &PathBuf) {
    println!("[Step 1] 创建默认人格...");
    let registry_path = base_dir.join("personas").join("registry.json");

    if registry_path.exists() {
        println!("  - 人格注册表已存在，跳过");
        return;
    }

    // 直接写入默认注册表（包含 default 人格）
    let registry = PersonaRegistry::default();
    let dir = registry_path.parent().unwrap();
    fs::create_dir_all(dir).ok();

    let json = serde_json::to_string_pretty(&registry).unwrap();
    match fs::write(&registry_path, json) {
        Ok(()) => println!("  ✓ 默认人格已创建: 智脑 (default)"),
        Err(e) => eprintln!("  ✗ 保存人格注册表失败: {e}"),
    }
}

// ─── Step 2: 迁移 L1 Sessions ─────────────────────────────────────

fn migrate_l1_sessions(base_dir: &PathBuf, stats: &mut MigrationStats) {
    println!("\n[Step 2] 迁移 L1 会话数据...");
    let old_sessions_dir = base_dir.join("sessions");
    let storage = PyramidStorage::new(base_dir.clone(), "default");

    if !old_sessions_dir.exists() {
        println!("  - 无旧会话数据，跳过");
        return;
    }

    // 确保目标目录存在
    if let Err(e) = storage.ensure_dirs() {
        eprintln!("  ✗ 创建目标目录失败: {e}");
        return;
    }

    let jsonl_files = match fs::read_dir(&old_sessions_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
            .map(|e| e.path())
            .collect::<Vec<_>>(),
        Err(e) => {
            eprintln!("  ✗ 读取 sessions 目录失败: {e}");
            return;
        }
    };

    if jsonl_files.is_empty() {
        println!("  - 无 JSONL 文件，跳过");
        return;
    }

    for old_path in &jsonl_files {
        let file_name = old_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        let session_id = old_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let new_path = storage.l1_session_path(session_id);

        // 跳过已存在的文件（幂等）
        if new_path.exists() {
            println!("  - 跳过已存在: {file_name}");
            stats.sessions_skipped += 1;
            continue;
        }

        // 读取旧行并转换
        match convert_session(old_path, &new_path) {
            Ok(count) => {
                println!("  ✓ {file_name}: {count} 条记录已转换");
                stats.sessions_migrated += 1;
                stats.turns_converted += count;
            }
            Err(e) => {
                eprintln!("  ✗ 转换失败 {file_name}: {e}");
            }
        }
    }
}

/// 将旧格式 JSONL 转换为新 RawTurn 格式
///
/// 旧格式可能有两种:
/// 1. TurnRecord: { role: TurnRole, content: String, tool_call: Option, timestamp: String }
/// 2. RawEntry: { id, content, raw_input, context: BrainContext, timestamp }
/// 3. 未知格式: 尝试当作 { role, content, timestamp } 解析
fn convert_session(old_path: &PathBuf, new_path: &PathBuf) -> Result<usize, String> {
    let data = fs::read_to_string(old_path).map_err(|e| format!("读取失败: {e}"))?;
    let mut converted = Vec::new();

    for (line_no, line) in data.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let raw_turn = if let Ok(turn) = serde_json::from_str::<TurnRecord>(trimmed) {
            // 旧格式1: TurnRecord
            let role_str = match turn.role {
                brain_core::types::TurnRole::User => "User",
                brain_core::types::TurnRole::Assistant => "Assistant",
                brain_core::types::TurnRole::ToolCall => "ToolCall",
                brain_core::types::TurnRole::ToolResult => "ToolResult",
            };
            let tool_output = turn
                .tool_call
                .map(|tc| format!("{}: {}", tc.tool_name, tc.output));
            RawTurn {
                role: role_str.to_string(),
                content: turn.content,
                tool_output,
                timestamp: Utc::now(),
            }
        } else if let Ok(flexible) = serde_json::from_str::<FlexibleTurn>(trimmed) {
            // 旧格式2/3: 灵活解析
            RawTurn {
                role: flexible.role,
                content: flexible.content,
                tool_output: flexible.tool_output,
                timestamp: flexible.timestamp.unwrap_or_else(Utc::now),
            }
        } else {
            eprintln!("    警告: 第 {} 行无法解析，跳过", line_no + 1);
            continue;
        };

        converted.push(raw_turn);
    }

    // 写入新格式
    if let Some(parent) = new_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }

    let mut out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(new_path)
        .map_err(|e| format!("打开输出文件失败: {e}"))?;

    use std::io::Write;
    for turn in &converted {
        let json_line = serde_json::to_string(turn).map_err(|e| format!("序列化失败: {e}"))?;
        writeln!(out, "{json_line}").map_err(|e| format!("写入失败: {e}"))?;
    }

    Ok(converted.len())
}

/// 灵活解析中间结构（兼容多种旧格式）
#[derive(serde::Deserialize)]
struct FlexibleTurn {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_output: Option<String>,
    #[serde(default)]
    timestamp: Option<chrono::DateTime<Utc>>,
}

// ─── Step 3: 迁移 EvalInfo ────────────────────────────────────────

fn migrate_eval_info(base_dir: &PathBuf, stats: &mut MigrationStats) {
    println!("\n[Step 3] 迁移评估信息 (EvalInfo)...");

    let storage = PyramidStorage::new(base_dir.clone(), "default");
    let eval_store = EvalInfoStore::new(storage);
    let eval_path = base_dir
        .join("personas")
        .join("default")
        .join("eval-info.json");

    // 幂等: 已存在则跳过
    if eval_path.exists() {
        println!("  - eval-info.json 已存在，跳过");
        return;
    }

    // 收集 pitfall 描述
    let pitfalls = load_old_pitfalls(base_dir);
    stats.pitfalls_merged = pitfalls.len();

    // 收集 evolution rules
    let rules = load_old_evolution_rules(base_dir);
    stats.rules_merged = rules.len();

    // 收集 eval requirements
    let requirements = load_old_eval_requirements(base_dir);
    stats.requirements_merged = requirements.len();

    if pitfalls.is_empty() && rules.is_empty() && requirements.is_empty() {
        println!("  - 无旧评估数据，跳过");
        return;
    }

    match eval_store.regenerate(requirements, pitfalls, rules) {
        Ok(()) => println!("  ✓ eval-info.json 已生成"),
        Err(e) => eprintln!("  ✗ 生成 eval-info.json 失败: {e}"),
    }
}

/// 从 memory/pitfall/*.json 加载踩坑描述
fn load_old_pitfalls(base_dir: &PathBuf) -> Vec<String> {
    let pitfall_dir = base_dir.join("memory").join("pitfall");
    load_json_files_as::<PitfallRecord>(&pitfall_dir)
        .into_iter()
        .filter(|p| !p.superseded)
        .map(|p| {
            if let Some(correction) = &p.user_correction {
                format!("{} (纠正: {})", p.description, correction)
            } else {
                p.description
            }
        })
        .collect()
}

/// 从 memory/evolution/*.json 加载进化规则
fn load_old_evolution_rules(base_dir: &PathBuf) -> Vec<String> {
    let evo_dir = base_dir.join("memory").join("evolution");
    load_json_files_as::<EvolutionRule>(&evo_dir)
        .into_iter()
        .filter(|r| !r.superseded)
        .map(|r| r.rule)
        .collect()
}

/// 从 memory/eval-requirement/*.json 加载评估要求
fn load_old_eval_requirements(base_dir: &PathBuf) -> Vec<String> {
    let req_dir = base_dir.join("memory").join("eval-requirement");

    // EvalRequirement 在 brain-memory 中定义，用灵活解析
    #[derive(serde::Deserialize)]
    struct FlexReq {
        #[serde(default)]
        content: String,
        #[serde(default)]
        superseded: bool,
    }

    load_json_files_as::<FlexReq>(&req_dir)
        .into_iter()
        .filter(|r| !r.superseded && !r.content.trim().is_empty())
        .map(|r| r.content)
        .collect()
}

/// 通用: 加载目录下所有 JSON 文件并反序列化
fn load_json_files_as<T: serde::de::DeserializeOwned>(dir: &PathBuf) -> Vec<T> {
    if !dir.exists() {
        return Vec::new();
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            let data = fs::read_to_string(e.path()).ok()?;
            serde_json::from_str::<T>(&data).ok()
        })
        .collect()
}

// ─── Step 4: 迁移 Profile ──────────────────────────────────────────

fn migrate_profile(base_dir: &PathBuf, stats: &mut MigrationStats) {
    println!("\n[Step 4] 迁移用户画像 (Profile)...");

    let storage = PyramidStorage::new(base_dir.clone(), "default");
    let profile_store = ProfileStore::new(storage);

    // 检查新 profile 是否已存在
    if profile_store.load().unwrap_or_default().is_some() {
        println!("  - profile.json 已存在，跳过");
        return;
    }

    // 读取旧 UserProfile
    let old_profile_path = base_dir
        .join("memory")
        .join("profile")
        .join("user_profile.json");

    if !old_profile_path.exists() {
        println!("  - 无旧用户画像数据，跳过");
        return;
    }

    let summary = match build_profile_summary(&old_profile_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  ✗ 读取旧画像失败: {e}");
            return;
        }
    };

    match profile_store.regenerate(&summary) {
        Ok(()) => {
            println!(
                "  ✓ profile.json 已生成 ({}/100字)",
                summary.chars().count()
            );
            stats.profile_migrated = true;
        }
        Err(e) => eprintln!("  ✗ 生成 profile.json 失败: {e}"),
    }
}

/// 从旧 UserProfile 构建简短摘要（≤100字）
fn build_profile_summary(path: &PathBuf) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct OldProfile {
        #[serde(default)]
        explicit_preferences: Vec<String>,
        #[serde(default)]
        implicit_preferences: Vec<String>,
        #[serde(default)]
        habits: Vec<String>,
    }

    let data = fs::read_to_string(path).map_err(|e| format!("读取失败: {e}"))?;
    let profile: OldProfile = serde_json::from_str(&data).map_err(|e| format!("解析失败: {e}"))?;

    // 合并所有偏好和习惯
    let mut parts = Vec::new();
    if !profile.explicit_preferences.is_empty() {
        parts.push(profile.explicit_preferences.join("、"));
    }
    if !profile.implicit_preferences.is_empty() {
        parts.push(profile.implicit_preferences.join("、"));
    }
    if !profile.habits.is_empty() {
        parts.push(format!("习惯: {}", profile.habits.join("、")));
    }

    let mut summary = parts.join("；");
    // 截断到100字
    if summary.chars().count() > 100 {
        let chars: Vec<char> = summary.chars().take(97).collect();
        summary = chars.into_iter().collect::<String>() + "...";
    }

    if summary.is_empty() {
        summary = "通用AI用户".to_string();
    }

    Ok(summary)
}

// ─── 打印迁移摘要 ──────────────────────────────────────────────────

fn print_summary(stats: &MigrationStats) {
    println!("\n=== 迁移摘要 ===");
    println!(
        "  L1 会话: {} 个已迁移, {} 个跳过",
        stats.sessions_migrated, stats.sessions_skipped
    );
    println!("  L1 记录: {} 条已转换", stats.turns_converted);
    println!(
        "  评估信息: {} 要求 + {} 踩坑 + {} 规则",
        stats.requirements_merged, stats.pitfalls_merged, stats.rules_merged
    );
    println!(
        "  用户画像: {}",
        if stats.profile_migrated {
            "已迁移"
        } else {
            "无数据"
        }
    );
    println!();
    println!("注意: L2/L3/L4 层级不迁移，将在下次四步分析时由 LLM 自动生成。");
}

// ─── 测试 ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::{PitfallCategory, ToolCallRecord, TurnRole};
    use brain_memory::pyramid_storage::PyramidStorage;
    use brain_memory::raw_pool::RawTurn;

    /// 创建模拟旧数据目录结构
    fn setup_old_data() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        // 旧 sessions 目录
        let sessions_dir = base.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        // 写入 TurnRecord 格式的 JSONL
        let turn1 = TurnRecord {
            role: TurnRole::User,
            content: "你好".into(),
            tool_call: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let turn2 = TurnRecord {
            role: TurnRole::Assistant,
            content: "你好！".into(),
            tool_call: None,
            timestamp: "2026-01-01T00:00:01Z".into(),
        };
        let turn3 = TurnRecord {
            role: TurnRole::ToolCall,
            content: "执行工具".into(),
            tool_call: Some(ToolCallRecord {
                tool_name: "Bash".into(),
                input: serde_json::json!("ls"),
                output: "file1.txt\nfile2.txt".into(),
                duration_ms: 100,
                is_error: false,
            }),
            timestamp: "2026-01-01T00:00:02Z".into(),
        };

        let sess_path = sessions_dir.join("sess-old.jsonl");
        let mut f = fs::File::create(&sess_path).unwrap();
        use std::io::Write;
        writeln!(f, "{}", serde_json::to_string(&turn1).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&turn2).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&turn3).unwrap()).unwrap();

        // 灵活格式 JSONL
        let flex_path = sessions_dir.join("sess-flex.jsonl");
        let mut f2 = fs::File::create(&flex_path).unwrap();
        let flex_line =
            r#"{"role":"User","content":"灵活格式测试","timestamp":"2026-01-01T00:00:00Z"}"#;
        writeln!(f2, "{flex_line}").unwrap();

        // 旧 pitfall 数据
        let pitfall_dir = base.join("memory").join("pitfall");
        fs::create_dir_all(&pitfall_dir).unwrap();
        let pitfall = PitfallRecord {
            id: "pit-001".into(),
            category: PitfallCategory::ToolFailure,
            description: "工具调用超时".into(),
            user_correction: Some("增加超时设置".into()),
            occurred_at: Utc::now(),
            occurrence_count: 2,
            superseded: false,
        };
        fs::write(
            pitfall_dir.join("pit-001.json"),
            serde_json::to_string_pretty(&pitfall).unwrap(),
        )
        .unwrap();

        // superseded 的 pitfall（不应被迁移）
        let pitfall_old = PitfallRecord {
            id: "pit-002".into(),
            category: PitfallCategory::Other,
            description: "旧的踩坑".into(),
            user_correction: None,
            occurred_at: Utc::now(),
            occurrence_count: 1,
            superseded: true,
        };
        fs::write(
            pitfall_dir.join("pit-002.json"),
            serde_json::to_string_pretty(&pitfall_old).unwrap(),
        )
        .unwrap();

        // 旧 evolution 数据
        let evo_dir = base.join("memory").join("evolution");
        fs::create_dir_all(&evo_dir).unwrap();
        let evo = EvolutionRule {
            id: "evo-001".into(),
            rule: "先写测试再实现".into(),
            source_pitfall_ids: vec!["pit-001".into()],
            priority: 8,
            created_at: Utc::now(),
            superseded: false,
        };
        fs::write(
            evo_dir.join("evo-001.json"),
            serde_json::to_string_pretty(&evo).unwrap(),
        )
        .unwrap();

        // 旧 user_profile
        let profile_dir = base.join("memory").join("profile");
        fs::create_dir_all(&profile_dir).unwrap();
        let profile_json = r#"{
            "explicit_preferences": ["Rust", "Tokio"],
            "implicit_preferences": ["简洁代码"],
            "taboos": ["不要GC"],
            "habits": ["TDD"],
            "updated_at": "2026-01-01T00:00:00Z"
        }"#;
        fs::write(profile_dir.join("user_profile.json"), profile_json).unwrap();

        tmp
    }

    #[test]
    fn migrate_persona_creates_default() {
        let tmp = tempfile::tempdir().unwrap();
        let base = PathBuf::from(tmp.path());
        migrate_persona(&base);

        // 验证 registry.json 已创建
        let registry_path = base.join("personas").join("registry.json");
        assert!(registry_path.exists());

        let data = fs::read_to_string(&registry_path).unwrap();
        let registry: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(registry["personas"][0]["id"], "default");
        assert_eq!(registry["personas"][0]["name"], "智脑");
    }

    #[test]
    fn migrate_persona_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let base = PathBuf::from(tmp.path());
        migrate_persona(&base);
        migrate_persona(&base); // 第二次应该跳过

        let data = fs::read_to_string(base.join("personas").join("registry.json")).unwrap();
        let registry: serde_json::Value = serde_json::from_str(&data).unwrap();
        // 只有一个 default
        assert_eq!(registry["personas"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn migrate_l1_converts_turn_record() {
        let tmp = setup_old_data();
        let base = PathBuf::from(tmp.path());
        let mut stats = MigrationStats::default();
        migrate_l1_sessions(&base, &mut stats);

        assert_eq!(stats.sessions_migrated, 2);
        assert_eq!(stats.turns_converted, 4); // 3 + 1

        // 验证新文件存在
        let storage = PyramidStorage::new(base.clone(), "default");
        let new_path = storage.l1_session_path("sess-old");
        assert!(new_path.exists());

        // 验证格式正确
        let turns: Vec<RawTurn> = storage.read_jsonl(&new_path).unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, "User");
        assert_eq!(turns[0].content, "你好");
        assert_eq!(turns[1].role, "Assistant");
        assert_eq!(turns[2].role, "ToolCall");
        assert!(turns[2].tool_output.is_some());
    }

    #[test]
    fn migrate_l1_skips_existing() {
        let tmp = setup_old_data();
        let base = PathBuf::from(tmp.path());
        let mut stats1 = MigrationStats::default();
        migrate_l1_sessions(&base, &mut stats1);

        // 第二次运行应跳过已存在的
        let mut stats2 = MigrationStats::default();
        migrate_l1_sessions(&base, &mut stats2);
        assert_eq!(stats2.sessions_migrated, 0);
        assert_eq!(stats2.sessions_skipped, 2);
    }

    #[test]
    fn migrate_eval_info_merges_data() {
        let tmp = setup_old_data();
        let base = PathBuf::from(tmp.path());
        let mut stats = MigrationStats::default();
        migrate_eval_info(&base, &mut stats);

        assert_eq!(stats.pitfalls_merged, 1); // superseded 的被过滤
        assert_eq!(stats.rules_merged, 1);

        // 验证 eval-info.json
        let storage = PyramidStorage::new(base.clone(), "default");
        let eval_store = EvalInfoStore::new(storage);
        let info = eval_store.load().unwrap().unwrap();
        assert!(!info.pitfalls.is_empty());
        assert!(info.pitfalls[0].contains("工具调用超时"));
        assert!(!info.rules.is_empty());
    }

    #[test]
    fn migrate_profile_converts_to_summary() {
        let tmp = setup_old_data();
        let base = PathBuf::from(tmp.path());
        let mut stats = MigrationStats::default();
        migrate_profile(&base, &mut stats);

        assert!(stats.profile_migrated);

        // 验证 profile.json
        let storage = PyramidStorage::new(base.clone(), "default");
        let profile_store = ProfileStore::new(storage);
        let profile = profile_store.load().unwrap().unwrap();
        assert!(!profile.summary.is_empty());
        assert!(profile.summary.contains("Rust"));
        assert!(profile.summary.chars().count() <= 100);
    }

    #[test]
    fn full_migration_end_to_end() {
        let tmp = setup_old_data();
        let base = PathBuf::from(tmp.path());
        let mut stats = MigrationStats::default();

        migrate_persona(&base);
        migrate_l1_sessions(&base, &mut stats);
        migrate_eval_info(&base, &mut stats);
        migrate_profile(&base, &mut stats);

        // 验证完整目录结构
        assert!(base.join("personas").join("registry.json").exists());
        assert!(base.join("personas").join("default").exists());
        assert!(base
            .join("personas")
            .join("default")
            .join("pyramid")
            .join("l1-raw")
            .exists());

        // 验证幂等性（再跑一次不会出错）
        let mut stats2 = MigrationStats::default();
        migrate_l1_sessions(&base, &mut stats2);
        migrate_eval_info(&base, &mut stats2);
        migrate_profile(&base, &mut stats2);
        assert_eq!(stats2.sessions_skipped, 2);
    }

    #[test]
    fn migrate_empty_dir_no_error() {
        let tmp = tempfile::tempdir().unwrap();
        let base = PathBuf::from(tmp.path());
        let mut stats = MigrationStats::default();

        // 不会 panic
        migrate_persona(&base);
        migrate_l1_sessions(&base, &mut stats);
        migrate_eval_info(&base, &mut stats);
        migrate_profile(&base, &mut stats);

        assert_eq!(stats.sessions_migrated, 0);
        assert_eq!(stats.pitfalls_merged, 0);
    }
}
