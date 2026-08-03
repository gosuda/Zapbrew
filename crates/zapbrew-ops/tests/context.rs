use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput};

struct PanicRunner;

impl CommandRunner for PanicRunner {
    fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        panic!("context construction must not invoke host commands")
    }
}

#[derive(Default)]
struct RecordingReporter {
    messages: Mutex<Vec<String>>,
}

impl RecordingReporter {
    fn record(&self, kind: &str, message: &str) {
        match self.messages.lock() {
            Ok(mut messages) => messages.push(format!("{kind}:{message}")),
            Err(_) => panic!("recording reporter lock poisoned"),
        }
    }
}

impl Reporter for RecordingReporter {
    fn ohai(&self, message: &str) {
        self.record("ohai", message);
    }

    fn oh1(&self, message: &str) {
        self.record("oh1", message);
    }

    fn opoo(&self, message: &str) {
        self.record("opoo", message);
    }

    fn onoe(&self, message: &str) {
        self.record("onoe", message);
    }

    fn print(&self, message: &str) {
        self.record("print", message);
    }

    fn eprint(&self, message: &str) {
        self.record("eprint", message);
    }
}

fn test_env() -> Env {
    match Env::detect_from(
        &EnvDetectInput {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            home: Utf8PathBuf::from("/scratch/home"),
            xdg_cache_home: Some(Utf8PathBuf::from("/scratch/cache")),
            vars: HashMap::from([
                ("HOMEBREW_PREFIX".to_owned(), "/scratch/prefix".to_owned()),
                (
                    "HOMEBREW_REPOSITORY".to_owned(),
                    "/scratch/repository".to_owned(),
                ),
            ]),
            available_parallelism: 2,
        },
        &PanicRunner,
    ) {
        Ok(env) => env,
        Err(error) => panic!("expected scratch Env, got {error:?}"),
    }
}

#[test]
fn context_owns_all_boundaries_and_reporter_is_object_safe() {
    let env = test_env();
    let catalog = match Catalog::from_payload(b"[]", &env.bottle_tag) {
        Ok(catalog) => Arc::new(catalog),
        Err(error) => panic!("expected empty formula catalog, got {error:?}"),
    };
    let casks = match CaskCatalog::from_payload(b"[]", &env.bottle_tag) {
        Ok(casks) => Arc::new(casks),
        Err(error) => panic!("expected empty cask catalog, got {error:?}"),
    };
    let commands: Arc<dyn CommandRunner> = Arc::new(PanicRunner);
    let recording = Arc::new(RecordingReporter::default());
    let reporter: Arc<dyn Reporter> = recording.clone();

    let ctx = Ctx {
        env,
        http: reqwest::Client::new(),
        catalog: catalog.clone(),
        casks: casks.clone(),
        commands: commands.clone(),
        reporter: reporter.clone(),
    };

    assert_eq!(ctx.env.prefix, Utf8PathBuf::from("/scratch/prefix"));
    assert!(Arc::ptr_eq(&ctx.catalog, &catalog));
    assert!(Arc::ptr_eq(&ctx.casks, &casks));
    assert!(Arc::ptr_eq(&ctx.commands, &commands));
    assert!(Arc::ptr_eq(&ctx.reporter, &reporter));

    ctx.reporter.ohai("heading");
    ctx.reporter.oh1("step");
    ctx.reporter.opoo("warning");
    ctx.reporter.onoe("failure");
    ctx.reporter.print("stdout");
    ctx.reporter.eprint("stderr");
    let messages = match recording.messages.lock() {
        Ok(messages) => messages.clone(),
        Err(_) => panic!("recording reporter lock poisoned"),
    };
    assert_eq!(
        messages,
        [
            "ohai:heading",
            "oh1:step",
            "opoo:warning",
            "onoe:failure",
            "print:stdout",
            "eprint:stderr",
        ]
    );
}
