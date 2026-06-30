use std::env;
use std::path::PathBuf;

use cowboy_workflow_core::{StepId, WorkflowDefinition, WorkflowSourceRef};
use cowboy_workflow_lua::load;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: workflow-chart <workflow.lua>");
        std::process::exit(2);
    };
    if args.next().is_some() {
        eprintln!("usage: workflow-chart <workflow.lua>");
        std::process::exit(2);
    }

    let path = PathBuf::from(path).canonicalize()?;
    let root = path
        .parent()
        .ok_or("workflow path must have a parent directory")?
        .to_path_buf();
    let entry = path
        .file_name()
        .ok_or("workflow path must have a file name")?
        .to_string_lossy()
        .to_string();
    let source = WorkflowSourceRef {
        id: entry.trim_end_matches(".lua").to_string(),
        entry,
        root: Some(root.to_string_lossy().to_string()),
        description: None,
    };
    let compiled = load(&source)?;
    print_chart(&compiled.definition);
    Ok(())
}

fn print_chart(definition: &WorkflowDefinition) {
    println!("workflow {}", definition.name);
    println!("head {}", definition.head);
    println!();
    println!("roles");
    for (id, role) in &definition.roles {
        let summary = first_line(&role.instructions);
        if summary.is_empty() {
            println!("  - {id}");
        } else {
            println!("  - {id}: {summary}");
        }
    }
    println!();
    println!("steps");
    for (id, step) in &definition.steps {
        let marker = if id == &definition.head { " *" } else { "  " };
        match &step.role {
            Some(role) => println!("{marker}{id} (role: {role})"),
            None => println!("{marker}{id}"),
        }
        if step.transitions.by_status.is_empty() {
            println!("     success -> <complete>");
        } else {
            for (status, target) in &step.transitions.by_status {
                println!("     {status} -> {target}");
            }
            if !step.transitions.by_status.contains_key("success") {
                println!("     success -> <complete>");
            }
        }
    }
    println!();
    println!("mermaid");
    println!("flowchart TD");
    for (id, step) in &definition.steps {
        if step.transitions.by_status.is_empty() {
            print_end_edge(id, "success");
        } else {
            for (status, target) in &step.transitions.by_status {
                println!("  {} -- {} --> {}", node(id), status, node(target));
            }
            if !step.transitions.by_status.contains_key("success") {
                print_end_edge(id, "success");
            }
        }
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

fn node(id: &StepId) -> String {
    id.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn print_end_edge(id: &StepId, status: &str) {
    println!("  {} -- {} --> END", node(id), status);
}
