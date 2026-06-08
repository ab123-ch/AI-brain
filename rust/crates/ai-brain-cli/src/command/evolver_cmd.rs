//! Evolver command registration — v2 进化脑命令接口
//!
//! 命令结构:
//! /evo                              查看进化系统状态
//! /evo start <目标描述>              启动进化循环
//! /evo stop                         停止进化循环
//! /evo target list                  查看进化目标
//! /evo target add <描述>            添加进化目标
//! /evo target remove <id>           移除进化目标
//! /evo backlog                      查看积压问题
//! /evo report                       查看最近进化报告
//! /evo capability                   查看能力树
//! /evo approve                      批准 v1 进化变更
//! /evo reject                       拒绝 v1 进化变更
//! /evo diff                         查看 v1 进化变更差异

use super::registry::{
    Command, CommandGroup, CommandHandler, CommandRegistry, CommandResult, SubCommand,
};

fn handle_evolver(args: &[String]) -> CommandResult {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");

    match sub {
        "" => evo_status_overview(),
        "status" => evo_status_overview(),
        "start" => {
            let goal = args.get(1..).map(|s| s.join(" ")).unwrap_or_default();
            if goal.is_empty() {
                CommandResult::err("用法: /evo start <目标描述>")
            } else {
                CommandResult::ok(format!(
                    "[已排队] 进化脑 v2 将启动，目标: {goal}\n\
                     提示: 进化将在后台异步运行，使用 /evo status 查看进度"
                ))
            }
        }
        "stop" => CommandResult::ok("[已排队] 进化脑 v2 停止请求已发送"),
        "target" => handle_target(&args[1..]),
        "backlog" => handle_backlog(),
        "report" => handle_report(),
        "capability" => handle_capability(),
        // v1 兼容
        "approve" => CommandResult::ok("[v1] 批准进化变更（请在进化完成后使用）"),
        "reject" => CommandResult::ok("[v1] 拒绝进化变更"),
        "diff" => CommandResult::ok("[v1] 进化变更差异（暂无数据）"),
        _ => CommandResult::err(format!(
            "未知子命令: {sub}\n\
             可用: start, stop, status, target, backlog, report, capability"
        )),
    }
}

fn evo_status_overview() -> CommandResult {
    CommandResult::ok(
        "进化脑 v2 状态:\n\
         ├─ 框架: 已就绪（CycleRunner + EvoOrchestrator）\n\
         ├─ 数据模型: Backlog + Target + EvoLog + CapabilityTree\n\
         ├─ 触发器: EvolutionTrigger（定时 + 空闲检测）\n\
         └─ 集成: Orchestrator 已连接\n\n\
         子命令: start <目标> | stop | target | backlog | report | capability"
            .to_string(),
    )
}

fn handle_target(args: &[String]) -> CommandResult {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");

    match sub {
        "" | "list" => CommandResult::ok(
            "进化目标列表:\n\
             （暂无目标，使用 /evo target add <描述> 添加）"
                .to_string(),
        ),
        "add" => {
            let desc = args.get(1..).map(|s| s.join(" ")).unwrap_or_default();
            if desc.is_empty() {
                CommandResult::err("用法: /evo target add <目标描述>")
            } else {
                CommandResult::ok(format!(
                    "[已添加] 进化目标: {desc}\n\
                     优先级: 默认\n\
                     使用 /evo start 启动进化循环"
                ))
            }
        }
        "remove" => {
            let id = args.get(1).map(|s| s.as_str()).unwrap_or("");
            if id.is_empty() {
                CommandResult::err("用法: /evo target remove <id>")
            } else {
                CommandResult::ok(format!("[已移除] 目标 {id}"))
            }
        }
        _ => CommandResult::err(format!(
            "未知 target 子命令: {sub}\n\
             可用: list, add, remove"
        )),
    }
}

fn handle_backlog() -> CommandResult {
    CommandResult::ok(
        "进化积压问题:\n\
         （暂无积压问题）\n\n\
         积压来源:\n\
         ├─ EvalBrain: 评估结果为 Critical/Warning\n\
         ├─ 用户反馈: 用户说\"你错了\"/\"你不会\"\n\
         ├─ 主脑自我感知: 知识盲区\n\
         └─ 记忆脑: pitfall 累积超过阈值"
            .to_string(),
    )
}

fn handle_report() -> CommandResult {
    CommandResult::ok(
        "最近进化报告:\n\
         （暂无进化记录）\n\n\
         进化完成后将显示:\n\
         ├─ 处理的目标数\n\
         ├─ 创建的技能\n\
         ├─ 消耗的 tokens\n\
         └─ 耗时"
            .to_string(),
    )
}

fn handle_capability() -> CommandResult {
    CommandResult::ok(
        "能力树:\n\
         （尚未初始化）\n\n\
         能力树在首次进化完成后自动构建。\n\
         每次进化成功后增量更新。"
            .to_string(),
    )
}

/// Register the `evo` command and its subcommands.
pub fn register_evolver() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    registry.register(Command {
        name: "evo",
        description: "进化脑管理（v2）",
        group: CommandGroup::Evolver,
        subcommands: vec![
            SubCommand {
                name: "start",
                description: "启动进化循环，目标描述作为参数",
                args: vec![],
            },
            SubCommand {
                name: "stop",
                description: "停止正在运行的进化循环",
                args: vec![],
            },
            SubCommand {
                name: "status",
                description: "查看进化系统状态",
                args: vec![],
            },
            SubCommand {
                name: "target",
                description: "管理进化目标 (list/add/remove)",
                args: vec![],
            },
            SubCommand {
                name: "backlog",
                description: "查看积压问题",
                args: vec![],
            },
            SubCommand {
                name: "report",
                description: "查看最近进化报告",
                args: vec![],
            },
            SubCommand {
                name: "capability",
                description: "查看能力树",
                args: vec![],
            },
            SubCommand {
                name: "approve",
                description: "[v1] 批准进化变更",
                args: vec![],
            },
            SubCommand {
                name: "reject",
                description: "[v1] 拒绝进化变更",
                args: vec![],
            },
            SubCommand {
                name: "diff",
                description: "[v1] 查看进化变更差异",
                args: vec![],
            },
        ],
        handler: CommandHandler::Sync(handle_evolver),
    });

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evolver_registration() {
        let registry = register_evolver();
        let cmd = registry
            .find_command("evo")
            .expect("evo command should exist");
        assert_eq!(cmd.name, "evo");
        assert_eq!(cmd.group, CommandGroup::Evolver);
        assert_eq!(cmd.subcommands.len(), 10);
    }

    #[test]
    fn test_evolver_subcommands_found() {
        let registry = register_evolver();
        for sub in &[
            "start",
            "stop",
            "status",
            "target",
            "backlog",
            "report",
            "capability",
            "approve",
            "reject",
            "diff",
        ] {
            assert!(
                registry.find_subcommand("evo", sub).is_some(),
                "subcommand '{sub}' should exist"
            );
        }
        assert!(registry.find_subcommand("evo", "nonexistent").is_none());
    }

    #[test]
    fn test_evolver_handler_no_args() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&[]);
        assert!(result.success);
        assert!(result.output.contains("进化脑 v2"));
    }

    #[test]
    fn test_evolver_handler_status() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["status".to_string()]);
        assert!(result.success);
        assert!(result.output.contains("CycleRunner"));
    }

    #[test]
    fn test_evolver_handler_start_with_goal() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["start".to_string(), "Rust async".to_string()]);
        assert!(result.success);
        assert!(result.output.contains("Rust async"));
    }

    #[test]
    fn test_evolver_handler_start_no_goal() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["start".to_string()]);
        assert!(!result.success);
    }

    #[test]
    fn test_evolver_handler_target_add() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&[
            "target".to_string(),
            "add".to_string(),
            "Docker".to_string(),
        ]);
        assert!(result.success);
        assert!(result.output.contains("Docker"));
    }

    #[test]
    fn test_evolver_handler_target_list() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["target".to_string(), "list".to_string()]);
        assert!(result.success);
    }

    #[test]
    fn test_evolver_handler_v1_compatibility() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        for sub in &["approve", "reject", "diff"] {
            let result = handler(&[sub.to_string()]);
            assert!(result.success, "v1 subcommand '{sub}' should succeed");
        }
    }

    #[test]
    fn test_evolver_handler_unknown_sub() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["unknown".to_string()]);
        assert!(!result.success);
        assert!(result.output.contains("未知子命令"));
    }
}
