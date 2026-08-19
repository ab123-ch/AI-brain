pub mod plugin_manager;
pub mod skill_loader;

pub use plugin_manager::{PluginManager, PluginMeta, PluginSource};
pub use skill_loader::{
    project_skill_roots, SkillCatalog, SkillCatalogResolver, SkillMeta, SkillPackMeta, SkillRoot,
};
