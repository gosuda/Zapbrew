use std::collections::HashMap;
use std::io;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::postinstall::{self, Args};
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, Tab};
use zapbrew_types::{FormulaName, PkgVersion};

struct PanicRunner;
impl CommandRunner for PanicRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        panic!("host command must not run: {:?}", spec.program())
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
