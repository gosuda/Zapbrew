use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use zapbrew_api::Formula;
use zapbrew_prefix::{CommandRunner, CommandSpec, Env};
use zapbrew_types::BottleTag;

use crate::{Ctx, OpError};

const MAINTENANCE_TYPES: &[&str] = &[
    "compile_gsettings_schemas",
    "gio_querymodules",
    "gdk_pixbuf_query_loaders",
    "gtk_update_icon_cache",
    "update_mime_database",
    "update_desktop_database",
];

#[derive(Debug, Clone)]
pub(crate) struct InstallSteps {
    formula: String,
    steps: Vec<Step>,
}

#[derive(Debug, Clone)]
struct Step {
    index: usize,
    kind: StepKind,
    guards: Vec<Guard>,
}

#[derive(Debug, Clone)]
enum StepKind {
    Mkdir {
        path: PathSpec,
        parents: bool,
    },
    Touch {
        path: PathSpec,
    },
    Move {
        source: PathSpec,
        target: PathSpec,
        force: bool,
        overwrite: bool,
        source_glob: bool,
    },
    MoveChildren {
        source: PathSpec,
        target: PathSpec,
    },
    Copy {
        source: PathSpec,
        target: PathSpec,
        recursive: bool,
        overwrite: bool,
        source_glob: bool,
    },
    Remove {
        paths: Vec<PathSpec>,
        recursive: bool,
        symlink_target_contains: Option<String>,
        content_contains: Option<String>,
    },
    Inreplace {
        path: PathSpec,
        before: String,
        after: String,
        first_only: bool,
    },
    LinkDir {
        source: PathSpec,
        target: PathSpec,
    },
    LinkChildren {
        source: PathSpec,
        target: PathSpec,
        prefix: String,
        suffix: String,
    },
    Symlink {
        source: PathSpec,
        target: PathSpec,
        force: bool,
        source_glob: bool,
    },
    Write {
        path: PathSpec,
        content: String,
        overwrite: bool,
    },
    Warn {
        message: String,
    },
    SetPermissions {
        paths: Vec<PathSpec>,
        permissions: u32,
        non_recursive: bool,
    },
    Run(RunStep),
    Maintenance {
        kind: MaintenanceKind,
        path: Option<PathSpec>,
    },
}

#[derive(Debug, Clone)]
struct RunStep {
    command: PathSpec,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    chdir: Option<PathSpec>,
}

#[derive(Debug, Clone, Copy)]
enum MaintenanceKind {
    CompileGsettingsSchemas,
    GioQuerymodules,
    GdkPixbufQueryLoaders,
    GtkUpdateIconCache,
    UpdateMimeDatabase,
    UpdateDesktopDatabase,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Guard {
    IfExists(PathSpec),
    UnlessExists(PathSpec),
    On(Platform),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Platform {
    Linux,
    MacOs,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PathSpec {
    base: Base,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Base {
    Prefix,
    OptPrefix,
    Bin,
    Sbin,
    Lib,
    Libexec,
    Share,
    Pkgshare,
    Include,
    Etc,
    Pkgetc,
    Var,
    HomebrewPrefix,
    FormulaPrefix(String),
    FormulaOptPrefix(String),
    FormulaPkgetc(String),
    Cellar,
    Rack,
    BashCompletion,
    ZshCompletion,
    FishCompletion,
    PwshCompletion,
    Relative,
}

#[derive(Debug)]
enum Inverse {
    Remove(Utf8PathBuf),
    Replace {
        path: Utf8PathBuf,
        backup: Utf8PathBuf,
    },
    Restore {
        path: Utf8PathBuf,
        backup: Utf8PathBuf,
    },
    MoveBack {
        source: Utf8PathBuf,
        destination: Utf8PathBuf,
    },
    Mode {
        path: Utf8PathBuf,
        mode: u32,
    },
}

#[derive(Debug, Default)]
pub(crate) struct StepJournal {
    root: Option<Utf8PathBuf>,
    inverses: Vec<Inverse>,
    next_backup: u64,
}

struct Resolver<'a> {
    env: &'a Env,
    formula: &'a Formula,
    catalog: &'a zapbrew_api::Catalog,
    keg: &'a Utf8Path,
}

impl InstallSteps {
    pub(crate) fn parse(ctx: &Ctx, formula: &Formula) -> Result<Self, OpError> {
        let value = formula
            .raw
            .get("post_install_steps")
            .unwrap_or(&Value::Null);
        if value.is_null() {
            return Ok(Self {
                formula: formula.name.clone(),
                steps: Vec::new(),
            });
        }
        let array = value.as_array().ok_or_else(|| {
            step_error(
                formula,
                0,
                "<root>",
                "post_install_steps must be an array or null",
            )
        })?;
        let mut steps = Vec::with_capacity(array.len());
        for (index, value) in array.iter().enumerate() {
            steps.push(parse_step(ctx, formula, index, value)?);
        }
        let parsed = Self {
            formula: formula.name.clone(),
            steps,
        };
        parsed.preflight(ctx, formula)?;
        Ok(parsed)
    }

    fn preflight(&self, ctx: &Ctx, formula: &Formula) -> Result<(), OpError> {
        let keg = ctx
            .env
            .cellar
            .join(&formula.name)
            .join(formula.pkg_version.to_string());
        let resolver = Resolver {
            env: &ctx.env,
            formula,
            catalog: ctx.catalog.as_ref(),
            keg: &keg,
        };
        for step in &self.steps {
            preflight_step(&resolver, step)?;
        }
        Ok(())
    }

    pub(crate) fn execute(
        &self,
        ctx: &Ctx,
        formula: &Formula,
        keg: &Utf8Path,
        journal: &mut StepJournal,
    ) -> Result<(), OpError> {
        if self.steps.is_empty() {
            return Ok(());
        }
        let resolver = Resolver {
            env: &ctx.env,
            formula,
            catalog: ctx.catalog.as_ref(),
            keg,
        };
        let mut guard_results = BTreeMap::new();
        let mut maintenance = Vec::new();
        for step in &self.steps {
            if !guards_match(&resolver, step, &mut guard_results)? {
                continue;
            }
            match &step.kind {
                StepKind::Run(run) => run_command(
                    ctx.commands.as_ref(),
                    &self.formula,
                    step.index,
                    command_spec(&resolver, step.index, run)?,
                )?,
                StepKind::Maintenance { kind, path } => {
                    maintenance.push((
                        step.index,
                        maintenance_spec(&resolver, step.index, *kind, path.as_ref())?,
                    ));
                }
                _ => execute_filesystem_step(ctx, &resolver, step, journal)?,
            }
        }
        for (index, spec) in maintenance {
            run_command(ctx.commands.as_ref(), &self.formula, index, spec)?;
        }
        Ok(())
    }
}

impl StepJournal {
    fn backup(
        &mut self,
        env: &Env,
        formula: &str,
        index: usize,
        path: &Utf8Path,
    ) -> Result<Utf8PathBuf, OpError> {
        if self.root.is_none() {
            let rack = path_ancestor_rack(env, formula);
            let root = create_owned_directory(&rack, ".zapbrew-step-journal")?;
            self.root = Some(root);
        }
        let root = self.root.as_ref().ok_or_else(|| OpError::InvalidState {
            reason: "step journal root was not created".to_owned(),
        })?;
        let backup = root.join(format!("{index}-{}", self.next_backup));
        self.next_backup = self.next_backup.saturating_add(1);
        fs::rename(path, &backup)
            .map_err(|source| OpError::io("journal install-step path", path, source))?;
        Ok(backup)
    }

    fn replace_before_mutation(
        &mut self,
        env: &Env,
        formula: &str,
        index: usize,
        path: &Utf8Path,
    ) -> Result<(), OpError> {
        if entry_exists(path) {
            let backup = self.backup(env, formula, index, path)?;
            self.inverses.push(Inverse::Replace {
                path: path.to_path_buf(),
                backup,
            });
        } else {
            self.inverses.push(Inverse::Remove(path.to_path_buf()));
        }
        Ok(())
    }

    pub(crate) fn rollback(&mut self, env: &Env) -> Vec<Utf8PathBuf> {
        let mut leftovers = Vec::new();
        for inverse in self.inverses.iter().rev() {
            if apply_inverse(env, inverse).is_err() {
                inverse_leftovers(inverse, &mut leftovers);
            }
        }
        if let Some(root) = self.root.as_ref()
            && entry_exists(root)
            && remove_tree_confined(env, root).is_err()
        {
            leftovers.push(root.clone());
        }
        leftovers.sort();
        leftovers.dedup();
        leftovers
    }

    pub(crate) fn cleanup_path(&self) -> Option<&Utf8Path> {
        self.root.as_deref()
    }

    pub(crate) fn clear(&mut self) {
        self.root = None;
        self.inverses.clear();
    }
}

fn parse_step(ctx: &Ctx, formula: &Formula, index: usize, value: &Value) -> Result<Step, OpError> {
    let object = value
        .as_object()
        .ok_or_else(|| step_error(formula, index, "<unknown>", "step must be an object"))?;
    let type_name = required_string(formula, index, "<unknown>", object, "type")?;
    let guards = parse_guards(ctx, formula, index, &type_name, object.get("guards"))?;
    let kind = match type_name.as_str() {
        "mkdir" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &["type", "path", "guards"],
            )?;
            StepKind::Mkdir {
                path: path_field(ctx, formula, index, &type_name, object, "path")?,
                parents: false,
            }
        }
        "mkdir_p" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &["type", "path", "guards"],
            )?;
            StepKind::Mkdir {
                path: path_field(ctx, formula, index, &type_name, object, "path")?,
                parents: true,
            }
        }
        "touch" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &["type", "path", "guards"],
            )?;
            StepKind::Touch {
                path: path_field(ctx, formula, index, &type_name, object, "path")?,
            }
        }
        "move" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &[
                    "type",
                    "source",
                    "target",
                    "force",
                    "overwrite",
                    "source_glob",
                    "guards",
                ],
            )?;
            StepKind::Move {
                source: path_field(ctx, formula, index, &type_name, object, "source")?,
                target: path_field(ctx, formula, index, &type_name, object, "target")?,
                force: optional_bool(formula, index, &type_name, object, "force", false)?,
                overwrite: optional_bool(formula, index, &type_name, object, "overwrite", true)?,
                source_glob: optional_bool(
                    formula,
                    index,
                    &type_name,
                    object,
                    "source_glob",
                    false,
                )?,
            }
        }
        "move_children" | "move_contents" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &["type", "source", "target", "guards"],
            )?;
            StepKind::MoveChildren {
                source: path_field(ctx, formula, index, &type_name, object, "source")?,
                target: path_field(ctx, formula, index, &type_name, object, "target")?,
            }
        }
        "copy" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &[
                    "type",
                    "source",
                    "target",
                    "recursive",
                    "overwrite",
                    "source_glob",
                    "guards",
                ],
            )?;
            StepKind::Copy {
                source: path_field(ctx, formula, index, &type_name, object, "source")?,
                target: path_field(ctx, formula, index, &type_name, object, "target")?,
                recursive: optional_bool(formula, index, &type_name, object, "recursive", false)?,
                overwrite: optional_bool(formula, index, &type_name, object, "overwrite", true)?,
                source_glob: optional_bool(
                    formula,
                    index,
                    &type_name,
                    object,
                    "source_glob",
                    false,
                )?,
            }
        }
        "remove" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &[
                    "type",
                    "paths",
                    "recursive",
                    "sudo",
                    "symlink_target_contains",
                    "content_contains",
                    "guards",
                ],
            )?;
            reject_sudo(formula, index, &type_name, object)?;
            StepKind::Remove {
                paths: paths_field(ctx, formula, index, &type_name, object, "paths")?,
                recursive: optional_bool(formula, index, &type_name, object, "recursive", false)?,
                symlink_target_contains: optional_string(
                    formula,
                    index,
                    &type_name,
                    object,
                    "symlink_target_contains",
                )?,
                content_contains: optional_string(
                    formula,
                    index,
                    &type_name,
                    object,
                    "content_contains",
                )?,
            }
        }
        "inreplace" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &[
                    "type",
                    "path",
                    "before",
                    "after",
                    "regexp",
                    "regexp_options",
                    "skip_audit",
                    "first_only",
                    "guards",
                ],
            )?;
            if optional_bool(formula, index, &type_name, object, "regexp", false)?
                || object.contains_key("regexp_options")
            {
                return Err(step_error(
                    formula,
                    index,
                    &type_name,
                    "regexp inreplace is not supported",
                ));
            }
            StepKind::Inreplace {
                path: path_field(ctx, formula, index, &type_name, object, "path")?,
                before: required_string(formula, index, &type_name, object, "before")?,
                after: required_string(formula, index, &type_name, object, "after")?,
                first_only: optional_bool(formula, index, &type_name, object, "first_only", false)?,
            }
        }
        "link_dir" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &["type", "source", "target", "guards"],
            )?;
            StepKind::LinkDir {
                source: path_field(ctx, formula, index, &type_name, object, "source")?,
                target: path_field(ctx, formula, index, &type_name, object, "target")?,
            }
        }
        "link_children" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &["type", "source", "target", "prefix", "suffix", "guards"],
            )?;
            StepKind::LinkChildren {
                source: path_field(ctx, formula, index, &type_name, object, "source")?,
                target: path_field(ctx, formula, index, &type_name, object, "target")?,
                prefix: optional_string(formula, index, &type_name, object, "prefix")?
                    .unwrap_or_default(),
                suffix: optional_string(formula, index, &type_name, object, "suffix")?
                    .unwrap_or_default(),
            }
        }
        "symlink" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &[
                    "type",
                    "source",
                    "target",
                    "force",
                    "uninstall",
                    "source_glob",
                    "sudo",
                    "guards",
                ],
            )?;
            reject_sudo(formula, index, &type_name, object)?;
            StepKind::Symlink {
                source: path_field_allow_relative(
                    ctx, formula, index, &type_name, object, "source",
                )?,
                target: path_field(ctx, formula, index, &type_name, object, "target")?,
                force: optional_bool(formula, index, &type_name, object, "force", false)?,
                source_glob: optional_bool(
                    formula,
                    index,
                    &type_name,
                    object,
                    "source_glob",
                    false,
                )?,
            }
        }
        "write" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &["type", "path", "content", "overwrite", "guards"],
            )?;
            StepKind::Write {
                path: path_field(ctx, formula, index, &type_name, object, "path")?,
                content: required_string(formula, index, &type_name, object, "content")?,
                overwrite: optional_bool(formula, index, &type_name, object, "overwrite", false)?,
            }
        }
        "warn" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &["type", "message", "guards"],
            )?;
            StepKind::Warn {
                message: required_string(formula, index, &type_name, object, "message")?,
            }
        }
        "set_permissions" => {
            allowed_keys(
                formula,
                index,
                &type_name,
                object,
                &["type", "paths", "permissions", "non_recursive", "guards"],
            )?;
            let permissions = required_string(formula, index, &type_name, object, "permissions")?;
            let mode =
                u32::from_str_radix(permissions.trim_start_matches('0'), 8).map_err(|_| {
                    step_error(
                        formula,
                        index,
                        &type_name,
                        "permissions must be an octal string",
                    )
                })?;
            if mode > 0o7777 {
                return Err(step_error(
                    formula,
                    index,
                    &type_name,
                    "permissions exceed 07777",
                ));
            }
            StepKind::SetPermissions {
                paths: paths_field(ctx, formula, index, &type_name, object, "paths")?,
                permissions: mode,
                non_recursive: optional_bool(
                    formula,
                    index,
                    &type_name,
                    object,
                    "non_recursive",
                    false,
                )?,
            }
        }
        "run" => parse_run(ctx, formula, index, &type_name, object)?,
        name if MAINTENANCE_TYPES.contains(&name) => {
            parse_maintenance(ctx, formula, index, name, object)?
        }
        _ => {
            return Err(step_error(
                formula,
                index,
                &type_name,
                "unsupported install step type",
            ));
        }
    };
    Ok(Step {
        index,
        kind,
        guards,
    })
}

fn parse_run(
    ctx: &Ctx,
    formula: &Formula,
    index: usize,
    type_name: &str,
    object: &Map<String, Value>,
) -> Result<StepKind, OpError> {
    allowed_keys(
        formula,
        index,
        type_name,
        object,
        &[
            "type",
            "command",
            "args",
            "env",
            "sudo",
            "print_stdout",
            "suppress_stderr",
            "stdin_path",
            "stdout_path",
            "chdir",
            "guards",
        ],
    )?;
    reject_sudo(formula, index, type_name, object)?;
    if object
        .get("stdin_path")
        .is_some_and(|value| !value.is_null())
        || object
            .get("stdout_path")
            .is_some_and(|value| !value.is_null())
    {
        return Err(step_error(
            formula,
            index,
            type_name,
            "stdin_path and stdout_path are unsupported by CommandRunner",
        ));
    }
    optional_bool(formula, index, type_name, object, "print_stdout", false)?;
    optional_bool(formula, index, type_name, object, "suppress_stderr", false)?;
    let args = match object.get("args") {
        None => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    step_error(formula, index, type_name, "args must contain only strings")
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(step_error(
                formula,
                index,
                type_name,
                "args must be an array",
            ));
        }
    };
    let env = match object.get("env") {
        None => BTreeMap::new(),
        Some(Value::Object(values)) => values
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_owned()))
                    .ok_or_else(|| {
                        step_error(formula, index, type_name, "env values must be strings")
                    })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?,
        Some(_) => {
            return Err(step_error(
                formula,
                index,
                type_name,
                "env must be an object",
            ));
        }
    };
    let command = path_field(ctx, formula, index, type_name, object, "command")?;
    if matches!(
        command.base,
        Base::HomebrewPrefix
            | Base::Etc
            | Base::Var
            | Base::Pkgetc
            | Base::FormulaPkgetc(_)
            | Base::Relative
    ) {
        return Err(step_error(
            formula,
            index,
            type_name,
            "run executable base is not a keg or opt base",
        ));
    }
    Ok(StepKind::Run(RunStep {
        command,
        args,
        env,
        chdir: optional_path_field(ctx, formula, index, type_name, object, "chdir")?,
    }))
}

fn parse_maintenance(
    ctx: &Ctx,
    formula: &Formula,
    index: usize,
    name: &str,
    object: &Map<String, Value>,
) -> Result<StepKind, OpError> {
    let path_required = !matches!(name, "gdk_pixbuf_query_loaders");
    let allowed = if path_required {
        &["type", "path", "guards"][..]
    } else {
        &["type", "guards"][..]
    };
    allowed_keys(formula, index, name, object, allowed)?;
    let kind = match name {
        "compile_gsettings_schemas" => MaintenanceKind::CompileGsettingsSchemas,
        "gio_querymodules" => MaintenanceKind::GioQuerymodules,
        "gdk_pixbuf_query_loaders" => MaintenanceKind::GdkPixbufQueryLoaders,
        "gtk_update_icon_cache" => MaintenanceKind::GtkUpdateIconCache,
        "update_mime_database" => MaintenanceKind::UpdateMimeDatabase,
        "update_desktop_database" => MaintenanceKind::UpdateDesktopDatabase,
        _ => {
            return Err(step_error(
                formula,
                index,
                name,
                "unsupported maintenance action",
            ));
        }
    };
    Ok(StepKind::Maintenance {
        kind,
        path: if path_required {
            Some(path_field(ctx, formula, index, name, object, "path")?)
        } else {
            None
        },
    })
}

fn parse_guards(
    ctx: &Ctx,
    formula: &Formula,
    index: usize,
    type_name: &str,
    value: Option<&Value>,
) -> Result<Vec<Guard>, OpError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| step_error(formula, index, type_name, "guards must be an array"))?;
    array
        .iter()
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| step_error(formula, index, type_name, "guard must be an object"))?;
            let condition = required_string(formula, index, type_name, object, "condition")?;
            match condition.as_str() {
                "if_exists" | "unless_exists" => {
                    allowed_keys(
                        formula,
                        index,
                        type_name,
                        object,
                        &["condition", "base", "path", "formula"],
                    )?;
                    let mut path_object = object.clone();
                    path_object.remove("condition");
                    let spec = parse_path_spec(
                        ctx,
                        formula,
                        index,
                        type_name,
                        &Value::Object(path_object),
                        false,
                    )?;
                    Ok(if condition == "if_exists" {
                        Guard::IfExists(spec)
                    } else {
                        Guard::UnlessExists(spec)
                    })
                }
                "on" => {
                    allowed_keys(formula, index, type_name, object, &["condition", "value"])?;
                    match required_string(formula, index, type_name, object, "value")?.as_str() {
                        "linux" => Ok(Guard::On(Platform::Linux)),
                        "macos" => Ok(Guard::On(Platform::MacOs)),
                        _ => Err(step_error(
                            formula,
                            index,
                            type_name,
                            "unknown platform guard value",
                        )),
                    }
                }
                _ => Err(step_error(
                    formula,
                    index,
                    type_name,
                    "unknown guard condition",
                )),
            }
        })
        .collect()
}

fn path_field(
    ctx: &Ctx,
    formula: &Formula,
    index: usize,
    type_name: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<PathSpec, OpError> {
    let value = object
        .get(key)
        .ok_or_else(|| step_error(formula, index, type_name, &format!("missing {key}")))?;
    parse_path_spec(ctx, formula, index, type_name, value, false)
}

fn path_field_allow_relative(
    ctx: &Ctx,
    formula: &Formula,
    index: usize,
    type_name: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<PathSpec, OpError> {
    let value = object
        .get(key)
        .ok_or_else(|| step_error(formula, index, type_name, &format!("missing {key}")))?;
    parse_path_spec(ctx, formula, index, type_name, value, true)
}

fn optional_path_field(
    ctx: &Ctx,
    formula: &Formula,
    index: usize,
    type_name: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<PathSpec>, OpError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => parse_path_spec(ctx, formula, index, type_name, value, false).map(Some),
    }
}

fn paths_field(
    ctx: &Ctx,
    formula: &Formula,
    index: usize,
    type_name: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<Vec<PathSpec>, OpError> {
    let values = object.get(key).and_then(Value::as_array).ok_or_else(|| {
        step_error(
            formula,
            index,
            type_name,
            &format!("{key} must be an array"),
        )
    })?;
    if values.is_empty() {
        return Err(step_error(
            formula,
            index,
            type_name,
            &format!("{key} must not be empty"),
        ));
    }
    values
        .iter()
        .map(|value| parse_path_spec(ctx, formula, index, type_name, value, false))
        .collect()
}

fn parse_path_spec(
    ctx: &Ctx,
    formula: &Formula,
    index: usize,
    type_name: &str,
    value: &Value,
    allow_relative: bool,
) -> Result<PathSpec, OpError> {
    let object = value
        .as_object()
        .ok_or_else(|| step_error(formula, index, type_name, "path spec must be an object"))?;
    allowed_keys(
        formula,
        index,
        type_name,
        object,
        &["base", "path", "formula"],
    )?;
    let path = required_string(formula, index, type_name, object, "path")?;
    validate_relative_path(formula, index, type_name, &path)?;
    // Live API path specs are either `{"base": ..., "path": ...}` or the
    // template form `{"path": "{{etc}}/..."}` where the leading `{{token}}`
    // selects the base. Derive the base for the template form so both shapes
    // parse identically (brew's formula.rb emits the template form at scale).
    let base_name = optional_string(formula, index, type_name, object, "base")?;
    let (base_name, path) = match base_name {
        Some(name) => (name, path),
        None => match path
            .split_once("{{")
            .and_then(|(head, _)| head.is_empty().then_some(()))
        {
            Some(()) => {
                let Some((token, rest)) = path.strip_prefix("{{").and_then(|t| t.split_once("}}/"))
                else {
                    return Err(step_error(
                        formula,
                        index,
                        type_name,
                        "template path must start with `{{token}}/`",
                    ));
                };
                (token.to_owned(), rest.to_owned())
            }
            None => {
                return Err(step_error(
                    formula,
                    index,
                    type_name,
                    "path spec requires a `base` field or a leading `{{token}}/` prefix",
                ));
            }
        },
    };
    let formula_ref = optional_string(formula, index, type_name, object, "formula")?;
    let base = match (base_name.as_str(), formula_ref) {
        ("prefix" | "keg" | "formula_prefix", None) => Base::Prefix,
        ("opt_prefix" | "opt", None) => Base::OptPrefix,
        ("bin", None) => Base::Bin,
        ("sbin", None) => Base::Sbin,
        ("lib", None) => Base::Lib,
        ("libexec", None) => Base::Libexec,
        ("share", None) => Base::Share,
        ("pkgshare", None) => Base::Pkgshare,
        ("include", None) => Base::Include,
        ("etc", None) => Base::Etc,
        ("pkgetc", None) => Base::Pkgetc,
        ("var", None) => Base::Var,
        ("homebrew_prefix" | "HOMEBREW_PREFIX", None) => Base::HomebrewPrefix,
        ("HOMEBREW_CELLAR", None) => Base::Cellar,
        ("rack", None) => Base::Rack,
        ("bash_completion", None) => Base::BashCompletion,
        ("zsh_completion", None) => Base::ZshCompletion,
        ("fish_completion", None) => Base::FishCompletion,
        ("pwsh_completion", None) => Base::PwshCompletion,
        ("formula_prefix", Some(name)) => Base::FormulaPrefix(resolve_formula_reference(
            ctx, formula, index, type_name, &name,
        )?),
        ("formula_opt_prefix", Some(name)) => Base::FormulaOptPrefix(resolve_formula_reference(
            ctx, formula, index, type_name, &name,
        )?),
        ("formula_pkgetc", Some(name)) => Base::FormulaPkgetc(resolve_formula_reference(
            ctx, formula, index, type_name, &name,
        )?),
        ("relative", None) if allow_relative => Base::Relative,
        ("home" | "temp", _) => {
            return Err(step_error(
                formula,
                index,
                type_name,
                "home and temp bases are rejected during formula install",
            ));
        }
        ("absolute", _) => {
            return Err(step_error(
                formula,
                index,
                type_name,
                "absolute paths are rejected",
            ));
        }
        (_, Some(_)) => {
            return Err(step_error(
                formula,
                index,
                type_name,
                "formula is only valid with a formula_* base",
            ));
        }
        _ => {
            return Err(step_error(
                formula,
                index,
                type_name,
                "unknown install step base",
            ));
        }
    };
    Ok(PathSpec { base, path })
}

fn resolve_formula_reference(
    ctx: &Ctx,
    formula: &Formula,
    index: usize,
    type_name: &str,
    name: &str,
) -> Result<String, OpError> {
    let resolved = ctx.catalog.get(name).ok_or_else(|| {
        step_error(
            formula,
            index,
            type_name,
            &format!("unknown formula reference {name}"),
        )
    })?;
    if resolved.name != formula.name
        && !formula.dependencies.iter().any(|dependency| {
            ctx.catalog
                .get(&dependency.name)
                .is_some_and(|candidate| candidate.name == resolved.name)
        })
    {
        return Err(step_error(
            formula,
            index,
            type_name,
            &format!(
                "formula reference {} is not an explicit dependency",
                resolved.name
            ),
        ));
    }
    Ok(resolved.name.clone())
}

fn validate_relative_path(
    formula: &Formula,
    index: usize,
    type_name: &str,
    path: &str,
) -> Result<(), OpError> {
    let candidate = Utf8Path::new(path);
    if candidate.is_absolute() || path.is_empty() {
        return Err(step_error(
            formula,
            index,
            type_name,
            "path must be non-empty and relative",
        ));
    }
    for component in candidate.components() {
        if matches!(
            component,
            Utf8Component::ParentDir | Utf8Component::RootDir | Utf8Component::Prefix(_)
        ) {
            return Err(step_error(
                formula,
                index,
                type_name,
                "path must not be absolute or contain ..",
            ));
        }
    }
    let bytes = path.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] == b']' {
            return Err(step_error(
                formula,
                index,
                type_name,
                "glob has an unmatched ]",
            ));
        }
        if bytes[offset] != b'[' {
            offset += 1;
            continue;
        }
        let Some(end) = bytes[offset + 1..]
            .iter()
            .position(|byte| *byte == b']')
            .map(|relative| offset + 1 + relative)
        else {
            return Err(step_error(
                formula,
                index,
                type_name,
                "glob has an unmatched [",
            ));
        };
        if end == offset + 1 {
            return Err(step_error(
                formula,
                index,
                type_name,
                "glob character class is empty",
            ));
        }
        offset = end + 1;
    }
    Ok(())
}

fn preflight_step(resolver: &Resolver<'_>, step: &Step) -> Result<(), OpError> {
    for guard in &step.guards {
        if let Guard::IfExists(path) | Guard::UnlessExists(path) = guard {
            preflight_path(resolver, step.index, step.kind.type_name(), path, false)?;
        }
    }
    match &step.kind {
        StepKind::Mkdir { path, .. } | StepKind::Touch { path } => {
            preflight_path(resolver, step.index, step.kind.type_name(), path, false)
        }
        StepKind::Inreplace {
            path,
            before,
            after,
            ..
        } => {
            preflight_path(resolver, step.index, step.kind.type_name(), path, false)?;
            expand_template(resolver, step.index, step.kind.type_name(), before)?;
            expand_template(resolver, step.index, step.kind.type_name(), after).map(|_| ())
        }
        StepKind::Write { path, content, .. } => {
            preflight_path(resolver, step.index, step.kind.type_name(), path, false)?;
            expand_template(resolver, step.index, step.kind.type_name(), content).map(|_| ())
        }
        StepKind::Move { source, target, .. }
        | StepKind::MoveChildren { source, target }
        | StepKind::Copy { source, target, .. }
        | StepKind::LinkDir { source, target } => {
            preflight_path(resolver, step.index, step.kind.type_name(), source, false)?;
            preflight_path(resolver, step.index, step.kind.type_name(), target, false)
        }
        StepKind::LinkChildren {
            source,
            target,
            prefix,
            suffix,
        } => {
            preflight_path(resolver, step.index, step.kind.type_name(), source, false)?;
            preflight_path(resolver, step.index, step.kind.type_name(), target, false)?;
            expand_template(resolver, step.index, step.kind.type_name(), prefix)?;
            expand_template(resolver, step.index, step.kind.type_name(), suffix).map(|_| ())
        }
        StepKind::Remove { paths, .. } | StepKind::SetPermissions { paths, .. } => {
            paths.iter().try_for_each(|path| {
                preflight_path(resolver, step.index, step.kind.type_name(), path, false)
            })
        }
        StepKind::Symlink { source, target, .. } => {
            preflight_path(resolver, step.index, step.kind.type_name(), source, true)?;
            preflight_path(resolver, step.index, step.kind.type_name(), target, false)
        }
        StepKind::Run(run) => {
            preflight_path(
                resolver,
                step.index,
                step.kind.type_name(),
                &run.command,
                false,
            )?;
            if let Some(chdir) = &run.chdir {
                preflight_path(resolver, step.index, step.kind.type_name(), chdir, false)?;
            }
            expand_all_strings(
                resolver,
                step.index,
                step.kind.type_name(),
                run.args
                    .iter()
                    .chain(run.env.keys())
                    .chain(run.env.values()),
            )
        }
        StepKind::Maintenance { kind, path } => {
            if let Some(path) = path {
                preflight_path(resolver, step.index, step.kind.type_name(), path, false)?;
            }
            let helpers: &[&str] = match kind {
                MaintenanceKind::CompileGsettingsSchemas | MaintenanceKind::GioQuerymodules => {
                    &["glib"]
                }
                MaintenanceKind::GdkPixbufQueryLoaders => &["gdk-pixbuf"],
                MaintenanceKind::GtkUpdateIconCache => &["gtk4", "gtk+3"],
                MaintenanceKind::UpdateMimeDatabase => &["shared-mime-info"],
                MaintenanceKind::UpdateDesktopDatabase => &["desktop-file-utils"],
            };
            if !helpers
                .iter()
                .any(|helper| resolver.catalog.get(helper).is_some())
            {
                return Err(step_error_named(
                    &resolver.formula.name,
                    step.index,
                    step.kind.type_name(),
                    &format!(
                        "maintenance formula {} is absent from catalog",
                        helpers.join(" or ")
                    ),
                ));
            }
            Ok(())
        }
        StepKind::Warn { message } => {
            expand_template(resolver, step.index, step.kind.type_name(), message).map(|_| ())
        }
    }
}

impl StepKind {
    fn type_name(&self) -> &'static str {
        match self {
            Self::Mkdir { parents: false, .. } => "mkdir",
            Self::Mkdir { parents: true, .. } => "mkdir_p",
            Self::Touch { .. } => "touch",
            Self::Move { .. } => "move",
            Self::MoveChildren { .. } => "move_contents",
            Self::Copy { .. } => "copy",
            Self::Remove { .. } => "remove",
            Self::Inreplace { .. } => "inreplace",
            Self::LinkDir { .. } => "link_dir",
            Self::LinkChildren { .. } => "link_children",
            Self::Symlink { .. } => "symlink",
            Self::Write { .. } => "write",
            Self::Warn { .. } => "warn",
            Self::SetPermissions { .. } => "set_permissions",
            Self::Run(_) => "run",
            Self::Maintenance { kind, .. } => kind.type_name(),
        }
    }
}

impl MaintenanceKind {
    fn type_name(self) -> &'static str {
        match self {
            Self::CompileGsettingsSchemas => "compile_gsettings_schemas",
            Self::GioQuerymodules => "gio_querymodules",
            Self::GdkPixbufQueryLoaders => "gdk_pixbuf_query_loaders",
            Self::GtkUpdateIconCache => "gtk_update_icon_cache",
            Self::UpdateMimeDatabase => "update_mime_database",
            Self::UpdateDesktopDatabase => "update_desktop_database",
        }
    }
}

fn preflight_path(
    resolver: &Resolver<'_>,
    index: usize,
    type_name: &str,
    spec: &PathSpec,
    allow_relative: bool,
) -> Result<(), OpError> {
    if matches!(spec.base, Base::Relative) {
        if allow_relative {
            return expand_template(resolver, index, type_name, &spec.path).map(|_| ());
        }
        return Err(step_error_named(
            &resolver.formula.name,
            index,
            type_name,
            "relative base is only valid for symlink sources",
        ));
    }
    let path = resolver.resolve(index, type_name, spec)?;
    ensure_existing_ancestors_confined(resolver.env, &path)
        .map_err(|reason| step_error_named(&resolver.formula.name, index, type_name, &reason))
}

impl Resolver<'_> {
    fn resolve(
        &self,
        index: usize,
        type_name: &str,
        spec: &PathSpec,
    ) -> Result<Utf8PathBuf, OpError> {
        if matches!(spec.base, Base::Relative) {
            return Err(step_error_named(
                &self.formula.name,
                index,
                type_name,
                "relative path has no filesystem location",
            ));
        }
        let root = self.base_path(index, type_name, &spec.base)?;
        let expanded = expand_template(self, index, type_name, &spec.path)?;
        let path = root.join(expanded);
        ensure_lexically_within(&path, &root)
            .map_err(|reason| step_error_named(&self.formula.name, index, type_name, &reason))?;
        Ok(path)
    }

    fn link_source(
        &self,
        index: usize,
        type_name: &str,
        spec: &PathSpec,
    ) -> Result<Utf8PathBuf, OpError> {
        if matches!(spec.base, Base::Relative) {
            return Ok(Utf8PathBuf::from(expand_template(
                self, index, type_name, &spec.path,
            )?));
        }
        self.resolve(index, type_name, spec)
    }

    fn base_path(
        &self,
        index: usize,
        type_name: &str,
        base: &Base,
    ) -> Result<Utf8PathBuf, OpError> {
        let path = match base {
            Base::Prefix => self.keg.to_path_buf(),
            Base::OptPrefix => self.env.prefix.join("opt").join(&self.formula.name),
            Base::Bin => self.keg.join("bin"),
            Base::Sbin => self.keg.join("sbin"),
            Base::Lib => self.keg.join("lib"),
            Base::Libexec => self.keg.join("libexec"),
            Base::Share => self.keg.join("share"),
            Base::Pkgshare => self.keg.join("share").join(&self.formula.name),
            Base::Include => self.keg.join("include"),
            Base::Etc => self.env.prefix.join("etc"),
            Base::Pkgetc => self.env.prefix.join("etc").join(&self.formula.name),
            Base::Var => self.env.prefix.join("var"),
            Base::HomebrewPrefix => self.env.prefix.clone(),
            Base::FormulaPrefix(name) => formula_keg(self, index, type_name, name)?,
            Base::FormulaOptPrefix(name) => self.env.prefix.join("opt").join(name),
            Base::FormulaPkgetc(name) => self.env.prefix.join("etc").join(name),
            Base::Cellar => self.env.cellar.clone(),
            Base::Rack => self.env.cellar.join(&self.formula.name),
            Base::BashCompletion => self.keg.join("etc/bash_completion.d"),
            Base::ZshCompletion => self.keg.join("share/zsh/site-functions"),
            Base::FishCompletion => self.keg.join("share/fish/vendor_completions.d"),
            Base::PwshCompletion => self.keg.join("share/pwsh/completions"),
            Base::Relative => {
                return Err(step_error_named(
                    &self.formula.name,
                    index,
                    type_name,
                    "relative base has no root",
                ));
            }
        };
        Ok(path)
    }
}

fn formula_keg(
    resolver: &Resolver<'_>,
    index: usize,
    type_name: &str,
    name: &str,
) -> Result<Utf8PathBuf, OpError> {
    let formula = resolver.catalog.get(name).ok_or_else(|| {
        step_error_named(
            &resolver.formula.name,
            index,
            type_name,
            "formula reference disappeared from catalog",
        )
    })?;
    Ok(resolver
        .env
        .cellar
        .join(&formula.name)
        .join(formula.pkg_version.to_string()))
}

fn execute_filesystem_step(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
) -> Result<(), OpError> {
    match &step.kind {
        StepKind::Mkdir { path, parents } => {
            let path = resolved_checked(resolver, step, path)?;
            if *parents {
                create_parents(ctx, resolver, step, journal, &path)
            } else {
                fs::create_dir(&path).map_err(|source| {
                    OpError::io("create install-step directory", &path, source)
                })?;
                journal.inverses.push(Inverse::Remove(path));
                Ok(())
            }
        }
        StepKind::Touch { path } => {
            let path = resolved_checked(resolver, step, path)?;
            create_parent(ctx, resolver, step, journal, &path)?;
            if !entry_exists(&path) {
                journal.inverses.push(Inverse::Remove(path.clone()));
                fs::write(&path, [])
                    .map_err(|source| OpError::io("touch install-step path", path, source))?;
            }
            Ok(())
        }
        StepKind::Move {
            source,
            target,
            force,
            overwrite,
            source_glob,
        } => execute_move(
            ctx,
            resolver,
            step,
            journal,
            (source, target, *force, *overwrite, *source_glob),
        ),
        StepKind::MoveChildren { source, target } => {
            execute_move_children(ctx, resolver, step, journal, source, target)
        }
        StepKind::Copy {
            source,
            target,
            recursive,
            overwrite,
            source_glob,
        } => execute_copy(
            ctx,
            resolver,
            step,
            journal,
            (source, target, *recursive, *overwrite, *source_glob),
        ),
        StepKind::Remove {
            paths,
            recursive,
            symlink_target_contains,
            content_contains,
        } => execute_remove(
            resolver,
            step,
            journal,
            (
                paths,
                *recursive,
                symlink_target_contains.as_deref(),
                content_contains.as_deref(),
            ),
        ),
        StepKind::Inreplace {
            path,
            before,
            after,
            first_only,
        } => execute_inreplace(
            ctx,
            resolver,
            step,
            journal,
            (path, before.as_str(), after.as_str(), *first_only),
        ),
        StepKind::LinkDir { source, target } => {
            execute_link_dir(ctx, resolver, step, journal, source, target)
        }
        StepKind::LinkChildren {
            source,
            target,
            prefix,
            suffix,
        } => execute_link_children(
            ctx,
            resolver,
            step,
            journal,
            (source, target, prefix.as_str(), suffix.as_str()),
        ),
        StepKind::Symlink {
            source,
            target,
            force,
            source_glob,
        } => execute_symlink(
            ctx,
            resolver,
            step,
            journal,
            (source, target, *force, *source_glob),
        ),
        StepKind::Write {
            path,
            content,
            overwrite,
        } => execute_write(ctx, resolver, step, journal, path, content, *overwrite),
        StepKind::Warn { message } => {
            ctx.reporter.opoo(&expand_template(
                resolver,
                step.index,
                step.kind.type_name(),
                message,
            )?);
            Ok(())
        }
        StepKind::SetPermissions {
            paths,
            permissions,
            non_recursive,
        } => execute_permissions(resolver, step, journal, paths, *permissions, *non_recursive),
        StepKind::Run(_) | StepKind::Maintenance { .. } => Ok(()),
    }
}

fn execute_move(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    action: (&PathSpec, &PathSpec, bool, bool, bool),
) -> Result<(), OpError> {
    let (source, target, force, overwrite, source_glob) = action;
    let source_pattern = resolved_checked(resolver, step, source)?;
    let sources = if source_glob {
        expand_glob(&source_pattern)?
    } else {
        vec![source_pattern]
    };
    if source_glob && sources.len() != 1 {
        return Err(step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            "source_glob must match exactly one path",
        ));
    }
    let source = sources.into_iter().next().ok_or_else(|| {
        step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            "source does not exist",
        )
    })?;
    let target = resolved_checked(resolver, step, target)?;
    create_parent(ctx, resolver, step, journal, &target)?;
    let destination = destination_for(&source, &target);
    if entry_exists(&destination) && !(overwrite || force) {
        return Err(step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            "move destination exists and overwrite is false",
        ));
    }
    if entry_exists(&destination) {
        let backup = journal.backup(
            resolver.env,
            &resolver.formula.name,
            step.index,
            &destination,
        )?;
        journal.inverses.push(Inverse::Replace {
            path: destination.clone(),
            backup,
        });
    }
    journal.inverses.push(Inverse::MoveBack {
        source: source.clone(),
        destination: destination.clone(),
    });
    fs::rename(&source, &destination)
        .map_err(|source_error| OpError::io("move install-step path", source, source_error))
}

fn execute_move_children(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    source: &PathSpec,
    target: &PathSpec,
) -> Result<(), OpError> {
    let source = resolved_checked(resolver, step, source)?;
    let target = resolved_checked(resolver, step, target)?;
    create_parents(ctx, resolver, step, journal, &target)?;
    for child in read_dir_sorted(&source)? {
        if child == target {
            continue;
        }
        let destination = target.join(child.file_name().ok_or_else(|| OpError::InvalidState {
            reason: format!("install-step child {child} has no name"),
        })?);
        if entry_exists(&destination) {
            return Err(step_error_named(
                &resolver.formula.name,
                step.index,
                step.kind.type_name(),
                "move_contents destination exists",
            ));
        }
        journal.inverses.push(Inverse::MoveBack {
            source: child.clone(),
            destination: destination.clone(),
        });
        fs::rename(&child, &destination)
            .map_err(|source_error| OpError::io("move install-step child", child, source_error))?;
    }
    Ok(())
}

fn execute_copy(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    action: (&PathSpec, &PathSpec, bool, bool, bool),
) -> Result<(), OpError> {
    let (source, target, recursive, overwrite, source_glob) = action;
    let source_pattern = resolved_checked(resolver, step, source)?;
    let sources = if source_glob {
        expand_glob(&source_pattern)?
    } else {
        vec![source_pattern]
    };
    if source_glob && sources.len() != 1 {
        return Err(step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            "source_glob must match exactly one path",
        ));
    }
    let source = sources.into_iter().next().ok_or_else(|| {
        step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            "copy source does not exist",
        )
    })?;
    let target = resolved_checked(resolver, step, target)?;
    create_parent(ctx, resolver, step, journal, &target)?;
    let destination = destination_for(&source, &target);
    if entry_exists(&destination) && !overwrite {
        return Err(step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            "copy destination exists and overwrite is false",
        ));
    }
    journal.replace_before_mutation(
        resolver.env,
        &resolver.formula.name,
        step.index,
        &destination,
    )?;
    copy_entry(&source, &destination, recursive)
}

fn execute_remove(
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    action: (&[PathSpec], bool, Option<&str>, Option<&str>),
) -> Result<(), OpError> {
    let (paths, recursive, symlink_contains, content_contains) = action;
    for spec in paths {
        let pattern = resolved_checked(resolver, step, spec)?;
        for path in expand_glob(&pattern)? {
            if !entry_exists(&path) {
                continue;
            }
            if let Some(fragment) = symlink_contains {
                if !fs::symlink_metadata(&path)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    continue;
                }
                let target = fs::read_link(&path)
                    .map_err(|source| OpError::io("read install-step symlink", &path, source))?;
                if !target.to_string_lossy().contains(fragment) {
                    continue;
                }
            }
            if let Some(fragment) = content_contains {
                if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file())
                {
                    continue;
                }
                let content = fs::read_to_string(&path).map_err(|source| {
                    OpError::io("read install-step filter path", &path, source)
                })?;
                if !content.contains(fragment) {
                    continue;
                }
            }
            if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_dir())
                && !recursive
            {
                return Err(step_error_named(
                    &resolver.formula.name,
                    step.index,
                    step.kind.type_name(),
                    "recursive is required to remove a directory",
                ));
            }
            let backup = journal.backup(resolver.env, &resolver.formula.name, step.index, &path)?;
            journal.inverses.push(Inverse::Restore { path, backup });
        }
    }
    Ok(())
}

fn execute_inreplace(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    action: (&PathSpec, &str, &str, bool),
) -> Result<(), OpError> {
    let (path, before, after, first_only) = action;
    let path = resolved_checked(resolver, step, path)?;
    let before = expand_template(resolver, step.index, step.kind.type_name(), before)?;
    let after = expand_template(resolver, step.index, step.kind.type_name(), after)?;
    let content = fs::read_to_string(&path)
        .map_err(|source| OpError::io("read inreplace path", &path, source))?;
    if !content.contains(&before) {
        return Err(step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            "inreplace pattern was not found",
        ));
    }
    let replaced = if first_only {
        content.replacen(&before, &after, 1)
    } else {
        content.replace(&before, &after)
    };
    journal.replace_before_mutation(resolver.env, &resolver.formula.name, step.index, &path)?;
    create_parent(ctx, resolver, step, journal, &path)?;
    fs::write(&path, replaced).map_err(|source| OpError::io("write inreplace path", path, source))
}

fn execute_link_dir(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    source: &PathSpec,
    target: &PathSpec,
) -> Result<(), OpError> {
    let source = resolved_checked(resolver, step, source)?;
    let target = resolved_checked(resolver, step, target)?;
    link_tree(ctx, resolver, step, journal, &source, &target)
}

fn link_tree(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    source: &Utf8Path,
    target: &Utf8Path,
) -> Result<(), OpError> {
    create_parents(ctx, resolver, step, journal, target)?;
    for child in read_dir_sorted(source)? {
        if child.file_name() == Some(".DS_Store") {
            continue;
        }
        let destination = target.join(child.file_name().ok_or_else(|| OpError::InvalidState {
            reason: format!("install-step path {child} has no name"),
        })?);
        let metadata = fs::symlink_metadata(&child)
            .map_err(|source_error| OpError::io("inspect link_dir source", &child, source_error))?;
        if metadata.file_type().is_dir() {
            if entry_exists(&destination)
                && !fs::symlink_metadata(&destination).is_ok_and(|entry| {
                    entry.file_type().is_dir() && !entry.file_type().is_symlink()
                })
            {
                journal.replace_before_mutation(
                    resolver.env,
                    &resolver.formula.name,
                    step.index,
                    &destination,
                )?;
            }
            create_parents(ctx, resolver, step, journal, &destination)?;
            link_tree(ctx, resolver, step, journal, &child, &destination)?;
        } else {
            create_link(ctx, resolver, step, journal, &child, &destination, true)?;
        }
    }
    Ok(())
}

fn execute_link_children(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    action: (&PathSpec, &PathSpec, &str, &str),
) -> Result<(), OpError> {
    let (source, target, prefix, suffix) = action;
    let source = resolved_checked(resolver, step, source)?;
    let target = resolved_checked(resolver, step, target)?;
    create_parents(ctx, resolver, step, journal, &target)?;
    let prefix = expand_template(resolver, step.index, step.kind.type_name(), prefix)?;
    let suffix = expand_template(resolver, step.index, step.kind.type_name(), suffix)?;
    for child in read_dir_sorted(&source)? {
        let name = child.file_name().ok_or_else(|| OpError::InvalidState {
            reason: format!("install-step path {child} has no name"),
        })?;
        create_link(
            ctx,
            resolver,
            step,
            journal,
            &child,
            &target.join(format!("{prefix}{name}{suffix}")),
            false,
        )?;
    }
    Ok(())
}

fn execute_symlink(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    action: (&PathSpec, &PathSpec, bool, bool),
) -> Result<(), OpError> {
    let (source, target, force, source_glob) = action;
    let target = resolved_checked(resolver, step, target)?;
    if source_glob {
        if matches!(source.base, Base::Relative) {
            return Err(step_error_named(
                &resolver.formula.name,
                step.index,
                step.kind.type_name(),
                "relative source_glob is not supported",
            ));
        }
        let pattern = resolved_checked(resolver, step, source)?;
        let sources = expand_glob(&pattern)?;
        if sources.is_empty() {
            return Ok(());
        }
        if sources.len() > 1
            || fs::symlink_metadata(&target).is_ok_and(|metadata| metadata.file_type().is_dir())
        {
            create_parents(ctx, resolver, step, journal, &target)?;
            for source in sources {
                let name = source.file_name().ok_or_else(|| OpError::InvalidState {
                    reason: format!("install-step path {source} has no name"),
                })?;
                create_link(
                    ctx,
                    resolver,
                    step,
                    journal,
                    &source,
                    &target.join(name),
                    force,
                )?;
            }
            return Ok(());
        }
        let source = sources.first().ok_or_else(|| OpError::InvalidState {
            reason: "source glob unexpectedly became empty".to_owned(),
        })?;
        return create_link(ctx, resolver, step, journal, source, &target, force);
    }
    let source = resolver.link_source(step.index, step.kind.type_name(), source)?;
    create_link(ctx, resolver, step, journal, &source, &target, force)
}

fn execute_write(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    path: &PathSpec,
    content: &str,
    overwrite: bool,
) -> Result<(), OpError> {
    let path = resolved_checked(resolver, step, path)?;
    if entry_exists(&path) && !overwrite {
        return Ok(());
    }
    let content = expand_template(resolver, step.index, step.kind.type_name(), content)?;
    journal.replace_before_mutation(resolver.env, &resolver.formula.name, step.index, &path)?;
    create_parent(ctx, resolver, step, journal, &path)?;
    fs::write(&path, content).map_err(|source| OpError::io("write install-step path", path, source))
}

fn execute_permissions(
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    paths: &[PathSpec],
    mode: u32,
    non_recursive: bool,
) -> Result<(), OpError> {
    for spec in paths {
        let pattern = resolved_checked(resolver, step, spec)?;
        for path in expand_glob(&pattern)? {
            set_mode_recursive(&path, mode, non_recursive, journal)?;
        }
    }
    Ok(())
}

fn set_mode_recursive(
    path: &Utf8Path,
    mode: u32,
    non_recursive: bool,
    journal: &mut StepJournal,
) -> Result<(), OpError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| OpError::io("inspect permission path", path, source))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    journal.inverses.push(Inverse::Mode {
        path: path.to_path_buf(),
        mode: metadata.permissions().mode(),
    });
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|source| OpError::io("set install-step permissions", path, source))?;
    if metadata.file_type().is_dir() && !non_recursive {
        for child in read_dir_sorted(path)? {
            set_mode_recursive(&child, mode, false, journal)?;
        }
    }
    Ok(())
}

fn create_link(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    source: &Utf8Path,
    destination: &Utf8Path,
    force: bool,
) -> Result<(), OpError> {
    create_parent(ctx, resolver, step, journal, destination)?;
    if entry_exists(destination) && !force {
        return Err(step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            "symlink destination exists and force is false",
        ));
    }
    journal.replace_before_mutation(
        resolver.env,
        &resolver.formula.name,
        step.index,
        destination,
    )?;
    symlink(source, destination).map_err(|source_error| {
        OpError::io("create install-step symlink", destination, source_error)
    })
}

fn create_parent(
    ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    path: &Utf8Path,
) -> Result<(), OpError> {
    let parent = path.parent().ok_or_else(|| {
        step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            "path has no parent",
        )
    })?;
    create_parents(ctx, resolver, step, journal, parent)
}

fn create_parents(
    _ctx: &Ctx,
    resolver: &Resolver<'_>,
    step: &Step,
    journal: &mut StepJournal,
    path: &Utf8Path,
) -> Result<(), OpError> {
    ensure_existing_ancestors_confined(resolver.env, path).map_err(|reason| {
        step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            &reason,
        )
    })?;
    let mut missing = Vec::new();
    let mut cursor = path;
    while !entry_exists(cursor) {
        missing.push(cursor.to_path_buf());
        cursor = cursor.parent().ok_or_else(|| {
            step_error_named(
                &resolver.formula.name,
                step.index,
                step.kind.type_name(),
                "directory escaped its root",
            )
        })?;
    }
    let metadata = fs::metadata(cursor)
        .map_err(|source| OpError::io("inspect install-step ancestor", cursor, source))?;
    if !metadata.is_dir() {
        return Err(step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            "existing ancestor is not a directory",
        ));
    }
    for directory in missing.into_iter().rev() {
        fs::create_dir(&directory)
            .map_err(|source| OpError::io("create install-step parent", &directory, source))?;
        journal.inverses.push(Inverse::Remove(directory));
    }
    Ok(())
}

fn resolved_checked(
    resolver: &Resolver<'_>,
    step: &Step,
    spec: &PathSpec,
) -> Result<Utf8PathBuf, OpError> {
    let path = resolver.resolve(step.index, step.kind.type_name(), spec)?;
    ensure_existing_ancestors_confined(resolver.env, &path).map_err(|reason| {
        step_error_named(
            &resolver.formula.name,
            step.index,
            step.kind.type_name(),
            &reason,
        )
    })?;
    Ok(path)
}

fn guards_match(
    resolver: &Resolver<'_>,
    step: &Step,
    cache: &mut BTreeMap<Guard, bool>,
) -> Result<bool, OpError> {
    for guard in &step.guards {
        let matches = if let Some(value) = cache.get(guard) {
            *value
        } else {
            let value = match guard {
                Guard::IfExists(spec) => {
                    entry_exists(&resolver.resolve(step.index, step.kind.type_name(), spec)?)
                }
                Guard::UnlessExists(spec) => {
                    !entry_exists(&resolver.resolve(step.index, step.kind.type_name(), spec)?)
                }
                Guard::On(Platform::Linux) => {
                    matches!(resolver.env.bottle_tag, BottleTag::Linux { .. })
                }
                Guard::On(Platform::MacOs) => {
                    matches!(resolver.env.bottle_tag, BottleTag::MacOs { .. })
                }
            };
            cache.insert(guard.clone(), value);
            value
        };
        if !matches {
            return Ok(false);
        }
    }
    Ok(true)
}

fn command_spec(
    resolver: &Resolver<'_>,
    index: usize,
    run: &RunStep,
) -> Result<CommandSpec, OpError> {
    let program = resolver.resolve(index, "run", &run.command)?;
    ensure_existing_ancestors_confined(resolver.env, &program)
        .map_err(|reason| step_error_named(&resolver.formula.name, index, "run", &reason))?;
    let args = run
        .args
        .iter()
        .map(|arg| expand_template(resolver, index, "run", arg))
        .collect::<Result<Vec<_>, _>>()?;
    let env = run
        .env
        .iter()
        .map(|(key, value)| {
            Ok((
                expand_template(resolver, index, "run", key)?,
                expand_template(resolver, index, "run", value)?,
            ))
        })
        .collect::<Result<Vec<_>, OpError>>()?;
    let mut spec = CommandSpec::new(program.as_std_path()).args(args).envs(env);
    if let Some(chdir) = &run.chdir {
        let cwd = resolver.resolve(index, "run", chdir)?;
        ensure_existing_ancestors_confined(resolver.env, &cwd)
            .map_err(|reason| step_error_named(&resolver.formula.name, index, "run", &reason))?;
        spec = spec.cwd(cwd.as_std_path());
    }
    Ok(spec)
}

fn maintenance_spec(
    resolver: &Resolver<'_>,
    index: usize,
    kind: MaintenanceKind,
    path: Option<&PathSpec>,
) -> Result<CommandSpec, OpError> {
    let (formula, executable, mut args): (&str, &str, Vec<String>) = match kind {
        MaintenanceKind::CompileGsettingsSchemas => ("glib", "glib-compile-schemas", Vec::new()),
        MaintenanceKind::GioQuerymodules => ("glib", "gio-querymodules", Vec::new()),
        MaintenanceKind::GdkPixbufQueryLoaders => (
            "gdk-pixbuf",
            "gdk-pixbuf-query-loaders",
            vec!["--update-cache".to_owned()],
        ),
        MaintenanceKind::GtkUpdateIconCache => {
            let gtk4 = resolver.env.prefix.join("opt/gtk4");
            if entry_exists(&gtk4) || resolver.catalog.get("gtk+3").is_none() {
                (
                    "gtk4",
                    "gtk4-update-icon-cache",
                    vec!["-q".to_owned(), "-t".to_owned(), "-f".to_owned()],
                )
            } else {
                (
                    "gtk+3",
                    "gtk3-update-icon-cache",
                    vec!["-q".to_owned(), "-t".to_owned(), "-f".to_owned()],
                )
            }
        }
        MaintenanceKind::UpdateMimeDatabase => {
            ("shared-mime-info", "update-mime-database", Vec::new())
        }
        MaintenanceKind::UpdateDesktopDatabase => {
            ("desktop-file-utils", "update-desktop-database", Vec::new())
        }
    };
    if let Some(path) = path {
        args.push(resolver.resolve(index, kind.type_name(), path)?.to_string());
    }
    let program = resolver
        .env
        .cellar
        .join(formula)
        .join(
            resolver
                .catalog
                .get(formula)
                .ok_or_else(|| {
                    step_error_named(
                        &resolver.formula.name,
                        index,
                        kind.type_name(),
                        &format!("maintenance formula {formula} is absent from catalog"),
                    )
                })?
                .pkg_version
                .to_string(),
        )
        .join("bin")
        .join(executable);
    ensure_existing_ancestors_confined(resolver.env, &program).map_err(|reason| {
        step_error_named(&resolver.formula.name, index, kind.type_name(), &reason)
    })?;
    Ok(CommandSpec::new(program.as_std_path()).args(args))
}

fn run_command(
    runner: &dyn CommandRunner,
    formula: &str,
    index: usize,
    spec: CommandSpec,
) -> Result<(), OpError> {
    let program = spec.program().to_string_lossy().into_owned();
    let output = runner.run(&spec).map_err(|source| {
        OpError::io(
            "run install-step command",
            Utf8PathBuf::from(program.clone()),
            source,
        )
    })?;
    if output.success() {
        return Ok(());
    }
    Err(OpError::CommandFailed {
        program: format!("{formula} post_install_steps[{index}] {program}"),
        status: output.status().to_string(),
        stderr: String::from_utf8_lossy(output.stderr()).into_owned(),
    })
}

fn expand_template(
    resolver: &Resolver<'_>,
    index: usize,
    type_name: &str,
    input: &str,
) -> Result<String, OpError> {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let token_start = start + 2;
        let tail = &rest[token_start..];
        let end = tail.find("}}").ok_or_else(|| {
            step_error_named(
                &resolver.formula.name,
                index,
                type_name,
                "unterminated template token",
            )
        })?;
        let token = &tail[..end];
        output.push_str(&template_value(resolver, index, type_name, token)?);
        rest = &tail[end + 2..];
    }
    if rest.contains("}}") {
        return Err(step_error_named(
            &resolver.formula.name,
            index,
            type_name,
            "unmatched template token terminator",
        ));
    }
    output.push_str(rest);
    Ok(output)
}

fn template_value(
    resolver: &Resolver<'_>,
    index: usize,
    type_name: &str,
    token: &str,
) -> Result<String, OpError> {
    let version = resolver.formula.pkg_version.to_string();
    let value = match token {
        "HOMEBREW_PREFIX" => resolver.env.prefix.to_string(),
        "HOMEBREW_CELLAR" => resolver.env.cellar.to_string(),
        "formula_name" | "name" | "token" => resolver.formula.name.clone(),
        "version" => version,
        "version.major" => version.split('.').next().unwrap_or_default().to_owned(),
        "version.major_minor" => version.split('.').take(2).collect::<Vec<_>>().join("."),
        "prefix" => resolver.keg.to_string(),
        "opt_prefix" => resolver
            .env
            .prefix
            .join("opt")
            .join(&resolver.formula.name)
            .to_string(),
        "bin" => resolver.keg.join("bin").to_string(),
        "sbin" => resolver.keg.join("sbin").to_string(),
        "lib" => resolver.keg.join("lib").to_string(),
        "libexec" => resolver.keg.join("libexec").to_string(),
        "share" => resolver.keg.join("share").to_string(),
        "pkgshare" => resolver
            .keg
            .join("share")
            .join(&resolver.formula.name)
            .to_string(),
        "include" => resolver.keg.join("include").to_string(),
        "etc" => resolver.env.prefix.join("etc").to_string(),
        "pkgetc" => resolver
            .env
            .prefix
            .join("etc")
            .join(&resolver.formula.name)
            .to_string(),
        "var" => resolver.env.prefix.join("var").to_string(),
        "rack" => resolver.env.cellar.join(&resolver.formula.name).to_string(),
        "bash_completion" => resolver.keg.join("etc/bash_completion.d").to_string(),
        "zsh_completion" => resolver.keg.join("share/zsh/site-functions").to_string(),
        "fish_completion" => resolver
            .keg
            .join("share/fish/vendor_completions.d")
            .to_string(),
        "pwsh_completion" => resolver.keg.join("share/pwsh/completions").to_string(),
        _ => {
            return Err(step_error_named(
                &resolver.formula.name,
                index,
                type_name,
                &format!("unknown template token {token}"),
            ));
        }
    };
    Ok(value)
}

fn expand_all_strings<'a>(
    resolver: &Resolver<'_>,
    index: usize,
    type_name: &str,
    values: impl Iterator<Item = &'a String>,
) -> Result<(), OpError> {
    for value in values {
        expand_template(resolver, index, type_name, value)?;
    }
    Ok(())
}

fn ensure_existing_ancestors_confined(env: &Env, path: &Utf8Path) -> Result<(), String> {
    let root = confinement_root(env, path)?;
    ensure_lexically_within(path, root)?;
    let mut cursor = path;
    while !entry_exists(cursor) {
        cursor = cursor
            .parent()
            .ok_or_else(|| format!("path {path} has no existing confined ancestor"))?;
    }
    let canonical = fs::canonicalize(cursor)
        .map_err(|error| format!("cannot canonicalize {cursor}: {error}"))?;
    let canonical = Utf8PathBuf::from_path_buf(canonical)
        .map_err(|_| format!("canonical path for {cursor} is not UTF-8"))?;
    let canonical_root = canonical_existing_root(root)?;
    if !canonical.starts_with(&canonical_root) {
        return Err(format!(
            "symlink ancestor escapes allowed root {root}: {cursor} -> {canonical}"
        ));
    }
    Ok(())
}

fn confinement_root<'a>(env: &'a Env, path: &Utf8Path) -> Result<&'a Utf8Path, String> {
    if path.starts_with(&env.cellar) {
        return Ok(&env.cellar);
    }
    if path.starts_with(&env.prefix) {
        return Ok(&env.prefix);
    }
    Err(format!(
        "path {path} escapes allowed roots {} and {}",
        env.prefix, env.cellar
    ))
}

fn canonical_existing_root(root: &Utf8Path) -> Result<Utf8PathBuf, String> {
    let mut cursor = root;
    while !entry_exists(cursor) {
        cursor = cursor
            .parent()
            .ok_or_else(|| format!("root {root} has no existing ancestor"))?;
    }
    let canonical = fs::canonicalize(cursor)
        .map_err(|error| format!("cannot canonicalize root {cursor}: {error}"))?;
    Utf8PathBuf::from_path_buf(canonical)
        .map_err(|_| format!("canonical root {cursor} is not UTF-8"))
}

fn ensure_lexically_within(path: &Utf8Path, root: &Utf8Path) -> Result<(), String> {
    if !path.starts_with(root) {
        return Err(format!("path {path} escapes allowed root {root}"));
    }
    for component in path.components() {
        if matches!(component, Utf8Component::ParentDir) {
            return Err(format!("path {path} contains .."));
        }
    }
    Ok(())
}

fn allowed_keys(
    formula: &Formula,
    index: usize,
    type_name: &str,
    object: &Map<String, Value>,
    allowed: &[&str],
) -> Result<(), OpError> {
    let allowed: BTreeSet<_> = allowed.iter().copied().collect();
    if let Some(key) = object.keys().find(|key| !allowed.contains(key.as_str())) {
        return Err(step_error(
            formula,
            index,
            type_name,
            &format!("unknown field {key}"),
        ));
    }
    Ok(())
}

fn required_string(
    formula: &Formula,
    index: usize,
    type_name: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<String, OpError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            step_error(
                formula,
                index,
                type_name,
                &format!("{key} must be a string"),
            )
        })
}

fn optional_string(
    formula: &Formula,
    index: usize,
    type_name: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, OpError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(step_error(
            formula,
            index,
            type_name,
            &format!("{key} must be a string"),
        )),
    }
}

fn optional_bool(
    formula: &Formula,
    index: usize,
    type_name: &str,
    object: &Map<String, Value>,
    key: &str,
    default: bool,
) -> Result<bool, OpError> {
    match object.get(key) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(step_error(
            formula,
            index,
            type_name,
            &format!("{key} must be a boolean"),
        )),
    }
}

fn reject_sudo(
    formula: &Formula,
    index: usize,
    type_name: &str,
    object: &Map<String, Value>,
) -> Result<(), OpError> {
    match object.get("sudo") {
        None | Some(Value::Bool(false)) => Ok(()),
        Some(_) => Err(step_error(
            formula,
            index,
            type_name,
            "sudo install steps are not supported",
        )),
    }
}

fn step_error(formula: &Formula, index: usize, type_name: &str, reason: &str) -> OpError {
    step_error_named(&formula.name, index, type_name, reason)
}

fn step_error_named(formula: &str, index: usize, type_name: &str, reason: &str) -> OpError {
    OpError::InstallStep {
        formula: formula.to_owned(),
        index,
        step_type: type_name.to_owned(),
        reason: reason.to_owned(),
    }
}

fn entry_exists(path: &Utf8Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn path_ancestor_rack(env: &Env, formula: &str) -> Utf8PathBuf {
    env.cellar.join(formula)
}

fn create_owned_directory(parent: &Utf8Path, stem: &str) -> Result<Utf8PathBuf, OpError> {
    static ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    loop {
        let id = ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = parent.join(format!("{stem}-{}-{id}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(OpError::io(
                    "create owned install-step journal",
                    path,
                    source,
                ));
            }
        }
    }
}

fn destination_for(source: &Utf8Path, target: &Utf8Path) -> Utf8PathBuf {
    if fs::symlink_metadata(target).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        target.join(source.file_name().unwrap_or_default())
    } else {
        target.to_path_buf()
    }
}

fn copy_entry(source: &Utf8Path, destination: &Utf8Path, recursive: bool) -> Result<(), OpError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| OpError::io("inspect install-step copy source", source, error))?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)
            .map_err(|error| OpError::io("read install-step copy symlink", source, error))?;
        return symlink(target, destination)
            .map_err(|error| OpError::io("copy install-step symlink", destination, error));
    }
    if metadata.file_type().is_dir() {
        if !recursive {
            return Err(OpError::InvalidState {
                reason: format!("copy source {source} is a directory but recursive is false"),
            });
        }
        fs::create_dir(destination).map_err(|error| {
            OpError::io("create install-step copy directory", destination, error)
        })?;
        fs::set_permissions(destination, metadata.permissions())
            .map_err(|error| OpError::io("set copied directory permissions", destination, error))?;
        for child in read_dir_sorted(source)? {
            let name = child.file_name().ok_or_else(|| OpError::InvalidState {
                reason: format!("copy child {child} has no name"),
            })?;
            copy_entry(&child, &destination.join(name), true)?;
        }
        return Ok(());
    }
    fs::copy(source, destination)
        .map_err(|error| OpError::io("copy install-step file", destination, error))?;
    Ok(())
}

fn read_dir_sorted(path: &Utf8Path) -> Result<Vec<Utf8PathBuf>, OpError> {
    let entries = fs::read_dir(path)
        .map_err(|source| OpError::io("read install-step directory", path, source))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|source| OpError::io("read install-step directory entry", path, source))?;
        paths.push(Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| {
            OpError::InvalidState {
                reason: format!("install-step path is not UTF-8: {}", path.display()),
            }
        })?);
    }
    paths.sort();
    Ok(paths)
}

fn expand_glob(pattern: &Utf8Path) -> Result<Vec<Utf8PathBuf>, OpError> {
    if !pattern.as_str().contains(['*', '?', '[']) {
        return Ok(if entry_exists(pattern) {
            vec![pattern.to_path_buf()]
        } else {
            Vec::new()
        });
    }
    let mut base = Utf8PathBuf::new();
    let mut candidates = vec![Utf8PathBuf::new()];
    for component in pattern.components() {
        let text = component.as_str();
        if candidates.len() == 1
            && candidates[0].as_str().is_empty()
            && !text.contains(['*', '?', '['])
        {
            base.push(text);
            candidates[0] = base.clone();
            continue;
        }
        let mut next = Vec::new();
        for candidate in candidates {
            if text.contains(['*', '?', '[']) {
                if !entry_exists(&candidate) {
                    continue;
                }
                for child in read_dir_sorted(&candidate)? {
                    if child
                        .file_name()
                        .is_some_and(|name| wildcard_match(text, name))
                    {
                        next.push(child);
                    }
                }
            } else {
                let child = candidate.join(text);
                if entry_exists(&child) {
                    next.push(child);
                }
            }
        }
        candidates = next;
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut p = 0;
    let mut v = 0;
    let mut star = None;
    let mut retry = 0;
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'[' {
            let Some(end) = pattern[p + 1..]
                .iter()
                .position(|byte| *byte == b']')
                .map(|offset| p + 1 + offset)
            else {
                return false;
            };
            if class_matches(&pattern[p + 1..end], value[v]) {
                p = end + 1;
                v += 1;
            } else if let Some(star_index) = star {
                p = star_index + 1;
                retry += 1;
                v = retry;
            } else {
                return false;
            }
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = v;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            retry += 1;
            v = retry;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn class_matches(class: &[u8], value: u8) -> bool {
    let (negated, class) = if class
        .first()
        .is_some_and(|byte| matches!(byte, b'!' | b'^'))
    {
        (true, &class[1..])
    } else {
        (false, class)
    };
    let mut matched = false;
    let mut index = 0;
    while index < class.len() {
        if index + 2 < class.len() && class[index + 1] == b'-' {
            matched |= class[index] <= value && value <= class[index + 2];
            index += 3;
        } else {
            matched |= class[index] == value;
            index += 1;
        }
    }
    matched != negated
}

fn apply_inverse(env: &Env, inverse: &Inverse) -> Result<(), OpError> {
    match inverse {
        Inverse::Remove(path) => {
            if entry_exists(path) {
                remove_entry_confined(env, path)?;
            }
            Ok(())
        }
        Inverse::Replace { path, backup } => {
            if entry_exists(path) {
                remove_entry_confined(env, path)?;
            }
            if entry_exists(backup) {
                fs::rename(backup, path).map_err(|source| {
                    OpError::io("restore replaced install-step path", backup, source)
                })?;
            }
            Ok(())
        }
        Inverse::Restore { path, backup } => {
            if entry_exists(backup) {
                fs::rename(backup, path).map_err(|source| {
                    OpError::io("restore removed install-step path", backup, source)
                })?;
            }
            Ok(())
        }
        Inverse::MoveBack {
            source,
            destination,
        } => {
            if entry_exists(destination) {
                fs::rename(destination, source).map_err(|error| {
                    OpError::io("reverse install-step move", destination, error)
                })?;
            }
            Ok(())
        }
        Inverse::Mode { path, mode } => {
            if entry_exists(path) {
                fs::set_permissions(path, fs::Permissions::from_mode(*mode)).map_err(|source| {
                    OpError::io("restore install-step permissions", path, source)
                })?;
            }
            Ok(())
        }
    }
}

fn inverse_leftovers(inverse: &Inverse, leftovers: &mut Vec<Utf8PathBuf>) {
    match inverse {
        Inverse::Remove(path) | Inverse::Mode { path, .. } => {
            if entry_exists(path) {
                leftovers.push(path.clone());
            }
        }
        Inverse::Replace { path, backup } | Inverse::Restore { path, backup } => {
            if entry_exists(path) {
                leftovers.push(path.clone());
            }
            if entry_exists(backup) {
                leftovers.push(backup.clone());
            }
        }
        Inverse::MoveBack {
            source,
            destination,
        } => {
            if entry_exists(source) {
                leftovers.push(source.clone());
            }
            if entry_exists(destination) {
                leftovers.push(destination.clone());
            }
        }
    }
}

fn remove_entry_confined(env: &Env, path: &Utf8Path) -> Result<(), OpError> {
    let root = confinement_root(env, path).map_err(|reason| OpError::InvalidState { reason })?;
    ensure_lexically_within(path, root).map_err(|reason| OpError::InvalidState { reason })?;
    let parent = path.parent().ok_or_else(|| OpError::InvalidState {
        reason: format!("rollback path {path} has no parent"),
    })?;
    ensure_existing_ancestors_confined(env, parent)
        .map_err(|reason| OpError::InvalidState { reason })?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| OpError::io("inspect install-step rollback path", path, source))?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
            .map_err(|source| OpError::io("remove install-step rollback tree", path, source))
    } else {
        fs::remove_file(path)
            .map_err(|source| OpError::io("remove install-step rollback path", path, source))
    }
}

pub(crate) fn remove_tree_confined(env: &Env, path: &Utf8Path) -> Result<(), OpError> {
    ensure_existing_ancestors_confined(env, path)
        .map_err(|reason| OpError::InvalidState { reason })?;
    fs::remove_dir_all(path)
        .map_err(|source| OpError::io("remove install-step journal", path, source))
}
