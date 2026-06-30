//! `catalog-cli` — a small playground for `cowboy-workflow-catalog`.
//!
//! Each subcommand exercises one piece of the crate's logic so the catalog
//! behavior can be poked at from the shell:
//!
//! ```text
//! catalog-cli list [DIR...]                load catalog (built-in + DIRs), list workflows
//! catalog-cli sources [DIR...]             load raw sources, show the first line of each
//! catalog-cli show <ROOT> <ENTRY>          resolve + read one source file (safe path)
//! catalog-cli builtin                      print the built-in default workflow
//! catalog-cli normalize <PATH>...          run safe entry-path normalization
//! catalog-cli create <ROOT> <ENTRY> <FILE> write FILE as a new workflow into ROOT
//! ```

use std::env;
use std::fs;

use cowboy_workflow_catalog::{
    AppliedWorkflowImprovement, LoadedWorkflowSource, WorkflowCatalogLoader, WorkflowSourceUpdate,
    apply_update, builtin_default_source_ref, builtin_default_workflow_source, load_source_ref,
    normalize_workflow_entry,
};
use cowboy_workflow_core::WorkflowSourceRef;

type CliResult = Result<(), Box<dyn std::error::Error>>;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> CliResult {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        usage();
    };
    let rest = args.collect::<Vec<_>>();
    match command.as_str() {
        "list" => list(&rest),
        "sources" => sources(&rest),
        "show" => show(&rest),
        "builtin" => builtin(),
        "normalize" => normalize(&rest),
        "create" => create(&rest),
        _ => usage(),
    }
}

/// Load the full catalog (built-in plus every DIR scanned as a project root)
/// and print one entry per workflow.
///
/// Note: descriptions are declared in Lua and only resolved by compiling the
/// source, which the engine does — the catalog crate alone reports `<none>`
/// for filesystem workflows.
fn list(dirs: &[String]) -> CliResult {
    warn_missing_dirs(dirs);
    let catalog = build_loader(dirs).load_catalog()?;
    println!("workflows ({})", catalog.workflows.len());
    for (id, source_ref) in &catalog.workflows {
        println!("- {id}");
        println!("    entry:       {}", source_ref.entry);
        println!("    root:        {}", root_label(source_ref));
        println!(
            "    description: {}",
            source_ref.description.as_deref().unwrap_or("<none>")
        );
    }
    Ok(())
}

/// Load raw source bundles without building a catalog map, printing the first
/// non-empty line of each so duplicate ids stay visible.
fn sources(dirs: &[String]) -> CliResult {
    warn_missing_dirs(dirs);
    let sources = build_loader(dirs).load_sources()?;
    println!("sources ({})", sources.len());
    for LoadedWorkflowSource { source_ref, source } in &sources {
        println!("- {} ({})", source_ref.id, source_ref.entry);
        println!("    root:       {}", root_label(source_ref));
        println!("    first line: {}", first_line(source));
    }
    Ok(())
}

/// Resolve `ENTRY` under `ROOT` through the crate's safe path handling and dump
/// the loaded source. Path escapes, absolute paths, and non-`.lua` entries are
/// rejected by `load_source_ref`.
fn show(args: &[String]) -> CliResult {
    let [root, entry] = args else { usage() };
    let source_ref = WorkflowSourceRef {
        id: workflow_id_from_entry(entry),
        entry: entry.clone(),
        root: Some(root.clone()),
        description: None,
    };
    let loaded = load_source_ref(&source_ref)?;
    println!("id:    {}", loaded.source_ref.id);
    println!("entry: {}", loaded.source_ref.entry);
    println!("root:  {}", root_label(&loaded.source_ref));
    println!("---");
    print!("{}", loaded.source);
    if !loaded.source.ends_with('\n') {
        println!();
    }
    Ok(())
}

/// Print the always-available built-in default workflow.
fn builtin() -> CliResult {
    let source_ref = builtin_default_source_ref();
    let loaded = builtin_default_workflow_source();
    println!("id:          {}", source_ref.id);
    println!("entry:       {}", source_ref.entry);
    println!(
        "description: {}",
        source_ref.description.as_deref().unwrap_or("<none>")
    );
    println!("---");
    print!("{}", loaded.source);
    if !loaded.source.ends_with('\n') {
        println!();
    }
    Ok(())
}

/// Run each PATH through `normalize_workflow_entry`, reporting the normalized
/// form or the rejection reason. Great for probing the path-safety rules.
fn normalize(paths: &[String]) -> CliResult {
    if paths.is_empty() {
        usage();
    }
    for path in paths {
        match normalize_workflow_entry(path) {
            Ok(normalized) => println!("ok   {path:?} -> {normalized:?}"),
            Err(err) => println!("err  {path:?} -> {err}"),
        }
    }
    Ok(())
}

/// Materialize FILE's contents as a brand-new workflow at ENTRY under ROOT.
/// Existing workflows in ROOT are loaded first so the create conflict check is
/// exercised.
fn create(args: &[String]) -> CliResult {
    let [root, entry, source_file] = args else {
        usage()
    };
    let replacement_source = fs::read_to_string(source_file)?;
    let draft = WorkflowSourceRef {
        id: workflow_id_from_entry(entry),
        entry: entry.clone(),
        root: None,
        description: None,
    };
    let catalog = build_loader(std::slice::from_ref(root))
        .without_builtin()
        .load_catalog()?;
    let update = WorkflowSourceUpdate::CreateNew {
        draft,
        replacement_source,
    };
    match apply_update(root, &catalog, &update)? {
        AppliedWorkflowImprovement::Created { source, path } => {
            println!("created {} at {path}", source.id);
        }
        other => println!("unexpected result: {other:?}"),
    }
    Ok(())
}

fn build_loader(dirs: &[String]) -> WorkflowCatalogLoader {
    let mut loader = WorkflowCatalogLoader::new();
    for dir in dirs {
        loader = loader.with_project_dir(dir);
    }
    loader
}

/// Warn (without failing) about DIR arguments that do not exist or are not
/// directories. The catalog loader silently skips such roots — intentional for
/// the product runtime, but a common source of confusion here when a relative
/// path is resolved against the wrong working directory.
fn warn_missing_dirs(dirs: &[String]) {
    for dir in dirs {
        let path = std::path::Path::new(dir);
        if !path.exists() {
            let cwd = env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "?".to_string());
            eprintln!("warning: directory {dir:?} does not exist; skipping (cwd: {cwd})");
        } else if !path.is_dir() {
            eprintln!("warning: {dir:?} is not a directory; skipping");
        }
    }
}

fn workflow_id_from_entry(entry: &str) -> String {
    entry.trim_end_matches(".lua").to_string()
}

fn root_label(source_ref: &WorkflowSourceRef) -> &str {
    source_ref.root.as_deref().unwrap_or("<built-in>")
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

fn usage() -> ! {
    eprintln!("catalog-cli — explore cowboy-workflow-catalog logic");
    eprintln!();
    eprintln!("usage:");
    eprintln!(
        "  catalog-cli list [DIR...]                 load catalog (built-in + DIRs), list workflows"
    );
    eprintln!(
        "  catalog-cli sources [DIR...]              load raw sources, show first line of each"
    );
    eprintln!(
        "  catalog-cli show <ROOT> <ENTRY>           resolve + read one source file (safe path)"
    );
    eprintln!("  catalog-cli builtin                       print the built-in default workflow");
    eprintln!("  catalog-cli normalize <PATH>...           run safe entry-path normalization");
    eprintln!("  catalog-cli create <ROOT> <ENTRY> <FILE>  write FILE as a new workflow into ROOT");
    std::process::exit(2);
}
