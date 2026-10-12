//! The keys a config file holds, read off its struct's own `Deserialize`
//! impl: [`shape_of`] drives the derive with a deserializer that records
//! what each field asks for instead of reading input. A key exists exactly
//! when the struct has the field, so `pm config` never keeps a list of its
//! own.

use serde::de::value::{Error, StrDeserializer};
use serde::de::{self, DeserializeOwned, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess};

/// What a config value is, as its struct field declares it. `Option`s are
/// transparent: every key is optional in the file.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// A struct: fixed keys, in declaration order.
    Table(Vec<(&'static str, Shape)>),
    /// A table whose keys the user names, every value of one shape.
    Map(Box<Shape>),
    List(Box<Shape>),
    Bool,
    Integer,
    Float,
    String,
    /// Any TOML value: another program's schema, passed through.
    Any,
}

/// The shape of `T`, a config struct.
pub fn shape_of<T: DeserializeOwned>() -> Shape {
    let mut shape = Shape::Any;
    // The trace feeds every field a placeholder, so it cannot fail on a
    // config struct; a failure would leave the fields traced so far.
    let _ = T::deserialize(Tracer { out: &mut shape });
    shape
}

impl Shape {
    /// The shape at `path` below this one: a struct field by name, a map
    /// entry by any name, and anything at all inside an [`Shape::Any`].
    pub fn at(&self, path: &[&str]) -> Option<&Shape> {
        let Some((first, rest)) = path.split_first() else {
            return Some(self);
        };
        match self {
            Shape::Table(fields) => fields
                .iter()
                .find(|(name, _)| name == first)
                .and_then(|(_, shape)| shape.at(rest)),
            Shape::Map(value) if !first.is_empty() => value.at(rest),
            Shape::Any => Some(self),
            _ => None,
        }
    }

    /// Every settable key under `prefix`, a user-named map entry written
    /// as `<agent>` under `agents`, else `<name>`.
    pub fn keys(&self, prefix: &str) -> Vec<String> {
        match self {
            Shape::Table(fields) => fields
                .iter()
                .flat_map(|(name, shape)| shape.keys(&join(prefix, name)))
                .collect(),
            Shape::Map(_) => {
                let placeholder = if prefix.starts_with("agents.") {
                    "<agent>"
                } else {
                    "<name>"
                };
                vec![join(prefix, placeholder)]
            }
            _ => vec![prefix.to_string()],
        }
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

struct Tracer<'a> {
    out: &'a mut Shape,
}

impl<'de> de::Deserializer<'de> for Tracer<'_> {
    type Error = Error;

    fn deserialize_any<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        *self.out = Shape::Any;
        visitor.visit_bool(false)
    }

    fn deserialize_bool<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        *self.out = Shape::Bool;
        visitor.visit_bool(false)
    }

    fn deserialize_i64<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        *self.out = Shape::Integer;
        visitor.visit_u64(0)
    }

    fn deserialize_u64<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_i8<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_i16<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_i32<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_u8<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_u16<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_u32<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_i64(visitor)
    }

    fn deserialize_f64<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        *self.out = Shape::Float;
        visitor.visit_f64(0.0)
    }

    fn deserialize_f32<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_f64(visitor)
    }

    fn deserialize_str<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        *self.out = Shape::String;
        visitor.visit_str("")
    }

    fn deserialize_string<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_char<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        *self.out = Shape::String;
        visitor.visit_char(' ')
    }

    fn deserialize_option<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let mut element = Shape::Any;
        let result = visitor.visit_seq(OneElement {
            out: Some(&mut element),
        });
        *self.out = Shape::List(Box::new(element));
        result
    }

    fn deserialize_map<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let mut value = Shape::Any;
        let result = visitor.visit_map(OneEntry {
            key: true,
            value: Some(&mut value),
        });
        *self.out = Shape::Map(Box::new(value));
        result
    }

    fn deserialize_struct<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        let mut traced = Vec::new();
        let result = visitor.visit_map(Fields {
            fields: fields.iter(),
            current: "",
            out: &mut traced,
        });
        *self.out = Shape::Table(traced);
        result
    }

    serde::forward_to_deserialize_any! {
        i128 u128 bytes byte_buf unit unit_struct tuple tuple_struct enum
        identifier ignored_any
    }
}

/// A sequence of one traced element.
struct OneElement<'a> {
    out: Option<&'a mut Shape>,
}

impl<'de> SeqAccess<'de> for OneElement<'_> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        match self.out.take() {
            Some(out) => seed.deserialize(Tracer { out }).map(Some),
            None => Ok(None),
        }
    }
}

/// A map of one entry with an empty-string key and a traced value.
struct OneEntry<'a> {
    key: bool,
    value: Option<&'a mut Shape>,
}

impl<'de> MapAccess<'de> for OneEntry<'_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if !std::mem::take(&mut self.key) {
            return Ok(None);
        }
        let key: StrDeserializer<'_, Error> = "".into_deserializer();
        seed.deserialize(key).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let mut scratch = Shape::Any;
        let out = self.value.take().unwrap_or(&mut scratch);
        seed.deserialize(Tracer { out })
    }
}

/// A struct's fields, each with a traced value.
struct Fields<'a> {
    fields: std::slice::Iter<'static, &'static str>,
    current: &'static str,
    out: &'a mut Vec<(&'static str, Shape)>,
}

impl<'de> MapAccess<'de> for Fields<'_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some(field) = self.fields.next() else {
            return Ok(None);
        };
        self.current = field;
        let key: StrDeserializer<'_, Error> = field.into_deserializer();
        seed.deserialize(key).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let mut shape = Shape::Any;
        let value = seed.deserialize(Tracer { out: &mut shape })?;
        self.out.push((self.current, shape));
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::BTreeMap;

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Outer {
        name: String,
        #[serde(default)]
        port: Option<u16>,
        #[serde(default)]
        hosts: Vec<String>,
        #[serde(default)]
        rows: BTreeMap<String, String>,
        #[serde(default)]
        inner: Inner,
        #[serde(default)]
        entries: BTreeMap<String, toml::Table>,
    }

    #[derive(Deserialize, Default)]
    #[allow(dead_code)]
    struct Inner {
        flag: Option<bool>,
        ratio: f64,
    }

    #[test]
    fn the_trace_records_each_fields_name_and_type() {
        let shape = shape_of::<Outer>();
        assert_eq!(shape.at(&["name"]), Some(&Shape::String));
        assert_eq!(shape.at(&["port"]), Some(&Shape::Integer));
        assert_eq!(
            shape.at(&["hosts"]),
            Some(&Shape::List(Box::new(Shape::String)))
        );
        assert_eq!(shape.at(&["rows", "anyone"]), Some(&Shape::String));
        assert_eq!(shape.at(&["inner", "flag"]), Some(&Shape::Bool));
        assert_eq!(shape.at(&["inner", "ratio"]), Some(&Shape::Float));
        assert_eq!(shape.at(&["entries", "e", "deep", "er"]), Some(&Shape::Any));
        assert_eq!(shape.at(&["inner", "missing"]), None);
        assert_eq!(shape.at(&["port", "x"]), None);
        assert_eq!(shape.at(&["rows", ""]), None);
    }

    #[test]
    fn keys_list_every_leaf_and_each_user_keyed_table_once() {
        assert_eq!(
            shape_of::<Outer>().keys(""),
            [
                "name",
                "port",
                "hosts",
                "rows.<name>",
                "inner.flag",
                "inner.ratio",
                "entries.<name>",
            ]
        );
    }

    #[test]
    fn project_name_is_a_project_only_key() {
        use crate::state::project::{GlobalConfig, ProjectConfig};
        assert_eq!(
            shape_of::<ProjectConfig>().at(&["project", "name"]),
            Some(&Shape::String)
        );
        assert_eq!(shape_of::<GlobalConfig>().at(&["project", "name"]), None);
    }
}
