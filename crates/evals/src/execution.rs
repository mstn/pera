use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct EvalProjectLayout {
    pub root: PathBuf,
    pub evals_dir: PathBuf,
    pub catalog_dir: PathBuf,
    pub cache_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PreparedCatalogSkill {
    pub skill_name: String,
    pub profile_name: String,
    pub compiled_dir: PathBuf,
    pub catalog_dir: PathBuf,
    pub compiled_now: bool,
    pub uploaded_now: bool,
}

#[derive(Debug, Clone)]
pub struct EvalPreparation {
    pub project: EvalProjectLayout,
    pub skills: Vec<PreparedCatalogSkill>,
}
