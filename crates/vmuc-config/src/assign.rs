// SPDX-License-Identifier: Apache-2.0
//! `vmuc create --set path=value`: assignments to an instance config before
//! it is written. A path is dotted keys (`network.mode`), the value is
//! YAML (`8GiB`, `true`, `[{location: ~/src, writable: true}]`), and the
//! result goes through the schema like any config file, so a mistyped key
//! or value is reported the same way.

use serde_yaml::{Mapping, Value};

use crate::schema::InstanceConfig;
use crate::{Error, Result};

impl InstanceConfig {
    /// This config with each `path=value` applied in order.
    pub fn with_assignments(&self, assignments: &[String]) -> Result<Self> {
        let mut doc: Value =
            serde_yaml::from_str(&self.to_yaml()?).map_err(|source| Error::Yaml {
                origin: "the template".into(),
                source,
            })?;
        for a in assignments {
            assign(&mut doc, a)?;
        }
        let text = serde_yaml::to_string(&doc).map_err(Error::YamlSerialize)?;
        Self::from_yaml(&text, "--set")
    }
}

fn invalid(assignment: &str, reason: impl Into<String>) -> Error {
    Error::InvalidAssignment {
        assignment: assignment.to_owned(),
        reason: reason.into(),
    }
}

fn assign(doc: &mut Value, assignment: &str) -> Result<()> {
    let (path, raw) = assignment
        .split_once('=')
        .ok_or_else(|| invalid(assignment, "expected path=value"))?;
    let keys: Vec<&str> = path.trim().split('.').collect();
    if keys.iter().any(|k| k.is_empty()) {
        return Err(invalid(assignment, "empty key in the path"));
    }
    let value: Value =
        serde_yaml::from_str(raw).map_err(|e| invalid(assignment, format!("value: {e}")))?;
    let (last, parents) = keys
        .split_last()
        .ok_or_else(|| invalid(assignment, "no key"))?;
    let mut cur = doc;
    for k in parents {
        if cur.is_null() {
            *cur = Value::Mapping(Mapping::new());
        }
        let map = cur.as_mapping_mut().ok_or_else(|| {
            invalid(
                assignment,
                format!("{k:?} is inside something that is not a mapping"),
            )
        })?;
        cur = map
            .entry(Value::String((*k).to_owned()))
            .or_insert(Value::Null);
    }
    if cur.is_null() {
        *cur = Value::Mapping(Mapping::new());
    }
    let map = cur.as_mapping_mut().ok_or_else(|| {
        invalid(
            assignment,
            format!("{last:?} is inside something that is not a mapping"),
        )
    })?;
    map.insert(Value::String((*last).to_owned()), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(base: &str, a: &[&str]) -> Result<InstanceConfig> {
        let cfg = InstanceConfig::from_yaml(base, "test").unwrap();
        cfg.with_assignments(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn scalars_nested_keys_and_lists() {
        let c = set(
            "cpus: 2\n",
            &[
                "cpus=8",
                "memory=8GiB",
                "network.mode=bridged",
                "network.interface=en0",
                "mounts=[{location: /tmp/src, writable: true}]",
            ],
        )
        .unwrap();
        let y = c.to_yaml().unwrap();
        for want in [
            "cpus: 8",
            "memory: 8GiB",
            "mode: bridged",
            "interface: en0",
            "location: /tmp/src",
        ] {
            assert!(y.contains(want), "{want:?} missing from\n{y}");
        }
        assert!(
            !set("cpus: 2\n", &["cpus="])
                .unwrap()
                .to_yaml()
                .unwrap()
                .contains("cpus"),
            "empty unsets"
        );
    }

    #[test]
    fn mistakes_are_reported_not_ignored() {
        for (bad, why) in [
            ("cpus", "path=value"),
            ("network..mode=x", "empty key"),
            ("cpus.count=2", "not a mapping"),
            ("cpuz=2", "--set"),
            ("cpus=lots", "--set"),
        ] {
            let e = set("cpus: 2\n", &[bad]).unwrap_err().to_string();
            assert!(e.contains(why), "{bad}: {e}");
        }
    }
}
