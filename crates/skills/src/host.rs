use std::fs;
use std::path::{Path, PathBuf};

use crate::error::SkillProvisionError;

#[derive(Debug, Clone)]
pub struct ProjectEntry {
    pub path: PathBuf,
    pub file_name: String,
    pub is_dir: bool,
}

pub trait ProjectHost: Clone + Send + Sync + 'static {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, SkillProvisionError>;
    fn create_dir_all(&self, path: &Path) -> Result<(), SkillProvisionError>;
    fn read_to_string(&self, path: &Path) -> Result<String, SkillProvisionError>;
    fn read(&self, path: &Path) -> Result<Vec<u8>, SkillProvisionError>;
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), SkillProvisionError>;
    fn exists(&self, path: &Path) -> bool;
    fn remove_dir_all(&self, path: &Path) -> Result<(), SkillProvisionError>;
    fn read_dir(&self, path: &Path) -> Result<Vec<ProjectEntry>, SkillProvisionError>;
    fn copy_file(&self, source: &Path, target: &Path) -> Result<(), SkillProvisionError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FileSystemProjectHost;

impl ProjectHost for FileSystemProjectHost {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, SkillProvisionError> {
        path.canonicalize().map_err(|source| SkillProvisionError::ReadFile {
            path: path.to_path_buf(),
            source,
        })
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), SkillProvisionError> {
        fs::create_dir_all(path).map_err(|source| SkillProvisionError::CreateDir {
            path: path.to_path_buf(),
            source,
        })
    }

    fn read_to_string(&self, path: &Path) -> Result<String, SkillProvisionError> {
        fs::read_to_string(path).map_err(|source| SkillProvisionError::ReadFile {
            path: path.to_path_buf(),
            source,
        })
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, SkillProvisionError> {
        fs::read(path).map_err(|source| SkillProvisionError::ReadFile {
            path: path.to_path_buf(),
            source,
        })
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), SkillProvisionError> {
        if let Some(parent) = path.parent() {
            self.create_dir_all(parent)?;
        }
        fs::write(path, bytes).map_err(|source| SkillProvisionError::WriteFile {
            path: path.to_path_buf(),
            source,
        })
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn remove_dir_all(&self, path: &Path) -> Result<(), SkillProvisionError> {
        fs::remove_dir_all(path).map_err(|source| SkillProvisionError::CopyPath {
            source_path: path.to_path_buf(),
            target_path: path.to_path_buf(),
            source,
        })
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<ProjectEntry>, SkillProvisionError> {
        let mut entries = fs::read_dir(path)
            .map_err(|source| SkillProvisionError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| SkillProvisionError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?
            .into_iter()
            .map(|entry| {
                let entry_path = entry.path();
                let file_name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry
                    .file_type()
                    .map_err(|source| SkillProvisionError::ReadFile {
                        path: entry_path.clone(),
                        source,
                    })?
                    .is_dir();
                Ok(ProjectEntry {
                    path: entry_path,
                    file_name,
                    is_dir,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(entries)
    }

    fn copy_file(&self, source: &Path, target: &Path) -> Result<(), SkillProvisionError> {
        if let Some(parent) = target.parent() {
            self.create_dir_all(parent)?;
        }
        fs::copy(source, target).map_err(|source_err| SkillProvisionError::CopyPath {
            source_path: source.to_path_buf(),
            target_path: target.to_path_buf(),
            source: source_err,
        })?;
        Ok(())
    }
}
