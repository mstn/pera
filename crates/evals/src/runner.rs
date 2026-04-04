use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pera_skills::{
    CompiledSkill, FileSystemProjectHost, InstalledSkill, ProjectHost, SkillProvisioner,
    UvxComponentizer,
};

use crate::error::EvalError;
use crate::execution::{EvalPreparation, EvalProjectLayout, PreparedCatalogSkill};
use crate::spec::{EvalCatalogSkillSpec, EvalSkillSourceSpec, EvalSpec};

#[derive(Debug, Clone)]
pub struct EvalRunner {
    uvx: String,
}

impl Default for EvalRunner {
    fn default() -> Self {
        Self {
            uvx: "uvx".to_owned(),
        }
    }
}

impl EvalRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn prepare(&self, spec: &EvalSpec) -> Result<EvalPreparation, EvalError> {
        eprintln!(
            "[eval] ensuring shared project layout root={}",
            spec.runtime.output_folder.display()
        );
        let provisioner =
            SkillProvisioner::new(FileSystemProjectHost, UvxComponentizer::new(&self.uvx));
        let project = provisioner
            .ensure_project_layout(&spec.runtime.output_folder)
            .map_err(EvalError::from)?;
        let source_map = spec
            .runtime
            .skill_sources
            .iter()
            .map(|source| (source.id.clone(), source.clone()))
            .collect::<BTreeMap<_, _>>();

        let mut skills = Vec::new();
        for catalog_skill in &spec.runtime.catalog {
            eprintln!(
                "[eval] preparing catalog skill skill={} source={} profile={}",
                catalog_skill.skill,
                catalog_skill.source,
                catalog_skill.profile.as_deref().unwrap_or("<default>")
            );
            let prepared = self
                .prepare_catalog_skill(&project.root, &source_map, catalog_skill)
                .await?;
            eprintln!(
                "[eval] catalog skill ready skill={} version={} compiled_now={} uploaded_now={}",
                prepared.skill_name,
                prepared.skill_version,
                prepared.compiled_now,
                prepared.uploaded_now
            );
            skills.push(prepared);
        }

        Ok(EvalPreparation {
            project: EvalProjectLayout {
                root: project.root,
                evals_dir: spec.runtime.output_folder.join("evals"),
                catalog_dir: project.catalog_dir,
                cache_dir: project.cache_dir,
            },
            skills,
        })
    }

    async fn prepare_catalog_skill(
        &self,
        project_root: &std::path::Path,
        source_map: &BTreeMap<String, EvalSkillSourceSpec>,
        entry: &EvalCatalogSkillSpec,
    ) -> Result<PreparedCatalogSkill, EvalError> {
        let provisioner =
            SkillProvisioner::new(FileSystemProjectHost, UvxComponentizer::new(&self.uvx));
        let source = source_map.get(&entry.source).ok_or_else(|| {
            EvalError::InvalidSpec(format!(
                "skill '{}' references unknown source '{}'",
                entry.skill, entry.source
            ))
        })?;
        let skill_dir = source.path.join(&entry.skill);
        eprintln!(
            "[eval] ensuring catalog skill from {}",
            skill_dir.display()
        );
        let installed = provisioner
            .ensure_catalog_skill(&skill_dir, entry.profile.as_deref(), project_root)
            .await
            .map_err(EvalError::from)?;

        Ok(PreparedCatalogSkill {
            skill_name: installed.compiled.skill_name,
            skill_version: installed.compiled.skill_version,
            profile_name: installed.compiled.profile_name,
            compiled_dir: installed.compiled.compiled_dir,
            catalog_dir: installed.catalog_dir,
            compiled_now: installed.compiled.compiled_now,
            uploaded_now: installed.uploaded_now,
        })
    }

    pub fn prepare_run_workspace(
        &self,
        preparation: &EvalPreparation,
        run_dir: &Path,
    ) -> Result<PathBuf, EvalError> {
        let host = FileSystemProjectHost;
        let workspace_root = run_dir.join("project");
        eprintln!(
            "[eval] preparing run workspace root={}",
            workspace_root.display()
        );
        host.create_dir_all(&workspace_root).map_err(EvalError::from)?;

        let shared_catalog = preparation.project.root.join("catalog");
        let shared_cache = preparation.project.root.join("cache");
        let workspace_catalog = workspace_root.join("catalog");
        let workspace_cache = workspace_root.join("cache");

        if !host.exists(&workspace_catalog) {
            eprintln!(
                "[eval] linking shared catalog {} -> {}",
                shared_catalog.display(),
                workspace_catalog.display()
            );
            host.symlink_dir(&shared_catalog, &workspace_catalog)
                .map_err(EvalError::from)?;
        }
        if !host.exists(&workspace_cache) {
            eprintln!(
                "[eval] linking shared cache {} -> {}",
                shared_cache.display(),
                workspace_cache.display()
            );
            host.symlink_dir(&shared_cache, &workspace_cache)
                .map_err(EvalError::from)?;
        }

        let provisioner =
            SkillProvisioner::new(FileSystemProjectHost, UvxComponentizer::new(&self.uvx));
        let _ = provisioner
            .ensure_project_layout(&workspace_root)
            .map_err(EvalError::from)?;
        for skill in &preparation.skills {
            eprintln!(
                "[eval] resetting seeded state for skill={} profile={}",
                skill.skill_name,
                skill.profile_name
            );
            let installed = InstalledSkill {
                compiled: CompiledSkill {
                    skill_name: skill.skill_name.clone(),
                    skill_version: skill.skill_version.clone(),
                    profile_name: skill.profile_name.clone(),
                    compiled_dir: skill.compiled_dir.clone(),
                    compiled_now: skill.compiled_now,
                },
                catalog_dir: skill.catalog_dir.clone(),
                uploaded_now: skill.uploaded_now,
            };
            provisioner
                .reset_installed_skill_state(&workspace_root, &installed, None)
                .map_err(EvalError::from)?;
        }
        eprintln!("[eval] run workspace ready");

        Ok(workspace_root)
    }
}
