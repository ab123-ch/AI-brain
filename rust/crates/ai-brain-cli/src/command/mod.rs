pub mod builtin;
pub mod config_cmd;
pub mod evolver_cmd;
pub mod mcp_cmd;
pub mod memory_cmd;
pub mod persona_cmd;
pub mod plugin_cmd;
pub mod registry;
pub mod skill_cmd;

pub use builtin::*;
pub use registry::*;

/// Build a fully-populated command registry by merging all command groups.
pub fn build_full_registry() -> CommandRegistry {
    let mut reg = builtin::register_builtin();
    reg.merge(config_cmd::register_config());
    reg.merge(plugin_cmd::register_plugin());
    reg.merge(skill_cmd::register_skill());
    reg.merge(mcp_cmd::register_mcp());
    reg.merge(memory_cmd::register_memory());
    reg.merge(evolver_cmd::register_evolver());
    reg.merge(persona_cmd::register_persona());
    reg
}
