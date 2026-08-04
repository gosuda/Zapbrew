mod support;

use support::Fixture;
use zapbrew_ops::shellenv::{self, Args};
use zapbrew_ops::shellenv_test_support;
use zapbrew_prefix::Shell;

#[tokio::test]
async fn maps_every_supported_shell_and_alias_to_the_existing_template() {
    let fixture = Fixture::new();
    let (ctx, reporter) = fixture.context(Vec::new());
    let cases = [
        ("fish", Shell::Fish),
        ("csh", Shell::Csh),
        ("tcsh", Shell::Csh),
        ("pwsh", Shell::Pwsh),
        ("pwsh-preview", Shell::Pwsh),
        ("zsh", Shell::Zsh),
        ("bash", Shell::Bash),
        ("sh", Shell::Bash),
    ];

    for (name, shell) in cases {
        shellenv::run(
            &ctx,
            Args {
                shell: Some(name.to_owned()),
            },
        )
        .await
        .expect("shellenv");
        assert_eq!(
            reporter.take(),
            [format!("print:{}", fixture.env.shellenv(shell))],
            "mapping for {name}"
        );
    }
}

#[tokio::test]
async fn explicit_shell_wins_and_detected_login_shell_uses_its_basename() {
    let fixture = Fixture::new();
    let (ctx, reporter) = fixture.context(Vec::new());

    shellenv_test_support::run_with_detected(
        &ctx,
        Args {
            shell: Some("fish".to_owned()),
        },
        Some("/bin/zsh"),
    )
    .await
    .expect("explicit shellenv");
    assert_eq!(
        reporter.take(),
        [format!("print:{}", fixture.env.shellenv(Shell::Fish))]
    );

    shellenv_test_support::run_with_detected(&ctx, Args::default(), Some("/bin/-zsh"))
        .await
        .expect("detected shellenv");
    assert_eq!(
        reporter.take(),
        [format!("print:{}", fixture.env.shellenv(Shell::Zsh))]
    );
}

#[tokio::test]
async fn unknown_and_absent_values_fall_back_to_bash() {
    let fixture = Fixture::new();
    let (ctx, reporter) = fixture.context(Vec::new());

    for detected in [Some("/opt/bin/elvish"), None] {
        shellenv_test_support::run_with_detected(&ctx, Args::default(), detected)
            .await
            .expect("fallback shellenv");
        assert_eq!(
            reporter.take(),
            [format!("print:{}", fixture.env.shellenv(Shell::Bash))]
        );
    }
}
