//! Compact level-behaviour projection of gameplay facts. Geometry is validated
//! and skipped instead of retaining millions of JSON nodes on headless startup.
//! Scalar defaults and duplicate-key handling match the original Value loader.
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{fmt, marker::PhantomData};

// Skipped data still goes through normal scalar decoding so numeric overflow
// and depth errors match the original JSON loader without allocating a tree.
#[derive(Default)]
pub(super) struct Validate;
impl<'de> Container<'de> for Validate {}
impl<'de> Deserialize<'de> for Validate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        container(d)
    }
}

trait Container<'de>: Default {
    fn map<A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        while map.next_key::<serde::de::IgnoredAny>()?.is_some() {
            map.next_value::<Validate>()?;
        }
        Ok(Self::default())
    }
    fn seq<A: SeqAccess<'de>>(mut seq: A) -> Result<Self, A::Error> {
        while seq.next_element::<Validate>()?.is_some() {}
        Ok(Self::default())
    }
}
struct ContainerVisitor<T>(PhantomData<T>);
impl<'de, T: Container<'de>> Visitor<'de> for ContainerVisitor<T> {
    type Value = T;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("JSON facts")
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
        T::map(map)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<T, A::Error> {
        T::seq(seq)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<T, E> {
        Ok(T::default())
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<T, E> {
        Ok(T::default())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<T, E> {
        Ok(T::default())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<T, E> {
        Ok(T::default())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<T, E> {
        Ok(T::default())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<T, E> {
        Ok(T::default())
    }
}
fn container<'de, D: Deserializer<'de>, T: Container<'de>>(d: D) -> Result<T, D::Error> {
    d.deserialize_any(ContainerVisitor(PhantomData))
}
macro_rules! read_field {
    ($map:ident, typed) => {
        $map.next_value()?
    };
    ($map:ident, value($convert:expr)) => {
        ($convert)($map.next_value::<Value>()?)
    };
}
macro_rules! object {
    ($name:ident<$life:lifetime> { $($field:ident: $ty:ty = $default:expr => $kind:ident $(($convert:expr))?),* $(,)? }) => {
        pub(super) struct $name<$life> {
            $(pub $field: $ty,)*
            marker: PhantomData<&$life ()>,
        }
        impl Default for $name<'_> {
            fn default() -> Self { Self { $($field: $default,)* marker: PhantomData } }
        }
        impl<$life> Container<$life> for $name<$life> {
            fn map<A: MapAccess<$life>>(mut map: A) -> Result<Self, A::Error> {
                #[derive(Deserialize)]
                #[allow(non_camel_case_types)]
                #[serde(field_identifier)]
                enum Field { $($field,)* #[serde(other)] Unknown }
                let mut result = Self::default();
                while let Some(field) = map.next_key::<Field>()? {
                    match field {
                        $(Field::$field => result.$field = read_field!(map, $kind $(($convert))?),)*
                        Field::Unknown => { map.next_value::<Validate>()?; }
                    }
                }
                Ok(result)
            }
        }
        impl<$life> Deserialize<$life> for $name<$life> {
            fn deserialize<D: Deserializer<$life>>(d: D) -> Result<Self, D::Error> { container(d) }
        }
    };
}
fn flag(v: Value) -> bool {
    v.as_bool().unwrap_or(false)
}
fn text(v: Value) -> Option<String> {
    if let Value::String(s) = v {
        Some(s)
    } else {
        None
    }
}

#[derive(Default)]
pub(super) struct Entries<'a>(pub Option<Vec<Entry<'a>>>);
impl<'de> Container<'de> for Entries<'de> {
    fn seq<A: SeqAccess<'de>>(mut seq: A) -> Result<Self, A::Error> {
        let mut entries = Vec::new();
        while let Some(entry) = seq.next_element()? {
            entries.push(entry);
        }
        Ok(Self(Some(entries)))
    }
}
impl<'de> Deserialize<'de> for Entries<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        container(d)
    }
}
object!(Gameplay<'a> { states: Entries<'a> = Entries::default() => typed });
object!(SoundId<'a> { id: Option<String> = None => value(text) });
object!(Sound<'a> {
    volume: f64 = 1.0 => value(|v: Value| v.as_f64().unwrap_or(1.0)),
    pitch: f64 = 1.0 => value(|v: Value| v.as_f64().unwrap_or(1.0)),
    r#break: SoundId<'a> = SoundId::default() => typed,
    place: SoundId<'a> = SoundId::default() => typed,
    fall: SoundId<'a> = SoundId::default() => typed,
});
object!(Entry<'a> {
    emission: Value = Value::Null => typed,
    dampening: Value = Value::Null => typed,
    skylight_down: bool = false => value(flag),
    shape_occlusion: bool = false => value(flag),
    can_occlude: bool = false => value(flag),
    solid_render: bool = false => value(flag),
    hardness: Value = Value::Null => typed,
    requires_tool: bool = false => value(flag),
    replaceable: bool = false => value(flag),
    random_ticks: bool = false => value(flag),
    block_entity: bool = false => value(flag),
    liquid: bool = false => value(flag),
    ignited_by_lava: bool = false => value(flag),
    push: Option<String> = None => value(text),
    full_collision: bool = false => value(flag),
    sound: Sound<'a> = Sound::default() => typed,
});

impl super::GameplayStateSource for Entry<'_> {
    fn flag(&self, name: &str) -> bool {
        match name {
            "replaceable" => self.replaceable,
            "random_ticks" => self.random_ticks,
            "solid_render" => self.solid_render,
            "liquid" => self.liquid,
            "block_entity" => self.block_entity,
            "ignited_by_lava" => self.ignited_by_lava,
            "can_occlude" => self.can_occlude,
            "full_collision" => self.full_collision,
            "skylight_down" => self.skylight_down,
            "requires_tool" => self.requires_tool,
            "shape_occlusion" => self.shape_occlusion,
            _ => false,
        }
    }
    fn unsigned(&self, name: &str) -> u64 {
        match name {
            "emission" => self.emission.as_u64().unwrap_or(0),
            "dampening" => self.dampening.as_u64().unwrap_or(0),
            _ => 0,
        }
    }
    fn number(&self, name: &str) -> f64 {
        match name {
            "hardness" => self.hardness.as_f64().unwrap_or(0.0),
            _ => 0.0,
        }
    }
    fn text(&self, name: &str) -> &str {
        match name {
            "push" => self.push.as_deref().unwrap_or(""),
            _ => "",
        }
    }
    fn sound_number(&self, name: &str) -> f64 {
        match name {
            "volume" => self.sound.volume,
            "pitch" => self.sound.pitch,
            _ => 1.0,
        }
    }
    fn sound_id(&self, name: &str) -> &str {
        match name {
            "place" => self.sound.place.id.as_deref().unwrap_or(""),
            "break" => self.sound.r#break.id.as_deref().unwrap_or(""),
            "fall" => self.sound.fall.id.as_deref().unwrap_or(""),
            _ => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::GameplayStateSource;

    #[test]
    fn projection_matches_json_defaults_types_and_last_duplicate() {
        let samples = [
            r#"{}"#,
            r#"null"#,
            r#"[1,true,{"unused":2}]"#,
            r#"{"replaceable":true,"replaceable":false,"hardness":2.5,"emission":12,"dampening":3,"push":"BLOCK","sound":{"volume":0.7,"pitch":1.2,"place":{"id":"place"},"break":{"id":"break"},"fall":{"id":"fall"}}}"#,
            r#"{"replaceable":"yes","hardness":null,"emission":-1,"dampening":1.5,"push":false,"sound":{"volume":null,"pitch":"high","place":{"id":3},"break":false}}"#,
            r#"{"sound":{"volume":0.3},"sound":null,"outline":[[0.0,0.0,0.0,1.0,1.0,1.0]],"outline_edges":[[0,0,0,1,0,0]]}"#,
        ];
        for sample in samples {
            let value: Value = serde_json::from_str(sample).unwrap();
            let entry: Entry<'_> = serde_json::from_str(sample).unwrap();
            for name in [
                "replaceable",
                "random_ticks",
                "solid_render",
                "liquid",
                "block_entity",
                "ignited_by_lava",
                "can_occlude",
                "full_collision",
                "skylight_down",
                "requires_tool",
                "shape_occlusion",
            ] {
                assert_eq!(entry.flag(name), value.flag(name), "{sample}: {name}");
            }
            for name in ["emission", "dampening"] {
                assert_eq!(
                    entry.unsigned(name),
                    value.unsigned(name),
                    "{sample}: {name}"
                );
            }
            assert_eq!(
                entry.number("hardness"),
                value.number("hardness"),
                "{sample}"
            );
            assert_eq!(entry.text("push"), value.text("push"), "{sample}");
            for name in ["volume", "pitch"] {
                assert_eq!(
                    entry.sound_number(name),
                    value.sound_number(name),
                    "{sample}: {name}"
                );
            }
            for name in ["place", "break", "fall"] {
                assert_eq!(
                    entry.sound_id(name),
                    value.sound_id(name),
                    "{sample}: {name}"
                );
            }
        }
    }

    #[test]
    fn skipped_geometry_is_still_validated_and_document_uses_last_states() {
        for input in [
            r#"{"states":[{"outline":[1e400]}]}"#,
            r#"{"states":[{"unused":{"a":[1,]}}]}"#,
        ] {
            assert!(serde_json::from_str::<Value>(input).is_err());
            assert!(serde_json::from_str::<Gameplay<'_>>(input).is_err());
        }
        let doc: Gameplay<'_> =
            serde_json::from_str(r#"{"states":[{"emission":1}],"states":[{"emission":7}]}"#)
                .unwrap();
        assert_eq!(doc.states.0.unwrap()[0].unsigned("emission"), 7);
        let doc: Gameplay<'_> = serde_json::from_str(r#"{"states":null}"#).unwrap();
        assert!(doc.states.0.is_none());
    }

    #[test]
    #[ignore = "requires HORIZON_TEST_RESOURCES pointing to prepared vanilla resources"]
    fn prepared_states_match_the_original_json_projection() {
        let root = std::env::var("HORIZON_TEST_RESOURCES").expect("prepared resources path");
        let bytes = std::fs::read(std::path::Path::new(&root).join("gameplay-facts.json")).unwrap();
        let original: Value = serde_json::from_slice(&bytes).unwrap();
        let compact: Gameplay<'_> = serde_json::from_slice(&bytes).unwrap();
        let original = original["states"].as_array().unwrap();
        let compact = compact.states.0.unwrap();
        assert_eq!(original.len(), compact.len());
        for (id, (old, new)) in original.iter().zip(&compact).enumerate() {
            for name in [
                "replaceable",
                "random_ticks",
                "solid_render",
                "liquid",
                "block_entity",
                "ignited_by_lava",
                "can_occlude",
                "full_collision",
                "skylight_down",
                "requires_tool",
                "shape_occlusion",
            ] {
                assert_eq!(old.flag(name), new.flag(name), "state {id}: {name}");
            }
            for name in ["emission", "dampening"] {
                assert_eq!(old.unsigned(name), new.unsigned(name), "state {id}: {name}");
            }
            assert_eq!(old.number("hardness"), new.number("hardness"), "state {id}");
            assert_eq!(old.text("push"), new.text("push"), "state {id}");
            for name in ["volume", "pitch"] {
                assert_eq!(
                    old.sound_number(name),
                    new.sound_number(name),
                    "state {id}: {name}"
                );
            }
            for name in ["place", "break", "fall"] {
                assert_eq!(old.sound_id(name), new.sound_id(name), "state {id}: {name}");
            }
        }
    }
}
