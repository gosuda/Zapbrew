use std::collections::BTreeMap;
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use plist::Value as PlistValue;
use serde_json::{Map, Value};
use zapbrew_prefix::{CommandSpec, Env};
use zapbrew_types::BottleTag;

use crate::platform::{
    LaunchctlAction, SystemctlAction, launchctl, launchctl_start, launchctl_stop, run_checked,
    systemctl, systemctl_daemon_reload, systemctl_is_active,
};
use crate::state::{self, InstalledState};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub action: ServiceAction,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    List,
    Start,
    Stop,
    Restart,
    Run,
    Info,
    Kill,
    Cleanup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Schedule {
    Immediate,
    Interval(u64),
    Cron(Cron),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cron {
    minute: CronField,
    hour: CronField,
    day: CronField,
    month: CronField,
    weekday: CronField,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CronField {
    Any,
    /// Sorted, deduplicated, non-empty; every element within the field's bounds.
    Set(Vec<u32>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CronKind {
    Minute,
    Hour,
    Day,
    Month,
    Weekday,
}

impl CronKind {
    fn bounds(self) -> (u32, u32) {
        match self {
            CronKind::Minute => (0, 59),
            CronKind::Hour => (0, 23),
            CronKind::Day => (1, 31),
            CronKind::Month => (1, 12),
            CronKind::Weekday => (0, 7),
        }
    }

    fn systemd_padded(self) -> bool {
        matches!(self, CronKind::Minute | CronKind::Hour)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartMode {
    Failure,
    Success,
}

#[derive(Debug, Clone, PartialEq)]
struct ServiceConfig {
    run: Vec<String>,
    schedule: Schedule,
    restart_mode: Option<RestartMode>,
    plist_keep_alive: Option<PlistValue>,
    launch_only_once: bool,
    environment: BTreeMap<String, String>,
    working_dir: Option<String>,
    root_dir: Option<String>,
    input_path: Option<String>,
    log_path: Option<String>,
    error_log_path: Option<String>,
    restart_delay: Option<u64>,
    stop_timeout: Option<u64>,
    nice: Option<i64>,
}

impl ServiceConfig {
    fn timed(&self) -> bool {
        !matches!(self.schedule, Schedule::Immediate)
    }
}

struct Target {
    name: String,
    config: ServiceConfig,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    if args.action == ServiceAction::List {
        return list(ctx);
    }
    if args.action == ServiceAction::Cleanup {
        return cleanup(ctx);
    }

    let installed = state::scan(&ctx.env)?;
    for name in args.names {
        let target = target(ctx, &installed, &name)?;
        match args.action {
            ServiceAction::List => unreachable!("list returned before target resolution"),
            ServiceAction::Cleanup => unreachable!("cleanup returned before target resolution"),
            ServiceAction::Start => start(ctx, &target)?,
            ServiceAction::Stop => stop(ctx, &target)?,
            ServiceAction::Restart => {
                stop(ctx, &target)?;
                start(ctx, &target)?;
            }
            ServiceAction::Run => run_service(ctx, &target)?,
            ServiceAction::Info => info(ctx, &target)?,
            ServiceAction::Kill => kill(ctx, &target)?,
        }
    }
    Ok(())
}

fn target(ctx: &Ctx, installed: &InstalledState, name: &str) -> Result<Target, OpError> {
    if !installed.contains(name) {
        return Err(refusal(format!("Formula `{name}` is not installed.")));
    }
    let service = ctx
        .catalog
        .get(name)
        .and_then(|formula| formula.service.as_ref())
        .ok_or_else(|| refusal(format!("Formula `{name}` has no service definition.")))?;
    Ok(Target {
        name: name.to_owned(),
        config: parse_service(&ctx.env, name, service)?,
    })
}

fn list(ctx: &Ctx) -> Result<(), OpError> {
    let installed = state::scan(&ctx.env)?;
    ctx.reporter.print("Name Status File");
    for formula in installed.iter() {
        let name = formula.name().name();
        let Some(service) = ctx
            .catalog
            .get(name)
            .and_then(|catalog_formula| catalog_formula.service.as_ref())
        else {
            continue;
        };
        let config = parse_service(&ctx.env, name, service)?;
        let file = service_file(&ctx.env, name)?;
        let started = match &ctx.env.bottle_tag {
            BottleTag::Linux { .. } => {
                let unit = if config.timed() {
                    timer_unit(name)
                } else {
                    service_unit(name)
                };
                probe(ctx, &systemctl_is_active(&unit))?
            }
            BottleTag::MacOs { .. } => probe(ctx, &launchctl_list(&plist_label(name)))?,
            BottleTag::All => return Err(unsupported_platform()),
        };
        let status = if started {
            "started"
        } else if file.exists() {
            "stopped"
        } else {
            "none"
        };
        ctx.reporter.print(&format!("{name} {status} {file}"));
    }
    Ok(())
}

fn start(ctx: &Ctx, target: &Target) -> Result<(), OpError> {
    let active = active(ctx, target)?;
    if active {
        ctx.reporter.print(&format!(
            "Service `{}` already started, use {} restart {} to restart.",
            target.name,
            ctx.reporter.hint_program(),
            target.name
        ));
        return Ok(());
    }

    match &ctx.env.bottle_tag {
        BottleTag::Linux { .. } => {
            let unit_path = systemd_dir(&ctx.env).join(service_unit(&target.name));
            write_file(
                &unit_path,
                render_systemd_unit(&target.name, &target.config),
            )?;
            let unit = if target.config.timed() {
                let timer_path = systemd_dir(&ctx.env).join(timer_unit(&target.name));
                write_file(
                    &timer_path,
                    render_systemd_timer(&target.name, &target.config)?,
                )?;
                timer_unit(&target.name)
            } else {
                service_unit(&target.name)
            };
            run_checked(ctx.commands.as_ref(), &systemctl_daemon_reload())?;
            run_checked(
                ctx.commands.as_ref(),
                &systemctl(SystemctlAction::Start, &unit),
            )?;
            started(ctx, target, &service_label(&target.name));
        }
        BottleTag::MacOs { .. } => {
            let path = launch_agents_dir(&ctx.env).join(plist_file_name(&target.name));
            write_file(
                &path,
                render_launchd_plist(&ctx.env, &target.name, &target.config)?,
            )?;
            run_checked(
                ctx.commands.as_ref(),
                &launchctl(LaunchctlAction::Load, &path),
            )?;
            started(ctx, target, &plist_label(&target.name));
        }
        BottleTag::All => return Err(unsupported_platform()),
    }
    Ok(())
}

fn stop(ctx: &Ctx, target: &Target) -> Result<(), OpError> {
    if !active(ctx, target)? {
        ctx.reporter
            .opoo(&format!("Service `{}` is not started.", target.name));
        return Ok(());
    }

    match &ctx.env.bottle_tag {
        BottleTag::Linux { .. } => {
            let unit = if target.config.timed() {
                timer_unit(&target.name)
            } else {
                service_unit(&target.name)
            };
            run_checked(
                ctx.commands.as_ref(),
                &systemctl(SystemctlAction::Stop, &unit),
            )?;
            stopped(ctx, target, &service_label(&target.name));
        }
        BottleTag::MacOs { .. } => {
            let path = launch_agents_dir(&ctx.env).join(plist_file_name(&target.name));
            run_checked(
                ctx.commands.as_ref(),
                &launchctl(LaunchctlAction::Unload, &path),
            )?;
            stopped(ctx, target, &plist_label(&target.name));
        }
        BottleTag::All => return Err(unsupported_platform()),
    }
    Ok(())
}

fn run_service(ctx: &Ctx, target: &Target) -> Result<(), OpError> {
    let active = active(ctx, target)?;
    if active {
        ctx.reporter.print(&format!(
            "Service `{}` already running, use {} restart {} to restart.",
            target.name,
            ctx.reporter.hint_program(),
            target.name
        ));
        return Ok(());
    }

    match &ctx.env.bottle_tag {
        BottleTag::Linux { .. } => {
            let unit_path = systemd_dir(&ctx.env).join(service_unit(&target.name));
            write_file(
                &unit_path,
                render_systemd_unit(&target.name, &target.config),
            )?;
            let unit = if target.config.timed() {
                let timer_path = systemd_dir(&ctx.env).join(timer_unit(&target.name));
                write_file(
                    &timer_path,
                    render_systemd_timer(&target.name, &target.config)?,
                )?;
                timer_unit(&target.name)
            } else {
                service_unit(&target.name)
            };
            run_checked(ctx.commands.as_ref(), &systemctl_daemon_reload())?;
            run_checked(
                ctx.commands.as_ref(),
                &systemctl(SystemctlAction::Start, &unit),
            )?;
            ran(ctx, target, &service_label(&target.name));
        }
        BottleTag::MacOs { .. } => {
            let path = launch_agents_dir(&ctx.env).join(plist_file_name(&target.name));
            write_file(
                &path,
                render_launchd_plist(&ctx.env, &target.name, &target.config)?,
            )?;
            run_checked(
                ctx.commands.as_ref(),
                &launchctl(LaunchctlAction::Load, &path),
            )?;
            run_checked(
                ctx.commands.as_ref(),
                &launchctl_start(&plist_label(&target.name)),
            )?;
            ran(ctx, target, &plist_label(&target.name));
        }
        BottleTag::All => return Err(unsupported_platform()),
    }
    Ok(())
}

fn info(ctx: &Ctx, target: &Target) -> Result<(), OpError> {
    let file = service_file(&ctx.env, &target.name)?;
    let running = active(ctx, target)?;
    let status = if running {
        "running"
    } else if file.exists() {
        "stopped"
    } else {
        "none"
    };
    ctx.reporter
        .print(&format!("{} {} {}", target.name, status, file));
    Ok(())
}

fn kill(ctx: &Ctx, target: &Target) -> Result<(), OpError> {
    if !active(ctx, target)? {
        ctx.reporter
            .print(&format!("Service `{}` is not started.", target.name));
        return Ok(());
    }

    match &ctx.env.bottle_tag {
        BottleTag::Linux { .. } => {
            let unit = if target.config.timed() {
                timer_unit(&target.name)
            } else {
                service_unit(&target.name)
            };
            run_checked(
                ctx.commands.as_ref(),
                &systemctl(SystemctlAction::Stop, &unit),
            )?;
            killed(ctx, target, &service_label(&target.name));
        }
        BottleTag::MacOs { .. } => {
            run_checked(
                ctx.commands.as_ref(),
                &launchctl_stop(&plist_label(&target.name)),
            )?;
            killed(ctx, target, &plist_label(&target.name));
        }
        BottleTag::All => return Err(unsupported_platform()),
    }
    Ok(())
}

fn cleanup(ctx: &Ctx) -> Result<(), OpError> {
    let installed = state::scan(&ctx.env)?;
    let mut cleaned = false;
    match &ctx.env.bottle_tag {
        BottleTag::Linux { .. } => {
            let dir = systemd_dir(&ctx.env);
            if dir.exists() {
                let entries = fs::read_dir(&dir)
                    .map_err(|source| OpError::io("read service directory", dir.clone(), source))?;
                for entry in entries {
                    let entry = entry
                        .map_err(|source| OpError::io("read service entry", dir.clone(), source))?;
                    let path = utf8_entry_path(&entry, &dir)?;
                    let Some(name) = formula_name_from_systemd_file(&path) else {
                        continue;
                    };
                    if installed.contains(&name) {
                        continue;
                    }
                    let unit = path.file_name().expect("file name").to_owned();
                    if probe(ctx, &systemctl_is_active(&unit))? {
                        continue;
                    }
                    ctx.reporter
                        .print(&format!("Removing unused service file: {path}"));
                    fs::remove_file(&path).map_err(|source| {
                        OpError::io("remove service file", path.clone(), source)
                    })?;
                    cleaned = true;
                }
            }
            if cleaned {
                run_checked(ctx.commands.as_ref(), &systemctl_daemon_reload())?;
            }
        }
        BottleTag::MacOs { .. } => {
            let dir = launch_agents_dir(&ctx.env);
            if dir.exists() {
                let entries = fs::read_dir(&dir)
                    .map_err(|source| OpError::io("read service directory", dir.clone(), source))?;
                for entry in entries {
                    let entry = entry
                        .map_err(|source| OpError::io("read service entry", dir.clone(), source))?;
                    let path = utf8_entry_path(&entry, &dir)?;
                    let Some(name) = formula_name_from_plist(&path) else {
                        continue;
                    };
                    if installed.contains(&name) {
                        continue;
                    }
                    if probe(ctx, &launchctl_list(&plist_label(&name)))? {
                        continue;
                    }
                    run_checked(
                        ctx.commands.as_ref(),
                        &launchctl(LaunchctlAction::Unload, &path),
                    )?;
                    ctx.reporter
                        .print(&format!("Removing unused service file: {path}"));
                    fs::remove_file(&path).map_err(|source| {
                        OpError::io("remove service file", path.clone(), source)
                    })?;
                    cleaned = true;
                }
            }
        }
        BottleTag::All => return Err(unsupported_platform()),
    }
    if !cleaned {
        ctx.reporter
            .print("All user-space services OK, nothing cleaned...");
    }
    Ok(())
}

fn active(ctx: &Ctx, target: &Target) -> Result<bool, OpError> {
    match &ctx.env.bottle_tag {
        BottleTag::Linux { .. } => {
            let unit = if target.config.timed() {
                timer_unit(&target.name)
            } else {
                service_unit(&target.name)
            };
            probe(ctx, &systemctl_is_active(&unit))
        }
        BottleTag::MacOs { .. } => probe(ctx, &launchctl_list(&plist_label(&target.name))),
        BottleTag::All => Err(unsupported_platform()),
    }
}

fn probe(ctx: &Ctx, spec: &CommandSpec) -> Result<bool, OpError> {
    let program = spec.program().to_string_lossy().into_owned();
    ctx.commands
        .run(spec)
        .map(|output| output.success())
        .map_err(|source| OpError::io("run", program, source))
}

fn started(ctx: &Ctx, target: &Target, label: &str) {
    ctx.reporter.ohai(&format!(
        "Successfully started `{}` (label: {label})",
        target.name
    ));
}

fn stopped(ctx: &Ctx, target: &Target, label: &str) {
    ctx.reporter.ohai(&format!(
        "Successfully stopped `{}` (label: {label})",
        target.name
    ));
}

fn ran(ctx: &Ctx, target: &Target, label: &str) {
    ctx.reporter.ohai(&format!(
        "Successfully ran `{}` (label: {label})",
        target.name
    ));
}

fn killed(ctx: &Ctx, target: &Target, label: &str) {
    ctx.reporter.ohai(&format!(
        "Successfully killed `{}` (label: {label})",
        target.name
    ));
}

fn systemd_dir(env: &Env) -> Utf8PathBuf {
    env.home.join(".config/systemd/user")
}

fn launch_agents_dir(env: &Env) -> Utf8PathBuf {
    env.home.join("Library/LaunchAgents")
}

fn service_file(env: &Env, name: &str) -> Result<Utf8PathBuf, OpError> {
    match &env.bottle_tag {
        BottleTag::Linux { .. } => Ok(systemd_dir(env).join(service_unit(name))),
        BottleTag::MacOs { .. } => Ok(launch_agents_dir(env).join(plist_file_name(name))),
        BottleTag::All => Err(unsupported_platform()),
    }
}

fn service_label(name: &str) -> String {
    format!("homebrew.{name}")
}

fn service_unit(name: &str) -> String {
    format!("{}.service", service_label(name))
}

fn timer_unit(name: &str) -> String {
    format!("{}.timer", service_label(name))
}

fn plist_label(name: &str) -> String {
    format!("homebrew.mxcl.{name}")
}

fn plist_file_name(name: &str) -> String {
    format!("{}.plist", plist_label(name))
}

fn launchctl_list(label: &str) -> CommandSpec {
    CommandSpec::new("launchctl").arg("list").arg(label)
}

/// Convert a `fs::read_dir` entry to a UTF-8 path, refusing non-UTF-8 names.
fn utf8_entry_path(entry: &fs::DirEntry, dir: &Utf8Path) -> Result<Utf8PathBuf, OpError> {
    let file_name = entry.file_name();
    let name = file_name.to_str().ok_or_else(|| OpError::InvalidState {
        reason: format!("non-UTF-8 service file name in {dir}"),
    })?;
    Ok(dir.join(name))
}

/// Extract the formula name from a `homebrew.<name>.service` or `.timer` file.
fn formula_name_from_systemd_file(path: &Utf8Path) -> Option<String> {
    let file_name = path.file_name()?;
    let stem = file_name.strip_prefix("homebrew.")?;
    if let Some(name) = stem.strip_suffix(".service") {
        return Some(name.to_owned());
    }
    stem.strip_suffix(".timer").map(|name| name.to_owned())
}

/// Extract the formula name from a `homebrew.mxcl.<name>.plist` file.
fn formula_name_from_plist(path: &Utf8Path) -> Option<String> {
    let file_name = path.file_name()?;
    let stem = file_name.strip_prefix("homebrew.mxcl.")?;
    stem.strip_suffix(".plist").map(|name| name.to_owned())
}

fn write_file(path: &Utf8Path, contents: String) -> Result<(), OpError> {
    let parent = path.parent().ok_or_else(|| OpError::InvalidState {
        reason: format!("service file has no parent: {path}"),
    })?;
    fs::create_dir_all(parent)
        .map_err(|source| OpError::io("create service directory", parent.to_path_buf(), source))?;
    fs::write(path, contents)
        .map_err(|source| OpError::io("write service file", path.to_path_buf(), source))
}

fn parse_service(env: &Env, name: &str, value: &Value) -> Result<ServiceConfig, OpError> {
    let object = value.as_object().ok_or_else(|| {
        refusal(format!(
            "Formula `{name}` has unsupported service data; expected an object."
        ))
    })?;
    let run = parse_run(env, name, object.get("run"))?;
    let schedule = parse_schedule(name, object)?;
    let (restart_mode, plist_keep_alive) = parse_keep_alive(name, object.get("keep_alive"))?;
    let environment = parse_environment(env, name, object.get("environment_variables"))?;

    Ok(ServiceConfig {
        run,
        schedule,
        restart_mode,
        plist_keep_alive,
        launch_only_once: bool_field(name, object, "launch_only_once")?.unwrap_or(false),
        environment,
        working_dir: path_field(env, name, object, "working_dir")?,
        root_dir: path_field(env, name, object, "root_dir")?,
        input_path: path_field(env, name, object, "input_path")?,
        log_path: path_field(env, name, object, "log_path")?,
        error_log_path: path_field(env, name, object, "error_log_path")?,
        restart_delay: unsigned_field(name, object, "restart_delay")?,
        stop_timeout: unsigned_field(name, object, "stop_timeout")?,
        nice: signed_field(name, object, "nice")?,
    })
}

fn parse_run(env: &Env, name: &str, value: Option<&Value>) -> Result<Vec<String>, OpError> {
    let value = value.ok_or_else(|| {
        refusal(format!(
            "Formula `{name}` has no runnable command in its service definition."
        ))
    })?;
    match value {
        Value::String(command) => Ok(vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            substitute(env, name, command)?,
        ]),
        Value::Array(arguments) => {
            if arguments.is_empty() {
                return Err(refusal(format!(
                    "Formula `{name}` has an empty service run command."
                )));
            }
            arguments
                .iter()
                .map(|argument| {
                    argument.as_str().ok_or_else(|| {
                        refusal(format!(
                            "Formula `{name}` has a non-string service run argument."
                        ))
                    })
                })
                .map(|argument| argument.and_then(|argument| substitute(env, name, argument)))
                .collect()
        }
        Value::Object(_) => Err(refusal(format!(
            "Formula `{name}` uses an unsupported service run object."
        ))),
        _ => Err(refusal(format!(
            "Formula `{name}` has an unsupported service run value."
        ))),
    }
}

fn parse_schedule(name: &str, object: &Map<String, Value>) -> Result<Schedule, OpError> {
    let run_type = object
        .get("run_type")
        .map(|value| {
            value.as_str().ok_or_else(|| {
                refusal(format!(
                    "Formula `{name}` has a non-string service run_type."
                ))
            })
        })
        .transpose()?
        .unwrap_or("immediate");
    match run_type {
        "immediate" => Ok(Schedule::Immediate),
        "interval" => unsigned_field(name, object, "interval")?
            .map(Schedule::Interval)
            .ok_or_else(|| refusal(format!("Formula `{name}` has no service interval."))),
        "cron" => string_field(name, object, "cron")?
            .ok_or_else(|| refusal(format!("Formula `{name}` has no service cron schedule.")))
            .and_then(|cron| parse_cron(name, &cron).map(Schedule::Cron)),
        other => Err(refusal(format!(
            "Formula `{name}` has unsupported service run_type `{other}`."
        ))),
    }
}

fn parse_cron(name: &str, value: &str) -> Result<Cron, OpError> {
    let invalid = || {
        refusal(format!(
            "Formula `{name}` has invalid service cron schedule `{value}`."
        ))
    };
    let fields = value.split_whitespace().collect::<Vec<_>>();
    let [minute, hour, day, month, weekday] = fields.as_slice() else {
        return Err(invalid());
    };
    let cron = Cron {
        minute: parse_cron_field(CronKind::Minute, minute).ok_or_else(invalid)?,
        hour: parse_cron_field(CronKind::Hour, hour).ok_or_else(invalid)?,
        day: parse_cron_field(CronKind::Day, day).ok_or_else(invalid)?,
        month: parse_cron_field(CronKind::Month, month).ok_or_else(invalid)?,
        weekday: parse_cron_field(CronKind::Weekday, weekday).ok_or_else(invalid)?,
    };
    if cron_combinations(&cron).is_none_or(|count| count > MAX_CRON_COMBINATIONS) {
        return Err(refusal(format!(
            "Formula `{name}` has a service cron schedule `{value}` that expands too broadly."
        )));
    }
    Ok(cron)
}

/// Upper bound on launchd `StartCalendarInterval` combinations, enforced before
/// any Cartesian expansion is allocated.
const MAX_CRON_COMBINATIONS: u64 = 4096;

/// Number of launchd interval dictionaries this cron expands to (the product of
/// each non-`Any` field's set length), or `None` on overflow.
fn cron_combinations(cron: &Cron) -> Option<u64> {
    [
        &cron.minute,
        &cron.hour,
        &cron.day,
        &cron.month,
        &cron.weekday,
    ]
    .into_iter()
    .try_fold(1u64, |total, field| match field {
        CronField::Any => Some(total),
        CronField::Set(values) => total.checked_mul(values.len() as u64),
    })
}

/// Parse a single cron field into `Any` or a validated `Set`. Returns `None` for
/// any malformed token, out-of-range value, empty term, bare `*` inside a list,
/// descending range, or zero step.
fn parse_cron_field(kind: CronKind, token: &str) -> Option<CronField> {
    if token == "*" {
        return Some(CronField::Any);
    }
    let (min, max) = kind.bounds();
    let mut values = Vec::new();
    for term in token.split(',') {
        parse_cron_term(term, min, max, &mut values)?;
    }
    values.sort_unstable();
    values.dedup();
    // `split(',')` yields at least one term and every accepted term pushes at
    // least one value, so the set is non-empty here.
    Some(CronField::Set(values))
}

/// Expand one comma-separated cron term (`A`, `A-B`, `*/N`, `A-B/N`, `A/N`) into
/// `out`, enforcing bounds. Returns `None` on any malformed or out-of-range term.
fn parse_cron_term(term: &str, min: u32, max: u32, out: &mut Vec<u32>) -> Option<()> {
    if term.is_empty() {
        return None;
    }
    let (base, step, has_step) = match term.split_once('/') {
        Some((base, step)) => {
            let step = step.parse::<u32>().ok()?;
            if step == 0 {
                return None;
            }
            (base, step, true)
        }
        None => (term, 1, false),
    };
    if base == "*" {
        // A bare `*` term only reaches here inside a list; the whole-field `*`
        // was already handled. Accept it only as the `*/N` wildcard-step form.
        if !has_step {
            return None;
        }
        return push_cron_range(out, min, max, step);
    }
    if let Some((start, end)) = base.split_once('-') {
        let start = start.parse::<u32>().ok()?;
        let end = end.parse::<u32>().ok()?;
        if start > end || start < min || end > max {
            return None;
        }
        return push_cron_range(out, start, end, step);
    }
    let single = base.parse::<u32>().ok()?;
    if single < min || single > max {
        return None;
    }
    if has_step {
        return push_cron_range(out, single, max, step);
    }
    out.push(single);
    Some(())
}

fn push_cron_range(out: &mut Vec<u32>, start: u32, end: u32, step: u32) -> Option<()> {
    let mut value = start;
    while value <= end {
        out.push(value);
        value = value.checked_add(step)?;
    }
    Some(())
}

fn parse_keep_alive(
    name: &str,
    value: Option<&Value>,
) -> Result<(Option<RestartMode>, Option<PlistValue>), OpError> {
    let Some(value) = value else {
        return Ok((None, None));
    };
    match value {
        Value::Bool(false) => Ok((None, None)),
        Value::Bool(true) => Ok((Some(RestartMode::Failure), Some(PlistValue::Boolean(true)))),
        Value::Object(object) => {
            if let Some(always) = object.get("always").and_then(Value::as_bool) {
                return if always {
                    Ok((Some(RestartMode::Failure), Some(PlistValue::Boolean(true))))
                } else {
                    Ok((None, None))
                };
            }
            for (json_key, plist_key, mode) in [
                ("successful_exit", "SuccessfulExit", RestartMode::Success),
                ("crashed", "Crashed", RestartMode::Failure),
            ] {
                if let Some(flag) = object.get(json_key).and_then(Value::as_bool) {
                    let mut dictionary = plist::Dictionary::new();
                    dictionary.insert(plist_key.to_owned(), PlistValue::Boolean(flag));
                    let restart = if flag {
                        mode
                    } else {
                        match mode {
                            RestartMode::Failure => RestartMode::Success,
                            RestartMode::Success => RestartMode::Failure,
                        }
                    };
                    return Ok((Some(restart), Some(PlistValue::Dictionary(dictionary))));
                }
            }
            Err(refusal(format!(
                "Formula `{name}` has unsupported keep_alive service data."
            )))
        }
        _ => Err(refusal(format!(
            "Formula `{name}` has unsupported keep_alive service data."
        ))),
    }
}

fn parse_environment(
    env: &Env,
    name: &str,
    value: Option<&Value>,
) -> Result<BTreeMap<String, String>, OpError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        refusal(format!(
            "Formula `{name}` has unsupported service environment data."
        ))
    })?;
    object
        .iter()
        .map(|(key, value)| {
            let value = value.as_str().ok_or_else(|| {
                refusal(format!(
                    "Formula `{name}` has a non-string service environment value for `{key}`."
                ))
            })?;
            Ok((key.clone(), substitute(env, name, value)?))
        })
        .collect()
}

fn path_field(
    env: &Env,
    name: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, OpError> {
    string_field(name, object, key)?
        .map(|value| substitute(env, name, &value))
        .transpose()
}

fn string_field(
    name: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, OpError> {
    object
        .get(key)
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                refusal(format!(
                    "Formula `{name}` has a non-string service `{key}` value."
                ))
            })
        })
        .transpose()
}

fn bool_field(name: &str, object: &Map<String, Value>, key: &str) -> Result<Option<bool>, OpError> {
    object
        .get(key)
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                refusal(format!(
                    "Formula `{name}` has a non-boolean service `{key}` value."
                ))
            })
        })
        .transpose()
}

fn unsigned_field(
    name: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<u64>, OpError> {
    object
        .get(key)
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                refusal(format!(
                    "Formula `{name}` has a non-negative-integer service `{key}` value."
                ))
            })
        })
        .transpose()
}

fn signed_field(
    name: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<i64>, OpError> {
    object
        .get(key)
        .map(|value| {
            value.as_i64().ok_or_else(|| {
                refusal(format!(
                    "Formula `{name}` has a non-integer service `{key}` value."
                ))
            })
        })
        .transpose()
}

fn substitute(env: &Env, name: &str, value: &str) -> Result<String, OpError> {
    let value = value
        .replace("$HOMEBREW_PREFIX", env.prefix.as_str())
        .replace("$HOMEBREW_CELLAR", env.cellar.as_str())
        .replace("/$HOME", env.home.as_str());
    if value.contains("$HOMEBREW_") || contains_at_placeholder(&value) {
        return Err(refusal(format!(
            "Formula `{name}` uses unsupported service placeholders."
        )));
    }
    Ok(value)
}

fn contains_at_placeholder(value: &str) -> bool {
    value
        .find("@@")
        .is_some_and(|start| value[start + 2..].contains("@@"))
}

fn render_systemd_unit(name: &str, config: &ServiceConfig) -> String {
    let mut options = vec![
        format!(
            "Type={}",
            if config.launch_only_once {
                "oneshot"
            } else {
                "simple"
            }
        ),
        format!(
            "ExecStart={}",
            config
                .run
                .iter()
                .map(|argument| systemd_quote(argument))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    ];
    if let Some(mode) = config.restart_mode {
        options.push(format!(
            "Restart={}",
            match mode {
                RestartMode::Failure => "on-failure",
                RestartMode::Success => "on-success",
            }
        ));
    }
    if let Some(value) = config.restart_delay {
        options.push(format!("RestartSec={value}"));
    }
    if let Some(value) = config.stop_timeout {
        options.push(format!("TimeoutStopSec={value}"));
    }
    if let Some(value) = config.nice {
        options.push(format!("Nice={value}"));
    }
    if let Some(value) = &config.working_dir {
        options.push(format!("WorkingDirectory={value}"));
    }
    if let Some(value) = &config.root_dir {
        options.push(format!("RootDirectory={value}"));
    }
    if let Some(value) = &config.input_path {
        options.push(format!("StandardInput=file:{value}"));
    }
    if let Some(value) = &config.log_path {
        options.push(format!("StandardOutput=append:{value}"));
    }
    if let Some(value) = &config.error_log_path {
        options.push(format!("StandardError=append:{value}"));
    }
    for (key, value) in &config.environment {
        options.push(format!(
            "Environment=\"{}={}\"",
            systemd_escape(key),
            systemd_escape(value)
        ));
    }

    format!(
        "[Unit]\nDescription=Homebrew generated unit for {name}\n\n[Install]\nWantedBy=default.target\n\n[Service]\n{}\n",
        options.join("\n")
    )
}

fn render_systemd_timer(name: &str, config: &ServiceConfig) -> Result<String, OpError> {
    let mut options = vec![format!("Unit={}", service_unit(name))];
    match &config.schedule {
        Schedule::Immediate => {
            return Err(OpError::InvalidState {
                reason: format!("service {name} is not timed"),
            });
        }
        Schedule::Interval(interval) => options.push(format!("OnUnitActiveSec={interval}")),
        Schedule::Cron(cron) => {
            options.push("Persistent=true".to_owned());
            options.push(format!("OnCalendar={}", systemd_calendar(cron)));
        }
    }
    Ok(format!(
        "[Unit]\nDescription=Homebrew generated timer for {name}\n\n[Install]\nWantedBy=timers.target\n\n[Timer]\n{}\n",
        options.join("\n")
    ))
}

fn render_launchd_plist(_env: &Env, name: &str, config: &ServiceConfig) -> Result<String, OpError> {
    let mut values = BTreeMap::<String, PlistValue>::new();
    if !config.environment.is_empty() {
        let mut environment = plist::Dictionary::new();
        for (key, value) in &config.environment {
            environment.insert(key.clone(), PlistValue::String(value.clone()));
        }
        values.insert(
            "EnvironmentVariables".to_owned(),
            PlistValue::Dictionary(environment),
        );
    }
    if let Some(value) = &config.plist_keep_alive {
        values.insert("KeepAlive".to_owned(), value.clone());
    }
    values.insert("Label".to_owned(), PlistValue::String(plist_label(name)));
    values.insert(
        "LimitLoadToSessionType".to_owned(),
        PlistValue::Array(
            ["Aqua", "Background", "LoginWindow", "StandardIO", "System"]
                .into_iter()
                .map(|value| PlistValue::String(value.to_owned()))
                .collect(),
        ),
    );
    values.insert(
        "ProgramArguments".to_owned(),
        PlistValue::Array(config.run.iter().cloned().map(PlistValue::String).collect()),
    );
    values.insert("RunAtLoad".to_owned(), PlistValue::Boolean(true));
    insert_string(&mut values, "StandardOutPath", &config.log_path);
    insert_string(&mut values, "StandardErrorPath", &config.error_log_path);
    match &config.schedule {
        Schedule::Immediate => {}
        Schedule::Interval(interval) => {
            values.insert(
                "StartInterval".to_owned(),
                PlistValue::Integer((*interval).into()),
            );
        }
        Schedule::Cron(cron) => {
            values.insert("StartCalendarInterval".to_owned(), plist_calendar(cron));
        }
    }
    insert_string(&mut values, "WorkingDirectory", &config.working_dir);

    let mut output = Vec::new();
    plist::to_writer_xml(&mut output, &values).map_err(|source| OpError::InvalidState {
        reason: format!("failed to serialize service plist: {source}"),
    })?;
    String::from_utf8(output).map_err(|source| OpError::InvalidState {
        reason: format!("plist serializer emitted invalid UTF-8: {source}"),
    })
}

fn insert_string(values: &mut BTreeMap<String, PlistValue>, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        values.insert(key.to_owned(), PlistValue::String(value.clone()));
    }
}

fn plist_calendar(cron: &Cron) -> PlistValue {
    let fields = [
        ("Minute", &cron.minute),
        ("Hour", &cron.hour),
        ("Day", &cron.day),
        ("Month", &cron.month),
        ("Weekday", &cron.weekday),
    ];
    let active: Vec<(&str, &[u32])> = fields
        .iter()
        .filter_map(|(key, field)| match field {
            CronField::Any => None,
            CronField::Set(values) => Some((*key, values.as_slice())),
        })
        .collect();

    // Every field single-valued (or Any) renders as one dictionary, byte-identical
    // to the historic single-value launchd output.
    if active.iter().all(|(_, values)| values.len() == 1) {
        let entries: Vec<(&str, u32)> = active
            .iter()
            .filter_map(|(key, values)| values.first().map(|value| (*key, *value)))
            .collect();
        return PlistValue::Dictionary(calendar_dict(&entries));
    }

    // Cartesian product over non-Any fields in Minute..Weekday order, last field
    // varying fastest (row-major). Bounded to MAX_CRON_COMBINATIONS at parse time.
    let mut rows: Vec<Vec<(&str, u32)>> = vec![Vec::new()];
    for (key, values) in &active {
        let mut next = Vec::with_capacity(rows.len() * values.len());
        for row in &rows {
            for value in *values {
                let mut extended = row.clone();
                extended.push((*key, *value));
                next.push(extended);
            }
        }
        rows = next;
    }
    PlistValue::Array(
        rows.iter()
            .map(|entries| PlistValue::Dictionary(calendar_dict(entries)))
            .collect(),
    )
}

fn calendar_dict(entries: &[(&str, u32)]) -> plist::Dictionary {
    let mut dictionary = plist::Dictionary::new();
    for (key, value) in entries {
        dictionary.insert((*key).to_owned(), PlistValue::Integer((*value).into()));
    }
    dictionary
}

fn systemd_calendar(cron: &Cron) -> String {
    let minute = systemd_field(CronKind::Minute, &cron.minute);
    let hour = systemd_field(CronKind::Hour, &cron.hour);
    let day = systemd_field(CronKind::Day, &cron.day);
    let month = systemd_field(CronKind::Month, &cron.month);
    let weekday = match &cron.weekday {
        CronField::Any => String::new(),
        CronField::Set(values) => {
            const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
            let mut indices: Vec<usize> = values.iter().map(|value| *value as usize % 7).collect();
            indices.sort_unstable();
            indices.dedup();
            let names: Vec<&str> = indices.iter().map(|index| DAYS[*index]).collect();
            format!("{} ", names.join(","))
        }
    };
    format!("{weekday}*-{month}-{day} {hour}:{minute}:00")
}

fn systemd_field(kind: CronKind, field: &CronField) -> String {
    match field {
        CronField::Any => "*".to_owned(),
        CronField::Set(values) => values
            .iter()
            .map(|value| {
                if kind.systemd_padded() {
                    format!("{value:02}")
                } else {
                    value.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    }
}

fn systemd_quote(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '\u{7}' => output.push_str("\\a"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{b}' => output.push_str("\\v"),
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '%' => output.push_str("%%"),
            other => output.push(other),
        }
    }
    output.push('"');
    output
}
fn systemd_escape(value: &str) -> String {
    value
        .replace('%', "%%")
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn refusal(message: String) -> OpError {
    OpError::Refusal { message }
}

fn unsupported_platform() -> OpError {
    refusal("Services are not supported for the universal bottle platform.".to_owned())
}

#[doc(hidden)]
pub(crate) fn parse_for_test(env: &Env, name: &str, value: &Value) -> Result<(), OpError> {
    parse_service(env, name, value).map(|_| ())
}

#[doc(hidden)]
pub(crate) fn render_systemd_unit_for_test(
    env: &Env,
    name: &str,
    value: &Value,
) -> Result<String, OpError> {
    parse_service(env, name, value).map(|config| render_systemd_unit(name, &config))
}

#[doc(hidden)]
pub(crate) fn render_systemd_timer_for_test(
    env: &Env,
    name: &str,
    value: &Value,
) -> Result<String, OpError> {
    let config = parse_service(env, name, value)?;
    render_systemd_timer(name, &config)
}

#[doc(hidden)]
pub(crate) fn render_launchd_plist_for_test(
    env: &Env,
    name: &str,
    value: &Value,
) -> Result<String, OpError> {
    let config = parse_service(env, name, value)?;
    render_launchd_plist(env, name, &config)
}
