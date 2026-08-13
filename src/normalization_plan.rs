use std::collections::BTreeMap;

use unicode_normalization::UnicodeNormalization;

use crate::error::AppError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub source: String,
    pub target: String,
}

pub fn normalized(value: &str) -> String {
    value.nfc().collect()
}

pub fn build(names: &[String]) -> Result<Vec<Entry>, AppError> {
    let entries = names
        .iter()
        .map(|name| Entry {
            source: name.clone(),
            target: normalized(name),
        })
        .collect::<Vec<_>>();

    let mut sources_by_target = BTreeMap::<&str, Vec<&str>>::new();
    for entry in &entries {
        sources_by_target
            .entry(&entry.target)
            .or_default()
            .push(&entry.source);
    }

    let collisions = sources_by_target
        .into_iter()
        .filter(|(_, sources)| sources.len() > 1)
        .map(|(target, _)| target.to_string())
        .collect::<Vec<_>>();
    if !collisions.is_empty() {
        return Err(AppError::Collision(collisions.join(", ")));
    }

    Ok(entries)
}
