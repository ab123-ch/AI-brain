use std::path::Path;

use brain_core::tool_executor::{
    CommandSyntax as BrainCommandSyntax, ResolvedCommandBackend as BrainCommandBackend,
    ResolvedCommandExecution as BrainCommandExecution, ToolExecutionContext,
};
use runtime::{
    validate_command_working_directory, CommandSyntax as RuntimeCommandSyntax, ConfigLoader,
    HostPlatform, ResolvedCommandBackend as RuntimeCommandBackend,
    ResolvedCommandExecution as RuntimeCommandExecution,
};

pub(crate) fn tool_execution_context_for_directory(
    working_directory: &Path,
) -> Result<ToolExecutionContext, String> {
    let config = ConfigLoader::default_for(working_directory)
        .load()
        .map_err(|error| format!("加载命令执行配置失败: {error}"))?;
    let resolved = config
        .command_execution()
        .resolve(HostPlatform::current())
        .map_err(|error| format!("解析命令执行配置失败: {error}"))?;
    validate_command_working_directory(&resolved, working_directory)
        .map_err(|error| format!("命令执行工作目录不可用: {error}"))?;
    let command_execution = brain_command_execution(resolved)?;
    command_execution.validate_for_current_host()?;
    Ok(ToolExecutionContext::with_command_execution(
        working_directory,
        command_execution,
    ))
}

pub(crate) fn validate_tool_execution_context(
    context: &ToolExecutionContext,
) -> Result<(), String> {
    context.command_execution.validate_for_current_host()?;
    let execution = runtime_command_execution(&context.command_execution)?;
    validate_command_working_directory(&execution, &context.working_directory)
        .map_err(|error| format!("命令执行工作目录不可用: {error}"))
}

fn brain_command_execution(
    execution: RuntimeCommandExecution,
) -> Result<BrainCommandExecution, String> {
    execution
        .validate()
        .map_err(|error| format!("校验命令执行配置失败: {error}"))?;
    Ok(BrainCommandExecution {
        backend: match execution.backend {
            RuntimeCommandBackend::Wsl => BrainCommandBackend::Wsl,
            RuntimeCommandBackend::Powershell => BrainCommandBackend::Powershell,
            RuntimeCommandBackend::Sh => BrainCommandBackend::Sh,
        },
        syntax: match execution.syntax {
            RuntimeCommandSyntax::Posix => BrainCommandSyntax::Posix,
            RuntimeCommandSyntax::Powershell => BrainCommandSyntax::Powershell,
        },
        host_os: if execution.host_platform == HostPlatform::Other
            && HostPlatform::current() == HostPlatform::Other
        {
            std::env::consts::OS.to_string()
        } else {
            execution.host_platform.as_str().to_string()
        },
        wsl_distribution: execution.wsl_distribution,
        wsl_user: execution.wsl_user,
    })
}

fn runtime_command_execution(
    execution: &BrainCommandExecution,
) -> Result<RuntimeCommandExecution, String> {
    let host_platform = match execution.host_os.as_str() {
        "windows" => HostPlatform::Windows,
        "macos" => HostPlatform::Macos,
        "linux" => HostPlatform::Linux,
        "other" => HostPlatform::Other,
        host if HostPlatform::current() == HostPlatform::Other && host == std::env::consts::OS => {
            HostPlatform::Other
        }
        host => return Err(format!("不支持的冻结命令宿主: {host}")),
    };
    Ok(RuntimeCommandExecution {
        backend: match execution.backend {
            BrainCommandBackend::Wsl => RuntimeCommandBackend::Wsl,
            BrainCommandBackend::Powershell => RuntimeCommandBackend::Powershell,
            BrainCommandBackend::Sh => RuntimeCommandBackend::Sh,
        },
        syntax: match execution.syntax {
            BrainCommandSyntax::Posix => RuntimeCommandSyntax::Posix,
            BrainCommandSyntax::Powershell => RuntimeCommandSyntax::Powershell,
        },
        host_platform,
        wsl_distribution: execution.wsl_distribution.clone(),
        wsl_user: execution.wsl_user.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_runtime_wsl_descriptor_without_re_resolving_it() {
        let converted = brain_command_execution(RuntimeCommandExecution {
            backend: RuntimeCommandBackend::Wsl,
            syntax: RuntimeCommandSyntax::Posix,
            host_platform: HostPlatform::Windows,
            wsl_distribution: Some("Ubuntu-24.04".into()),
            wsl_user: Some("brain".into()),
        })
        .expect("valid WSL descriptor");

        assert_eq!(converted.backend, BrainCommandBackend::Wsl);
        assert_eq!(converted.syntax, BrainCommandSyntax::Posix);
        assert_eq!(converted.host_os, "windows");
        assert_eq!(converted.wsl_distribution.as_deref(), Some("Ubuntu-24.04"));
        assert_eq!(converted.wsl_user.as_deref(), Some("brain"));
    }

    #[test]
    fn loads_directory_scoped_command_execution_settings() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".claw")).unwrap();
        std::fs::write(
            directory.path().join(".claw/settings.local.json"),
            r#"{"commandExecution":{"backend":"sh"}}"#,
        )
        .unwrap();

        let context = tool_execution_context_for_directory(directory.path()).unwrap();

        assert_eq!(context.working_directory, directory.path());
        assert_eq!(context.command_execution.backend, BrainCommandBackend::Sh);
        assert_eq!(context.command_execution.syntax, BrainCommandSyntax::Posix);
        assert_eq!(context.command_execution.host_os, std::env::consts::OS);
    }

    #[test]
    fn maps_the_current_other_platform_without_losing_its_host_identity() {
        if HostPlatform::current() != HostPlatform::Other {
            return;
        }
        let converted = brain_command_execution(RuntimeCommandExecution {
            backend: RuntimeCommandBackend::Sh,
            syntax: RuntimeCommandSyntax::Posix,
            host_platform: HostPlatform::Other,
            wsl_distribution: None,
            wsl_user: None,
        })
        .unwrap();

        assert_eq!(converted.host_os, std::env::consts::OS);
        converted.validate_for_current_host().unwrap();
        assert_eq!(
            runtime_command_execution(&converted).unwrap().host_platform,
            HostPlatform::Other
        );
    }
}
