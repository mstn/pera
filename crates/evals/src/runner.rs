use std::collections::BTreeMap;

use pera_skills::{FileSystemProjectHost, SkillProvisioner, UvxComponentizer};

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
            let prepared = self
                .prepare_catalog_skill(&project.root, &source_map, catalog_skill)
                .await?;
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
        let installed = provisioner
            .ensure_catalog_skill(&skill_dir, entry.profile.as_deref(), project_root)
            .await
            .map_err(EvalError::from)?;

        Ok(PreparedCatalogSkill {
            skill_name: installed.compiled.skill_name,
            profile_name: installed.compiled.profile_name,
            compiled_dir: installed.compiled.compiled_dir,
            catalog_dir: installed.catalog_dir,
            compiled_now: installed.compiled.compiled_now,
            uploaded_now: installed.uploaded_now,
        })
    }
}
