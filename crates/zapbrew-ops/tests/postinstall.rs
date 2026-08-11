use std::collections::HashMap;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::postinstall::{self, Args};
use zapbrew_ops::{Ctx, OpError, Reporter};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, Tab};
use zapbrew_types::{FormulaName, PkgVersion};

struct PanicRunner;
impl CommandRunner for PanicRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        panic!("host command must not run: {:?}", spec.program())
    }
}

struct CleanupSabotageRunner {
    rack: Utf8PathBuf,
    outside: Utf8PathBuf,
}

impl CommandRunner for CleanupSabotageRunner {
    fn run(&self, _: &CommandSpec) -> Result<CommandOutput, io::Error> {
        let root = std::fs::read_dir(self.rack.as_std_path())?
            .filter_map(Result::ok)
            .find_map(|entry| {
                let name = entry.file_name();
                name.to_string_lossy()
                    .starts_with(".zapbrew-step-journal")
                    .then(|| Utf8PathBuf::from_path_buf(entry.path()).expect("utf8 journal root"))
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "journal root"))?;
        std::fs::remove_dir_all(&root)?;
        symlink(&self.outside, &root)?;
        Ok(CommandOutput::new(
            ExitStatus::from_raw(0),
            Vec::new(),
            Vec::new(),
        ))
    }
}

#[derive(Default)]
struct RecordingReporter(Mutex<Vec<String>>);
impl RecordingReporter {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().expect("reporter lock"))
    }
}

impl Reporter for RecordingReporter {
    fn ohai(&self, message: &str) {
        self.0
            .lock()
            .expect("reporter lock")
            .push(format!("ohai:{message}"));
    }
    fn oh1(&self, message: &str) {
        self.0
            .lock()
            .expect("reporter lock")
            .push(format!("oh1:{message}"));
    }
    fn opoo(&self, message: &str) {
        self.0
            .lock()
            .expect("reporter lock")
            .push(format!("opoo:{message}"));
    }
    fn onoe(&self, message: &str) {
        self.0
            .lock()
            .expect("reporter lock")
            .push(format!("onoe:{message}"));
    }
    fn print(&self, message: &str) {
        self.0
            .lock()
            .expect("reporter lock")
            .push(format!("print:{message}"));
    }
    fn eprint(&self, message: &str) {
        self.0
            .lock()
            .expect("reporter lock")
            .push(format!("eprint:{message}"));
    }
    fn is_quiet(&self) -> bool {
        false
    }
    fn is_verbose(&self) -> bool {
        false
    }
}

fn scratch_env(temp: &TempDir) -> Env {
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp");
    Env::detect_from(
        &EnvDetectInput {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            home: root.join("home"),
            xdg_cache_home: None,
            vars: HashMap::from([
                (
                    "HOMEBREW_PREFIX".to_owned(),
                    root.join("prefix").to_string(),
                ),
                ("HOMEBREW_CACHE".to_owned(), root.join("cache").to_string()),
                ("HOMEBREW_TEMP".to_owned(), root.join("temp").to_string()),
            ]),
            available_parallelism: 2,
        },
        &PanicRunner,
    )
    .expect("scratch env")
}

fn catalog(env: &Env) -> Catalog {
    Catalog::from_payload(
        br#"[{
            "name": "demo",
            "full_name": "demo",
            "versions": {"stable": "1.0", "bottle": false},
            "post_install_defined": true,
            "post_install_steps": [{"type": "touch", "path": {"base": "keg", "path": "share/postinstall-done"}}]
        }]"#,
        &env.bottle_tag,
    )
    .expect("catalog")
}

fn make_keg(env: &Env, name: &str, version: &str) -> Keg {
    let keg = Keg::new(
        &env.cellar,
        FormulaName::from_str(name).expect("formula name"),
        PkgVersion::from_str(version).expect("pkg version"),
    )
    .expect("keg");
    std::fs::create_dir_all(keg.path()).expect("keg dir");
    Tab::default().write(keg.receipt_path()).expect("tab");
    keg
}

#[tokio::test]
async fn postinstall_runs_steps_for_installed_formula() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let keg = make_keg(&env, "demo", "1.0");

    let catalog = Arc::new(catalog(&env));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let recording = Arc::new(RecordingReporter::default());
    let reporter: Arc<dyn Reporter> = recording.clone();
    let ctx = Ctx {
        env: env.clone(),
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter,
    };

    postinstall::run(
        &ctx,
        Args {
            names: vec!["demo".to_owned()],
        },
    )
    .await
    .expect("postinstall");

    let marker = keg.path().join("share/postinstall-done");
    assert!(
        marker.exists(),
        "touch step should create marker inside keg"
    );

    let output = recording.take();
    assert!(
        output
            .iter()
            .any(|line| line.starts_with("ohai:Postinstalling "))
    );
}

#[tokio::test]
async fn postinstall_warns_when_no_post_install_defined() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let _keg = make_keg(&env, "plain", "1.0");

    let payload = br#"[{
        "name": "plain",
        "full_name": "plain",
        "versions": {"stable": "1.0", "bottle": false},
        "post_install_defined": false
    }]"#;
    let catalog = Arc::new(Catalog::from_payload(payload, &env.bottle_tag).expect("catalog"));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let recording = Arc::new(RecordingReporter::default());
    let reporter: Arc<dyn Reporter> = recording.clone();
    let ctx = Ctx {
        env,
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter,
    };

    postinstall::run(
        &ctx,
        Args {
            names: vec!["plain".to_owned()],
        },
    )
    .await
    .expect("postinstall");

    let output = recording.take();
    assert!(
        output
            .iter()
            .any(|line| line.contains("no post-install method"))
    );
}

#[tokio::test]
async fn postinstall_invalid_name_is_invalid_state() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let catalog = Arc::new(catalog(&env));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let ctx = Ctx {
        env,
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter: Arc::new(RecordingReporter::default()),
    };
    let err = postinstall::run(
        &ctx,
        Args {
            names: vec!["!!!".to_owned()],
        },
    )
    .await
    .expect_err("invalid name");
    assert!(matches!(err, zapbrew_ops::OpError::InvalidState { .. }));
}

#[tokio::test]
async fn postinstall_no_such_keg_is_refusal() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let catalog = Arc::new(catalog(&env));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let ctx = Ctx {
        env,
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter: Arc::new(RecordingReporter::default()),
    };
    let err = postinstall::run(
        &ctx,
        Args {
            names: vec!["demo".to_owned()],
        },
    )
    .await
    .expect_err("no keg");
    assert!(matches!(err, zapbrew_ops::OpError::Refusal { .. }));
    assert!(err.to_string().contains("No such keg"));
}

#[tokio::test]
async fn postinstall_missing_formula_is_missing_formula() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let _keg = make_keg(&env, "demo", "1.0");
    let catalog = Arc::new(Catalog::from_payload(b"[]", &env.bottle_tag).expect("catalog"));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let ctx = Ctx {
        env,
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter: Arc::new(RecordingReporter::default()),
    };
    let err = postinstall::run(
        &ctx,
        Args {
            names: vec!["demo".to_owned()],
        },
    )
    .await
    .expect_err("missing formula");
    assert!(matches!(err, zapbrew_ops::OpError::MissingFormula { .. }));
}

#[tokio::test]
async fn postinstall_prefers_linked_over_optlinked_and_latest() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let keg1 = make_keg(&env, "demo", "1.0");
    let keg2 = make_keg(&env, "demo", "2.0");
    // linked -> 1.0, opt -> 2.0
    std::fs::create_dir_all(&env.linked).expect("linked");
    std::fs::create_dir_all(env.prefix.join("opt")).expect("opt");
    std::os::unix::fs::symlink(
        keg1.path().as_std_path(),
        env.linked.join("demo").as_std_path(),
    )
    .expect("link");
    std::os::unix::fs::symlink(
        keg2.path().as_std_path(),
        env.prefix.join("opt/demo").as_std_path(),
    )
    .expect("opt link");
    let catalog = Arc::new(catalog(&env));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let recording = Arc::new(RecordingReporter::default());
    let ctx = Ctx {
        env: env.clone(),
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter: recording.clone(),
    };
    postinstall::run(
        &ctx,
        Args {
            names: vec!["demo".to_owned()],
        },
    )
    .await
    .expect("postinstall");
    assert!(keg1.path().join("share/postinstall-done").exists());
    assert!(!keg2.path().join("share/postinstall-done").exists());
}

#[tokio::test]
async fn postinstall_prefers_optlinked_when_no_linked() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let keg1 = make_keg(&env, "demo", "1.0");
    let keg2 = make_keg(&env, "demo", "2.0");
    std::fs::create_dir_all(env.prefix.join("opt")).expect("opt");
    std::os::unix::fs::symlink(
        keg2.path().as_std_path(),
        env.prefix.join("opt/demo").as_std_path(),
    )
    .expect("opt link");
    let catalog = Arc::new(catalog(&env));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let ctx = Ctx {
        env: env.clone(),
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter: Arc::new(RecordingReporter::default()),
    };
    postinstall::run(
        &ctx,
        Args {
            names: vec!["demo".to_owned()],
        },
    )
    .await
    .expect("postinstall");
    assert!(keg2.path().join("share/postinstall-done").exists());
    assert!(!keg1.path().join("share/postinstall-done").exists());
}

#[tokio::test]
async fn postinstall_cleans_up_journal_after_successful_overwrite() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let keg = make_keg(&env, "demo", "1.0");
    let existing = keg.path().join("share/existing");
    std::fs::create_dir_all(existing.parent().expect("share parent")).expect("share dir");
    std::fs::write(&existing, b"original").expect("original");
    let payload = br#"[{
        "name": "demo",
        "full_name": "demo",
        "versions": {"stable": "1.0", "bottle": false},
        "post_install_defined": true,
        "post_install_steps": [{
            "type": "write",
            "path": {"base": "keg", "path": "share/existing"},
            "content": "new",
            "overwrite": true
        }]
    }]"#;
    let catalog = Arc::new(Catalog::from_payload(payload, &env.bottle_tag).expect("catalog"));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let ctx = Ctx {
        env: env.clone(),
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter: Arc::new(RecordingReporter::default()),
    };
    postinstall::run(
        &ctx,
        Args {
            names: vec!["demo".to_owned()],
        },
    )
    .await
    .expect("postinstall");
    assert_eq!(std::fs::read_to_string(&existing).expect("read"), "new");
    let rack = env.cellar.join("demo");
    let entries = std::fs::read_dir(rack.as_std_path()).expect("read rack");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(
            !name.starts_with(".zapbrew-step-journal"),
            "journal should be cleaned up, found {name}"
        );
    }
}

#[tokio::test]
async fn postinstall_rollback_restores_original_on_failure() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let keg = make_keg(&env, "demo", "1.0");
    let existing = keg.path().join("share/existing");
    std::fs::create_dir_all(existing.parent().expect("share parent")).expect("share");
    std::fs::write(&existing, b"original").expect("original");
    let payload = br#"[{
        "name": "demo",
        "full_name": "demo",
        "versions": {"stable": "1.0", "bottle": false},
        "post_install_defined": true,
        "post_install_steps": [
            {
                "type": "write",
                "path": {"base": "keg", "path": "share/existing"},
                "content": "new",
                "overwrite": true
            },
            {
                "type": "mkdir",
                "path": {"base": "keg", "path": "share/missing_parent/child"}
            }
        ]
    }]"#;
    let catalog = Arc::new(Catalog::from_payload(payload, &env.bottle_tag).expect("catalog"));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let ctx = Ctx {
        env: env.clone(),
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter: Arc::new(RecordingReporter::default()),
    };
    let err = postinstall::run(
        &ctx,
        Args {
            names: vec!["demo".to_owned()],
        },
    )
    .await
    .expect_err("should fail");
    // original file must be restored
    assert_eq!(
        std::fs::read_to_string(&existing).expect("read"),
        "original"
    );
    // journal must be cleaned up (rollback removes it)
    let rack = env.cellar.join("demo");
    let entries = std::fs::read_dir(rack.as_std_path()).expect("read rack");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(
            !name.starts_with(".zapbrew-step-journal"),
            "journal should be removed after rollback, found {name}"
        );
    }
    // error should be either the mkdir failure or rollback incomplete
    let msg = err.to_string();
    assert!(
        msg.contains("create install-step directory") || msg.contains("rollback"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn postinstall_failure_does_not_roll_back_prior_formula() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let first = make_keg(&env, "first", "1.0");
    let second = make_keg(&env, "second", "1.0");
    let first_target = first.path().join("share/existing");
    let second_target = second.path().join("share/existing");
    for target in [&first_target, &second_target] {
        std::fs::create_dir_all(target.parent().expect("share parent")).expect("share");
        std::fs::write(target, b"original").expect("original");
    }
    let payload = br#"[
        {
            "name": "first",
            "full_name": "first",
            "versions": {"stable": "1.0", "bottle": false},
            "post_install_defined": true,
            "post_install_steps": [{
                "type": "write",
                "path": {"base": "keg", "path": "share/existing"},
                "content": "first-new",
                "overwrite": true
            }]
        },
        {
            "name": "second",
            "full_name": "second",
            "versions": {"stable": "1.0", "bottle": false},
            "post_install_defined": true,
            "post_install_steps": [
                {
                    "type": "write",
                    "path": {"base": "keg", "path": "share/existing"},
                    "content": "second-new",
                    "overwrite": true
                },
                {
                    "type": "mkdir",
                    "path": {"base": "keg", "path": "share/missing_parent/child"}
                }
            ]
        }
    ]"#;
    let catalog = Arc::new(Catalog::from_payload(payload, &env.bottle_tag).expect("catalog"));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let ctx = Ctx {
        env: env.clone(),
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter: Arc::new(RecordingReporter::default()),
    };

    let error = postinstall::run(
        &ctx,
        Args {
            names: vec!["first".to_owned(), "second".to_owned()],
        },
    )
    .await
    .expect_err("second formula must fail");
    let message = error.to_string();
    assert!(
        message.contains("create install-step directory"),
        "{message}"
    );
    assert!(
        message.contains("second/1.0/share/missing_parent/child"),
        "{message}"
    );

    assert_eq!(
        std::fs::read_to_string(&first_target).expect("read first"),
        "first-new"
    );
    assert_eq!(
        std::fs::read_to_string(&second_target).expect("read second"),
        "original"
    );
    for formula in ["first", "second"] {
        let entries =
            std::fs::read_dir(env.cellar.join(formula).as_std_path()).expect("read formula rack");
        assert!(
            entries.flatten().all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".zapbrew-step-journal")),
            "{formula} journal must not survive"
        );
    }
}

#[tokio::test]
async fn postinstall_cleanup_failure_reports_selected_keg_and_journal_root() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let keg = make_keg(&env, "demo", "1.0");
    let existing = keg.path().join("share/existing");
    std::fs::create_dir_all(existing.parent().expect("share parent")).expect("share");
    std::fs::write(&existing, b"original").expect("original");
    let command = keg.path().join("bin/sabotage");
    std::fs::create_dir_all(command.parent().expect("bin parent")).expect("bin");
    std::fs::write(&command, b"sabotage").expect("command");
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755))
        .expect("command mode");
    let outside = env.prefix.join("cleanup-sabotage");
    std::fs::create_dir_all(&outside).expect("outside");
    let payload = br#"[{
        "name": "demo",
        "full_name": "demo",
        "versions": {"stable": "1.0", "bottle": false},
        "post_install_defined": true,
        "post_install_steps": [
            {
                "type": "write",
                "path": {"base": "keg", "path": "share/existing"},
                "content": "new",
                "overwrite": true
            },
            {
                "type": "run",
                "command": {"base": "keg", "path": "bin/sabotage"}
            }
        ]
    }]"#;
    let catalog = Arc::new(Catalog::from_payload(payload, &env.bottle_tag).expect("catalog"));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let ctx = Ctx {
        env: env.clone(),
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(CleanupSabotageRunner {
            rack: env.cellar.join("demo"),
            outside: outside.clone(),
        }),
        reporter: Arc::new(RecordingReporter::default()),
    };

    let error = postinstall::run(
        &ctx,
        Args {
            names: vec!["demo".to_owned()],
        },
    )
    .await
    .expect_err("journal cleanup must fail");
    let (error_keg, leftovers) = match error {
        OpError::CleanupIncomplete { keg, leftovers } => (keg, leftovers),
        other => panic!("unexpected error: {other:?}"),
    };

    assert_eq!(error_keg, keg.path());
    assert_eq!(leftovers.len(), 1);
    assert!(leftovers[0].is_symlink());
    assert!(
        leftovers[0]
            .file_name()
            .is_some_and(|name| name.starts_with(".zapbrew-step-journal"))
    );
    assert_eq!(
        Utf8PathBuf::from_path_buf(std::fs::read_link(&leftovers[0]).expect("journal symlink"))
            .expect("utf8 journal target"),
        outside
    );
    assert!(outside.is_dir());
    assert_eq!(
        std::fs::read_to_string(existing).expect("replacement"),
        "new"
    );
}
