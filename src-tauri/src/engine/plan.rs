//! The implementation plan produced by the analysis step (structured output).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::sandbox::CommandSpec;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    /// programming | website | database | document | presentation | other
    pub kind: String,
    /// e.g. cpp, python, rust, csharp, java, php, javascript, html, sql, markdown
    pub language: String,
    pub technology: String,
    pub summary: String,
    pub deliverables: Vec<String>,
    /// Paths (relative to the project root) that must exist when done.
    pub required_files: Vec<String>,
    pub expected_output: String,
    pub dependency_commands: Vec<CommandSpec>,
    pub build_commands: Vec<CommandSpec>,
    pub test_commands: Vec<CommandSpec>,
    /// files | link | both | unknown — what the teacher expects to receive.
    pub submission_expects: String,
    pub submission_notes: String,
    pub ambiguities: Vec<String>,
    pub steps: Vec<String>,
}

fn cmd_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "program": {"type": "string", "description": "Executable name only, e.g. g++, python, cargo, dotnet, npm"},
            "args": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["program", "args"],
        "additionalProperties": false
    })
}

pub fn commands_schema() -> Value {
    json!({"type": "array", "items": cmd_schema()})
}

pub fn schema() -> Value {
    let s = |d: &str| json!({"type": "string", "description": d});
    let list = |d: &str| json!({"type": "array", "items": {"type": "string"}, "description": d});
    json!({
        "type": "object",
        "properties": {
            "kind": {"type": "string", "enum": ["programming", "website", "database", "document", "presentation", "other"]},
            "language": s("Primary language id: rust, cpp, c, csharp, java, python, php, javascript, typescript, html, sql, markdown, docx, pptx"),
            "technology": s("Frameworks/tools, e.g. 'C++17, g++' or 'HTML5 + CSS3'"),
            "summary": s("One paragraph: what must be created"),
            "deliverables": list("Concrete deliverables"),
            "required_files": list("Relative paths that must exist in the finished project, including README.md"),
            "expected_output": s("What running/opening the result should show"),
            "dependency_commands": commands_schema(),
            "build_commands": commands_schema(),
            "test_commands": commands_schema(),
            "submission_expects": {"type": "string", "enum": ["files", "link", "both", "unknown"]},
            "submission_notes": s("What the teacher asked to be handed in, quoting the assignment if possible"),
            "ambiguities": list("Unclear points in the assignment and the assumption taken"),
            "steps": list("Ordered implementation steps")
        },
        "required": ["kind", "language", "technology", "summary", "deliverables", "required_files", "expected_output",
                     "dependency_commands", "build_commands", "test_commands", "submission_expects", "submission_notes",
                     "ambiguities", "steps"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_strict_and_matches_struct() {
        let s = schema();
        let props = s["properties"].as_object().unwrap();
        let required: Vec<&str> = s["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(props.len(), required.len(), "strict structured output needs every property required");
        // A minimal instance of the schema must deserialize into Plan.
        let mut inst = serde_json::Map::new();
        for (k, v) in props {
            inst.insert(k.clone(), if v["type"] == "array" { json!([]) } else { json!("x") });
        }
        let _: Plan = serde_json::from_value(Value::Object(inst)).unwrap();
    }
}
