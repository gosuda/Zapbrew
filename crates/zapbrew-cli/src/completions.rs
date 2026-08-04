//! Shell completion generation for the `completions` subcommand.
//!
//! `main` intercepts `completions <shell>` before any environment detection,
//! runtime, or network work and calls [`generate`], which emits a static script
//! built from the clap surface. The script is a pure function of [`Cli`] and the
//! requested shell, so it always names the current command set.

use std::io::Write;

use clap::CommandFactory;
use clap_complete::Shell;

use crate::cli::{Cli, CompletionShell};

/// The binary name every generated script drives.
const BIN_NAME: &str = "zapbrew";

/// Write the completion script for `shell` to `out`.
///
/// The generator only produces stdout content; it never emits ANSI styling or
/// diagnostics, so `out` receives a ready-to-source script.
pub fn generate(shell: CompletionShell, out: &mut dyn Write) {
    let generator = match shell {
        CompletionShell::Bash => Shell::Bash,
        CompletionShell::Zsh => Shell::Zsh,
        CompletionShell::Fish => Shell::Fish,
    };
    let mut command = Cli::command();
    clap_complete::generate(generator, &mut command, BIN_NAME, out);
}

#[cfg(test)]
mod tests {
    use super::{BIN_NAME, generate};
    use crate::cli::CompletionShell;

    /// Generate a script into a UTF-8 string for structural inspection.
    fn script(shell: CompletionShell) -> String {
        let mut buffer = Vec::new();
        generate(shell, &mut buffer);
        String::from_utf8(buffer).expect("clap_complete emits UTF-8")
    }

    /// A compact, generator-version-stable structural fingerprint per shell:
    /// the function/registration marker plus proof the current command set and
    /// binary name reached the script, and proof no ANSI escape leaked in.
    fn markers(shell: CompletionShell, registration: &str) -> String {
        let text = script(shell);
        let mut lines = vec![
            format!("registration:{}", text.contains(registration)),
            format!("bin_name:{}", text.contains(BIN_NAME)),
            format!("no_ansi:{}", !text.contains('\u{1b}')),
        ];
        // Sample of representative verbs from every corner of the surface. A
        // dropped or renamed command flips one of these without pinning the
        // whole clap_complete-versioned script.
        for verb in ["install", "uninstall", "services", "shim", "completions"] {
            lines.push(format!("cmd:{verb}:{}", text.contains(verb)));
        }
        lines.join("\n")
    }

    #[test]
    fn bash_script_has_structural_markers() {
        insta::assert_snapshot!(markers(CompletionShell::Bash, "_zapbrew"), @r"
        registration:true
        bin_name:true
        no_ansi:true
        cmd:install:true
        cmd:uninstall:true
        cmd:services:true
        cmd:shim:true
        cmd:completions:true
        ");
    }

    #[test]
    fn zsh_script_has_structural_markers() {
        insta::assert_snapshot!(markers(CompletionShell::Zsh, "#compdef zapbrew"), @r"
        registration:true
        bin_name:true
        no_ansi:true
        cmd:install:true
        cmd:uninstall:true
        cmd:services:true
        cmd:shim:true
        cmd:completions:true
        ");
    }

    #[test]
    fn fish_script_has_structural_markers() {
        insta::assert_snapshot!(markers(CompletionShell::Fish, "complete -c zapbrew"), @r"
        registration:true
        bin_name:true
        no_ansi:true
        cmd:install:true
        cmd:uninstall:true
        cmd:services:true
        cmd:shim:true
        cmd:completions:true
        ");
    }
}
