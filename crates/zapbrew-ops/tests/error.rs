use std::io;

use camino::Utf8PathBuf;
use zapbrew_ops::OpError;
use zapbrew_prefix::PrefixError;

#[test]
fn lower_crate_errors_are_transparent_and_cli_prefix_free() {
    let error = OpError::from(PrefixError::InvalidEnvironment {
        name: "HOMEBREW_TEMP",
        value: "".to_owned(),
    });
    let display = error.to_string();
    assert_eq!(display, "environment variable HOMEBREW_TEMP is invalid: ");
    assert!(!display.starts_with("Error: "));
}

#[test]
fn operation_variants_have_typed_payloads_and_no_cli_prefix() {
    let errors = [
        OpError::MissingFormula {
            name: "gone".to_owned(),
        },
        OpError::InvalidState {
            reason: "receipt points outside its keg".to_owned(),
        },
        OpError::DependencyCycle {
            cycle: vec!["a".to_owned(), "b".to_owned(), "a".to_owned()],
        },
        OpError::Refusal {
            message: "source builds require Ruby".to_owned(),
        },
    ];
    let displays: Vec<String> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        displays,
        [
            "No available formula with the name \"gone\".",
            "invalid operation state: receipt points outside its keg",
            "dependency cycle detected: a -> b -> a",
            "source builds require Ruby",
        ]
    );
    assert!(
        displays
            .iter()
            .all(|display| !display.starts_with("Error: "))
    );
}

#[test]
fn io_and_command_failures_keep_machine_readable_context() {
    let io_error = OpError::Io {
        operation: "read",
        path: Utf8PathBuf::from("/prefix/Cellar/app"),
        source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
    };
    match &io_error {
        OpError::Io {
            operation,
            path,
            source,
        } => {
            assert_eq!(*operation, "read");
            assert_eq!(path, &Utf8PathBuf::from("/prefix/Cellar/app"));
            assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
        }
        other => panic!("expected IO error, got {other:?}"),
    }
    assert_eq!(
        io_error.to_string(),
        "failed to read /prefix/Cellar/app: denied"
    );

    let command = OpError::CommandFailed {
        program: "systemctl".to_owned(),
        status: "exit status: 5".to_owned(),
        stderr: "unit not found".to_owned(),
    };
    assert_eq!(
        command.to_string(),
        "command `systemctl` failed with status exit status: 5: unit not found"
    );
}
