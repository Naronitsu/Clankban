//! Agent workflows shipped inside the binary. The MCP bridge serves them as
//! prompts, and the board's "Agent runs" view sends them to a connected agent.

use serde_json::{json, Value};

pub struct SkillArg {
    pub name: &'static str,
    pub description: &'static str,
    pub required: bool,
}

pub struct Skill {
    pub name: &'static str,
    pub description: &'static str,
    pub args: &'static [SkillArg],
    pub template: &'static str,
}

pub const SKILLS: &[Skill] = &[
    Skill {
        name: "import_tasks",
        description: "Import work items from notes or a list into the board. Shows an inventory and waits for approval before creating anything.",
        args: &[SkillArg {
            name: "source",
            description: "Pasted notes, a list, or file contents to import",
            required: true,
        }],
        template: include_str!("../skills/import_tasks.md"),
    },
    Skill {
        name: "update_tasks",
        description: "Change existing items. Reads each item first, shows the planned changes, and applies them after approval.",
        args: &[SkillArg {
            name: "changes",
            description: "Which items to change and how",
            required: true,
        }],
        template: include_str!("../skills/update_tasks.md"),
    },
    Skill {
        name: "review_tasks",
        description: "Read-only review of board health: overdue, stuck, orphaned, and duplicate items.",
        args: &[SkillArg {
            name: "scope",
            description: "Optional status or area to focus on",
            required: false,
        }],
        template: include_str!("../skills/review_tasks.md"),
    },
];

pub fn find(name: &str) -> Option<&'static Skill> {
    SKILLS.iter().find(|s| s.name == name)
}

fn non_empty_arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Returns the first required argument that is missing or blank.
pub fn missing_required(skill: &Skill, args: &Value) -> Option<&'static str> {
    skill
        .args
        .iter()
        .find(|a| a.required && non_empty_arg(args, a.name).is_none())
        .map(|a| a.name)
}

/// Fills the template with the supplied arguments. Optional arguments that
/// were left out render as "(not provided)".
pub fn render(skill: &Skill, args: &Value) -> String {
    let mut text = skill.template.to_string();
    for arg in skill.args {
        let value = non_empty_arg(args, arg.name).unwrap_or("(not provided)");
        text = text.replace(&format!("{{{{{}}}}}", arg.name), value);
    }
    text
}

/// Registry as JSON for the board UI and other clients.
pub fn describe_all() -> Value {
    Value::Array(
        SKILLS
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "description": s.description,
                    "arguments": s.args.iter().map(|a| json!({
                        "name": a.name,
                        "description": a.description,
                        "required": a.required,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_three_unique_skills() {
        let names: Vec<&str> = SKILLS.iter().map(|s| s.name).collect();
        assert_eq!(names, ["import_tasks", "update_tasks", "review_tasks"]);
    }

    #[test]
    fn every_template_renders_without_placeholders_left() {
        for skill in SKILLS {
            let mut args = serde_json::Map::new();
            for arg in skill.args {
                args.insert(arg.name.to_string(), json!("value"));
            }
            let text = render(skill, &Value::Object(args));
            assert!(!text.contains("{{"), "{} has an unfilled placeholder", skill.name);
        }
    }

    #[test]
    fn missing_required_argument_is_reported() {
        let skill = find("import_tasks").unwrap();
        assert_eq!(missing_required(skill, &json!({ "source": "   " })), Some("source"));
        assert_eq!(missing_required(skill, &json!({ "source": "notes" })), None);
    }

    #[test]
    fn optional_argument_renders_as_not_provided() {
        let skill = find("review_tasks").unwrap();
        assert!(render(skill, &json!({})).contains("(not provided)"));
    }
}
