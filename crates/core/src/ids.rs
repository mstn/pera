use std::fmt::{Display, Formatter};

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct RunId(Uuid);

impl RunId {
    pub fn new(value: Uuid) -> Self {
        Self(value)
    }

    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn parse_str(value: &str) -> Result<Self, uuid::Error> {
        Uuid::parse_str(value).map(Self)
    }

    pub fn get(self) -> Uuid {
        self.0
    }

    pub fn as_hyphenated(&self) -> String {
        self.0.hyphenated().to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct ActionId(Uuid);

impl ActionId {
    pub fn new(value: Uuid) -> Self {
        Self(value)
    }

    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn parse_str(value: &str) -> Result<Self, uuid::Error> {
        Uuid::parse_str(value).map(Self)
    }

    pub fn get(self) -> Uuid {
        self.0
    }

    pub fn as_hyphenated(&self) -> String {
        self.0.hyphenated().to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct CodeArtifactId(Uuid);

impl CodeArtifactId {
    pub fn new(value: Uuid) -> Self {
        Self(value)
    }

    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn parse_str(value: &str) -> Result<Self, uuid::Error> {
        Uuid::parse_str(value).map(Self)
    }

    pub fn get(self) -> Uuid {
        self.0
    }

    pub fn as_hyphenated(&self) -> String {
        self.0.hyphenated().to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct InputName(String);

impl InputName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct ActionName(String);

impl ActionName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for RunId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_hyphenated())
    }
}

impl Display for ActionId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_hyphenated())
    }
}

impl Display for CodeArtifactId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_hyphenated())
    }
}
