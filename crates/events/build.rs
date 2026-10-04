#![allow(clippy::disallowed_macros, clippy::disallowed_methods)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use prost_types::field_descriptor_proto::{Label, Type};
use prost_types::{DescriptorProto, FileDescriptorSet};
use protox::prost::Message;

const PACKAGE: &str = ".mix.events.v1.";

fn upper_camel(name: &str) -> String {
    name.split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (index, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

enum Kind {
    Enum(String),
    Message,
    Other,
}

fn kind(
    field: &prost_types::FieldDescriptorProto,
    enums: &BTreeSet<String>,
    messages: &BTreeMap<String, &DescriptorProto>,
) -> Kind {
    let name = field.type_name();
    match field.r#type() {
        Type::Enum if enums.contains(name) => {
            Kind::Enum(name.trim_start_matches(PACKAGE).to_string())
        }
        Type::Message if messages.contains_key(name) => Kind::Message,
        _ => Kind::Other,
    }
}

fn normalize(descriptors: &FileDescriptorSet) -> String {
    let file = descriptors
        .file
        .iter()
        .find(|file| file.package() == PACKAGE.trim_matches('.'))
        .expect("the events package is compiled");
    let enums: BTreeSet<String> = file
        .enum_type
        .iter()
        .map(|e| format!("{PACKAGE}{}", e.name()))
        .collect();
    let messages: BTreeMap<String, &DescriptorProto> = file
        .message_type
        .iter()
        .map(|m| (format!("{PACKAGE}{}", m.name()), m))
        .collect();
    let mut out = String::new();
    for enumeration in &file.enum_type {
        let values: Vec<String> = enumeration
            .value
            .iter()
            .map(|value| value.number())
            .filter(|number| *number != 0)
            .map(|number| number.to_string())
            .collect();
        writeln!(
            out,
            "impl {} {{ pub const DEFINED: &'static [i32] = &[{}]; }}",
            enumeration.name(),
            values.join(", ")
        )
        .expect("writing to a string");
    }
    for message in &file.message_type {
        assert!(
            message.nested_type.is_empty() && message.enum_type.is_empty(),
            "{} nests a type, which normalize does not walk yet",
            message.name()
        );
        let mut body = String::new();
        for field in &message.field {
            let real_oneof = field.oneof_index.is_some() && !field.proto3_optional();
            if real_oneof {
                continue;
            }
            let name = field.name();
            let repeated = field.label() == Label::Repeated;
            let optional = field.proto3_optional() || field.r#type() == Type::Message;
            match (kind(field, &enums, &messages), repeated, optional) {
                (Kind::Enum(e), true, _) => writeln!(
                    body,
                    "for v in &mut self.{name} {{ if {e}::try_from(*v).is_err() {{ *v = 0; }} }}"
                ),
                (Kind::Enum(e), false, true) => writeln!(
                    body,
                    "if let Some(v) = &mut self.{name} && {e}::try_from(*v).is_err() {{ *v = 0; }}"
                ),
                (Kind::Enum(e), false, false) => writeln!(
                    body,
                    "if {e}::try_from(self.{name}).is_err() {{ self.{name} = 0; }}"
                ),
                (Kind::Message, true, _) => {
                    writeln!(body, "for v in &mut self.{name} {{ v.normalize(); }}")
                }
                (Kind::Message, false, _) => {
                    writeln!(
                        body,
                        "if let Some(v) = &mut self.{name} {{ v.normalize(); }}"
                    )
                }
                (Kind::Other, _, _) => Ok(()),
            }
            .expect("writing to a string");
        }
        for (index, oneof) in message.oneof_decl.iter().enumerate() {
            let members: Vec<_> = message
                .field
                .iter()
                .filter(|f| f.oneof_index == Some(index as i32) && !f.proto3_optional())
                .collect();
            if members.is_empty() {
                continue;
            }
            let path = format!("{}::{}", snake(message.name()), upper_camel(oneof.name()));
            writeln!(
                body,
                "if let Some(o) = &mut self.{} {{ match o {{",
                oneof.name()
            )
            .expect("writing to a string");
            for field in members {
                let variant = upper_camel(field.name());
                match kind(field, &enums, &messages) {
                    Kind::Enum(e) => writeln!(
                        body,
                        "{path}::{variant}(v) => if {e}::try_from(*v).is_err() {{ *v = 0; }},"
                    ),
                    Kind::Message => writeln!(body, "{path}::{variant}(v) => v.normalize(),"),
                    Kind::Other => writeln!(body, "{path}::{variant}(_) => {{}}"),
                }
                .expect("writing to a string");
            }
            body.push_str("} }\n");
        }
        writeln!(
            out,
            "impl crate::Normalize for {} {{ fn normalize(&mut self) {{ {body} }} }}",
            message.name()
        )
        .expect("writing to a string");
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");
    let descriptors = protox::compile(
        [
            "mix/events/v1/events.proto",
            "mix/capture/v1/capture.proto",
            "mix/result/v1/result.proto",
        ],
        ["proto"],
    )?;
    let encoded = descriptors.encode_to_vec();
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    std::fs::write(
        out_dir.join("mix.events.v1.normalize.rs"),
        normalize(&descriptors),
    )?;

    prost_build::Config::new()
        .compile_well_known_types()
        .extern_path(".google.protobuf", "::pbjson_types")
        .boxed(".mix.events.v1.NodeFinished.diagnostic")
        .boxed(".mix.events.v1.Diagnostic.detail.conflict")
        .boxed(".mix.events.v1.Diagnostic.detail.unit")
        // Keeps Command, and so every NodeStarted, from growing with each bootstrap option.
        .boxed(".mix.events.v1.Command.request.bootstrap")
        .compile_fds(descriptors)?;

    pbjson_build::Builder::new()
        .register_descriptors(&encoded)?
        .ignore_unknown_fields()
        .ignore_unknown_enum_variants()
        .build(&[".mix.events.v1", ".mix.capture.v1"])?;

    pbjson_build::Builder::new()
        .register_descriptors(&encoded)?
        .ignore_unknown_fields()
        .ignore_unknown_enum_variants()
        .emit_fields()
        .build(&[".mix.result.v1"])?;

    Ok(())
}
