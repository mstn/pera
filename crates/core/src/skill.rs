#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillManifest {
    pub schema_version: u32,
    pub skill: SkillMetadata,
    #[serde(default)]
    pub defaults: SkillDefaults,
    #[serde(default)]
    pub profiles: Vec<SkillProfileManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillMetadata {
    pub name: String,
    pub version: SkillVersion,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SkillVersion(pub String);

impl SkillVersion {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl SkillManifest {
    pub fn databases_for_profile<'a>(
        &'a self,
        profile: &'a SkillProfileManifest,
    ) -> &'a [SkillDatabaseSpec] {
        if profile.databases.is_empty() {
            &self.defaults.databases
        } else {
            &profile.databases
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct SkillDefaults {
    #[serde(default)]
    pub instructions: Option<SkillInstructionsSpec>,
    #[serde(default)]
    pub databases: Vec<SkillDatabaseSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillInstructionsSpec {
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillDatabaseSpec {
    pub name: String,
    pub engine: String,
    #[serde(default)]
    pub migrations: Option<SkillDatabaseMigrationsSpec>,
    #[serde(default)]
    pub seeds: Option<SkillDatabaseSeedsSpec>,
    #[serde(default)]
    pub on_load: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillDatabaseMigrationsSpec {
    pub dir: String,
    #[serde(default)]
    pub table: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillDatabaseSeedsSpec {
    pub dir: String,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillProfileManifest {
    pub name: String,
    #[serde(default)]
    pub default: bool,
    pub runtime: SkillRuntimeManifest,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub databases: Vec<SkillDatabaseSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillRuntimeManifest {
    pub kind: SkillRuntimeKind,
    #[serde(default)]
    pub wasm: Option<WasmSkillRuntimeSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SkillRuntimeKind(pub String);

impl SkillRuntimeKind {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WasmSkillRuntimeSpec {
    pub wit: WasmSkillInterfaceSpec,
    pub artifacts: SkillRuntimeArtifactSpec,
    #[serde(default)]
    pub build: Option<WasmSkillBuildSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WasmSkillInterfaceSpec {
    pub path: String,
    pub world: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillRuntimeArtifactSpec {
    pub dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WasmSkillBuildSpec {
    pub tool: String,
    pub module: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillBuildSpec {
    pub tool: String,
    pub module: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDescription {
    pub name: String,
    pub version: SkillVersion,
    pub profile_count: usize,
}
