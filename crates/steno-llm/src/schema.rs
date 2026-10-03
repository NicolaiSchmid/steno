//! A request-side JSON Schema builder limited to the strict subset.
//! Swift: `Sources/StenoLLM/StructuredOutput/JSONSchema.swift`.

use serde_json::{Map, Value};

/// A request-side JSON Schema builder limited to what every structured
/// output implementation accepts (OpenAI's strict subset): objects with
/// every property required and `additionalProperties: false`, arrays,
/// strings with optional `enum`, integers, numbers, booleans, and nullable
/// variants as `"type": [..., "null"]`. No `format`, `pattern`, bounds or
/// `anyOf`. Validation of the model's answer is done by decoding into the
/// draft types, never by this schema.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonSchema {
    node: Node,
    description: Option<String>,
    nullable: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Object(Vec<(String, JsonSchema)>),
    Array(Box<JsonSchema>),
    String(Option<Vec<String>>),
    Integer,
    Number,
    Boolean,
}

impl JsonSchema {
    fn new(node: Node) -> Self {
        JsonSchema {
            node,
            description: None,
            nullable: false,
        }
    }

    /// An object with `properties` in this order, every one required.
    #[must_use]
    pub fn object(properties: Vec<(&str, JsonSchema)>) -> Self {
        JsonSchema::new(Node::Object(
            properties
                .into_iter()
                .map(|(name, schema)| (name.to_owned(), schema))
                .collect(),
        ))
    }

    #[must_use]
    pub fn array(item: JsonSchema) -> Self {
        JsonSchema::new(Node::Array(Box::new(item)))
    }

    #[must_use]
    pub fn string() -> Self {
        JsonSchema::new(Node::String(None))
    }

    #[must_use]
    pub fn string_enum(cases: &[&str]) -> Self {
        JsonSchema::new(Node::String(Some(
            cases.iter().map(|case| (*case).to_owned()).collect(),
        )))
    }

    #[must_use]
    pub fn integer() -> Self {
        JsonSchema::new(Node::Integer)
    }

    #[must_use]
    pub fn number() -> Self {
        JsonSchema::new(Node::Number)
    }

    #[must_use]
    pub fn boolean() -> Self {
        JsonSchema::new(Node::Boolean)
    }

    /// The same schema with a description (a trailing comment in the prompt
    /// form).
    #[must_use]
    pub fn described(mut self, description: &str) -> Self {
        self.description = Some(description.to_owned());
        self
    }

    /// The same schema accepting `null`.
    #[must_use]
    pub fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }

    /// The property names of an object schema, in order; empty otherwise.
    #[must_use]
    pub fn property_names(&self) -> Vec<&str> {
        match &self.node {
            Node::Object(properties) => properties.iter().map(|(name, _)| name.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    // Wire form

    /// The schema as sent in `response_format.json_schema.schema`.
    #[must_use]
    pub fn json_value(&self) -> Value {
        let mut object = Map::new();
        let type_name = match &self.node {
            Node::Object(properties) => {
                object.insert(
                    "properties".to_owned(),
                    Value::Object(
                        properties
                            .iter()
                            .map(|(name, schema)| (name.clone(), schema.json_value()))
                            .collect(),
                    ),
                );
                object.insert(
                    "required".to_owned(),
                    Value::Array(
                        properties
                            .iter()
                            .map(|(name, _)| Value::String(name.clone()))
                            .collect(),
                    ),
                );
                object.insert("additionalProperties".to_owned(), Value::Bool(false));
                "object"
            }
            Node::Array(item) => {
                object.insert("items".to_owned(), item.json_value());
                "array"
            }
            Node::String(cases) => {
                if let Some(cases) = cases {
                    object.insert(
                        "enum".to_owned(),
                        Value::Array(cases.iter().cloned().map(Value::String).collect()),
                    );
                }
                "string"
            }
            Node::Integer => "integer",
            Node::Number => "number",
            Node::Boolean => "boolean",
        };
        let type_value = if self.nullable {
            Value::Array(vec![
                Value::String(type_name.to_owned()),
                Value::String("null".to_owned()),
            ])
        } else {
            Value::String(type_name.to_owned())
        };
        object.insert("type".to_owned(), type_value);
        if let Some(description) = &self.description {
            object.insert("description".to_owned(), Value::String(description.clone()));
        }
        Value::Object(object)
    }

    // Prompt form

    /// A compact, readable shape for the prompt: JSON with type names in
    /// place of values, `"a" | "b"` for enums, `| null` for nullable,
    /// descriptions as trailing comments. Shorter than the schema and
    /// easier for small models than JSON Schema itself.
    #[must_use]
    pub fn prompt_text(&self) -> String {
        self.render(0)
    }

    fn render(&self, indent: usize) -> String {
        let pad = "  ".repeat(indent);
        let inner = "  ".repeat(indent + 1);
        let mut text = match &self.node {
            Node::Object(properties) => {
                let compact = properties.len() <= 3
                    && properties
                        .iter()
                        .all(|(_, schema)| schema.is_scalar() && schema.description.is_none());
                if compact {
                    let fields: Vec<String> = properties
                        .iter()
                        .map(|(name, schema)| format!("\"{name}\": {}", schema.render(indent)))
                        .collect();
                    format!("{{ {} }}", fields.join(", "))
                } else {
                    let mut lines = vec!["{".to_owned()];
                    for (offset, (name, schema)) in properties.iter().enumerate() {
                        let mut line = format!("{inner}\"{name}\": {}", schema.render(indent + 1));
                        if offset + 1 < properties.len() {
                            line.push(',');
                        }
                        if let Some(description) = &schema.description {
                            line.push_str("  // ");
                            line.push_str(description);
                        }
                        lines.push(line);
                    }
                    lines.push(format!("{pad}}}"));
                    lines.join("\n")
                }
            }
            Node::Array(item) => format!("[{}]", item.render(indent)),
            Node::String(Some(cases)) => cases
                .iter()
                .map(|case| format!("\"{case}\""))
                .collect::<Vec<_>>()
                .join(" | "),
            Node::String(None) => "string".to_owned(),
            Node::Integer => "integer".to_owned(),
            Node::Number => "number".to_owned(),
            Node::Boolean => "boolean".to_owned(),
        };
        if self.nullable {
            text.push_str(" | null");
        }
        text
    }

    fn is_scalar(&self) -> bool {
        !matches!(self.node, Node::Object(_) | Node::Array(_))
    }
}
