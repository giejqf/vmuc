// SPDX-License-Identifier: Apache-2.0
use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::de::{self, Deserialize, Deserializer, Visitor};

use super::*;
use crate::schema::{
    ImageRef, InstanceConfig, KernelRef, MemoryPolicy, Mount, Network, PortForward, Probe,
    Provision, Ssh, User,
};

/// A deserializer that only records the field names a struct asks for.
struct Introspect<'a>(&'a mut Vec<&'static str>);

impl<'de> Deserializer<'de> for Introspect<'_> {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
        Err(de::Error::custom("not a struct"))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.extend_from_slice(fields);
        Err(de::Error::custom("introspected"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes
        byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map enum
        identifier ignored_any
    }
}

/// The schema's own field names for `T`, as serde sees them.
fn fields_of<'de, T: Deserialize<'de>>() -> Vec<&'static str> {
    let mut out = Vec::new();
    let _ = T::deserialize(Introspect(&mut out));
    assert!(
        !out.is_empty(),
        "{} did not introspect",
        std::any::type_name::<T>()
    );
    out
}

#[test]
fn the_table_lists_exactly_the_schemas_keys() {
    let groups: [(&str, Vec<&str>); 11] = [
        ("", fields_of::<InstanceConfig>()),
        ("images[].", fields_of::<ImageRef>()),
        ("images[].kernel.", fields_of::<KernelRef>()),
        ("mounts[].", fields_of::<Mount>()),
        ("ssh.", fields_of::<Ssh>()),
        ("network.", fields_of::<Network>()),
        ("portForwards[].", fields_of::<PortForward>()),
        ("provision[].", fields_of::<Provision>()),
        ("probes[].", fields_of::<Probe>()),
        ("user.", fields_of::<User>()),
        ("memoryPolicy.", fields_of::<MemoryPolicy>()),
    ];
    let schema: BTreeSet<String> = groups
        .iter()
        .flat_map(|(prefix, names)| names.iter().map(move |n| format!("{prefix}{n}")))
        .collect();
    let table: BTreeSet<String> = fields().map(|f| f.key.to_owned()).collect();
    let missing: Vec<_> = schema.difference(&table).collect();
    let stale: Vec<_> = table.difference(&schema).collect();
    assert!(
        missing.is_empty(),
        "schema keys without documentation: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "documented keys the schema does not have: {stale:?}"
    );
}

#[test]
fn the_rendered_reference_is_a_valid_empty_config() {
    let cfg = InstanceConfig::from_yaml(&render_yaml(), "reference").unwrap();
    assert_eq!(cfg, InstanceConfig::default());
}

/// The checked-in files are what the table renders. `VMUC_BLESS=1 cargo
/// test -p vmuc-config reference` rewrites them.
#[test]
fn checked_in_files_match_the_table() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (path, text) in [
        ("guest/templates/reference.yaml", render_yaml()),
        ("docs/config.md", render_markdown()),
    ] {
        let path = root.join(path);
        if std::env::var_os("VMUC_BLESS").is_some() {
            std::fs::write(&path, &text).unwrap();
            continue;
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            have == text,
            "{} is out of date; run `VMUC_BLESS=1 cargo test -p vmuc-config reference`",
            path.display()
        );
    }
}
