//! Version tags for the `schema` field of versioned machine documents.
//!
//! Each tag is a zero-sized type that serializes to its constant (e.g. `kiss.jev-context.v1`) and
//! refuses to deserialize from anything else, so a document of the wrong kind or version cannot be
//! mistaken for the right one.

macro_rules! schema_tag {
    ($(#[$doc:meta])* $name:ident = $value:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub struct $name;

        impl $name {
            pub const VALUE: &'static str = $value;
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str($value)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let found = String::deserialize(deserializer)?;
                if found == $value {
                    Ok($name)
                } else {
                    Err(<D::Error as ::serde::de::Error>::custom(format!(
                        "expected schema `{}`, found `{found}`",
                        $value
                    )))
                }
            }
        }
    };
}

schema_tag!(
    /// `kiss.jev-request.v1`
    JevRequestSchema = "kiss.jev-request.v1"
);
schema_tag!(
    /// `kiss.jev-context.v1`
    JevContextSchema = "kiss.jev-context.v1"
);
schema_tag!(
    /// `kiss.inference-ir.v1`
    InferenceIrSchema = "kiss.inference-ir.v1"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_serialize_to_their_constant_and_reject_other_versions() {
        assert_eq!(serde_json::to_string(&JevContextSchema).unwrap(), "\"kiss.jev-context.v1\"");
        assert!(serde_json::from_str::<JevContextSchema>("\"kiss.jev-context.v1\"").is_ok());
        assert!(serde_json::from_str::<JevContextSchema>("\"kiss.jev-context.v2\"").is_err());
        assert!(serde_json::from_str::<JevContextSchema>("\"kiss.jev-request.v1\"").is_err());
    }
}
