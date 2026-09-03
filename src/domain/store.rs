//! 内存键值数据及其业务规则实现。

use std::collections::{BTreeMap, HashMap};

use super::{Command, DomainError, Reply, SetOutcome, StoreStatus};

/// 内存中的领域模型。
///
/// Store 只处理键值数据和业务规则，不感知命令行、TCP、WAL 或快照文件。
#[derive(Debug, Default)]
pub struct Store {
    entries: HashMap<String, String>,
}

impl Store {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn execute(&mut self, command: Command) -> Result<Reply, DomainError> {
        match command {
            Command::Set { key, value } => self.set(key, value).map(Reply::Set),
            Command::Update { key, value } => self
                .update(&key, value)
                .map(|old_value| Reply::Updated { old_value }),
            Command::Get { key } => self.get(&key).map(|value| Reply::Value(value.to_owned())),
            Command::Delete { key } => self.delete(&key).map(|value| Reply::Deleted { value }),
            Command::Keys => Ok(Reply::Keys(self.keys())),
            Command::Status => Ok(Reply::Status(self.status())),
        }
    }

    /// 在不修改数据的情况下验证命令。
    ///
    /// Engine 会在写 WAL 之前调用此方法，因此失败的 UPDATE/DELETE 不会进入日志。
    pub fn validate(&self, command: &Command) -> Result<(), DomainError> {
        match command {
            Command::Set { key, .. } => Self::validate_key(key),
            Command::Update { key, .. } | Command::Get { key } | Command::Delete { key } => {
                Self::validate_key(key)?;
                if self.entries.contains_key(key) {
                    Ok(())
                } else {
                    Err(DomainError::NotFound { key: key.clone() })
                }
            }
            Command::Keys | Command::Status => Ok(()),
        }
    }

    pub fn set(&mut self, key: String, value: String) -> Result<SetOutcome, DomainError> {
        Self::validate_key(&key)?;

        Ok(match self.entries.insert(key, value) {
            Some(old_value) => SetOutcome::Overwritten { old_value },
            None => SetOutcome::Created,
        })
    }

    pub fn update(&mut self, key: &str, value: String) -> Result<String, DomainError> {
        Self::validate_key(key)?;

        let stored_value = self
            .entries
            .get_mut(key)
            .ok_or_else(|| DomainError::NotFound {
                key: key.to_owned(),
            })?;

        Ok(std::mem::replace(stored_value, value))
    }

    pub fn get(&self, key: &str) -> Result<&str, DomainError> {
        Self::validate_key(key)?;

        self.entries
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| DomainError::NotFound {
                key: key.to_owned(),
            })
    }

    pub fn delete(&mut self, key: &str) -> Result<String, DomainError> {
        Self::validate_key(key)?;

        self.entries
            .remove(key)
            .ok_or_else(|| DomainError::NotFound {
                key: key.to_owned(),
            })
    }

    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.entries.keys().cloned().collect();
        keys.sort();
        keys
    }

    #[must_use]
    pub fn status(&self) -> StoreStatus {
        StoreStatus {
            key_count: self.entries.len(),
        }
    }

    /// 导出有序数据，用于生成内容稳定、便于阅读和校验的 JSON 快照。
    #[must_use]
    pub fn export_entries(&self) -> BTreeMap<String, String> {
        self.entries
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// 从已经完成格式与校验和验证的快照数据恢复 Store。
    #[must_use]
    pub fn from_entries(entries: BTreeMap<String, String>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    fn validate_key(key: &str) -> Result<(), DomainError> {
        if key.is_empty() {
            Err(DomainError::EmptyKey)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_creates_and_then_overwrites_a_value() {
        let mut store = Store::new();

        assert_eq!(
            store.set("course".into(), "Rust".into()),
            Ok(SetOutcome::Created)
        );
        assert_eq!(
            store.set("course".into(), "Advanced Rust".into()),
            Ok(SetOutcome::Overwritten {
                old_value: "Rust".into()
            })
        );
        assert_eq!(store.get("course"), Ok("Advanced Rust"));
    }

    #[test]
    fn update_requires_an_existing_key() {
        let mut store = Store::new();

        assert_eq!(
            store.update("missing", "value".into()),
            Err(DomainError::NotFound {
                key: "missing".into()
            })
        );

        store.set("course".into(), "Rust".into()).unwrap();
        assert_eq!(
            store.update("course", "Advanced Rust".into()),
            Ok("Rust".into())
        );
        assert_eq!(store.get("course"), Ok("Advanced Rust"));
    }

    #[test]
    fn delete_removes_and_returns_the_value() {
        let mut store = Store::new();
        store.set("course".into(), "Rust".into()).unwrap();

        assert_eq!(store.delete("course"), Ok("Rust".into()));
        assert_eq!(
            store.get("course"),
            Err(DomainError::NotFound {
                key: "course".into()
            })
        );
    }

    #[test]
    fn keys_are_sorted_for_deterministic_output() {
        let mut store = Store::new();
        store.set("teacher".into(), "Li".into()).unwrap();
        store.set("course".into(), "Rust".into()).unwrap();

        assert_eq!(store.keys(), vec!["course", "teacher"]);
        assert_eq!(store.status(), StoreStatus { key_count: 2 });
    }

    #[test]
    fn empty_keys_are_rejected() {
        let mut store = Store::new();

        assert_eq!(
            store.set(String::new(), "value".into()),
            Err(DomainError::EmptyKey)
        );
        assert_eq!(store.get(""), Err(DomainError::EmptyKey));
        assert_eq!(store.delete(""), Err(DomainError::EmptyKey));
    }

    #[test]
    fn execute_dispatches_domain_commands() {
        let mut store = Store::new();

        assert_eq!(
            store.execute(Command::Set {
                key: "course".into(),
                value: "Rust".into()
            }),
            Ok(Reply::Set(SetOutcome::Created))
        );
        assert_eq!(
            store.execute(Command::Get {
                key: "course".into()
            }),
            Ok(Reply::Value("Rust".into()))
        );
    }

    #[test]
    fn validation_does_not_modify_the_store() {
        let mut store = Store::new();
        store.set("course".into(), "Rust".into()).unwrap();

        assert_eq!(
            store.validate(&Command::Update {
                key: "course".into(),
                value: "Advanced Rust".into(),
            }),
            Ok(())
        );
        assert_eq!(store.get("course"), Ok("Rust"));
    }
}
