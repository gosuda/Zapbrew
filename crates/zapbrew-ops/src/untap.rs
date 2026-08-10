use std::fs;

use crate::state;
use crate::tap::{TapName, is_empty_real_directory, is_installed, measure, remove_tree};
use crate::transaction::acquire_exclusive_tap_locks;
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub force: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let taps = args
        .names
        .iter()
        .map(|name| TapName::parse(name))
        .collect::<Result<Vec<_>, _>>()?;

    // Acquire exclusive tap locks for all requested taps (sorted/deduped)
    // before scanning receipts or removing any tap directory. Held until
    // function exit so a concurrent install cannot commit a receipt from a
    // tap being removed. --force bypasses only the dependent refusal below,
    // not this lock.
    let _tap_locks = acquire_exclusive_tap_locks(&ctx.env, &taps)?;

    if !args.force {
        refuse_if_dependents(ctx, &taps)?;
    }

    for tap in taps {
        if !is_installed(&ctx.env, &tap)? {
            return Err(OpError::Refusal {
                message: format!("No available tap {}.", tap.name()),
            });
        }

        let path = tap.path(&ctx.env);
        let stats = measure(&path)?;
        ctx.reporter.ohai(&format!("Untapping {}", tap.name()));
        remove_tree(&path)?;

        let user_path = tap.user_path(&ctx.env);
        if is_empty_real_directory(&user_path)? {
            fs::remove_dir(&user_path).map_err(|source| {
                OpError::io("remove empty tap user directory", user_path.clone(), source)
            })?;
        }
        ctx.reporter.print(&format!("Untapped ({}).", stats.abv()));
    }
    Ok(())
}

/// Refuse when any installed keg receipt names one of `taps` as its source tap.
///
/// Both requested names and receipt values are normalized through
/// [`TapName::parse`]. Unparseable receipt values are reported and skipped
/// because they cannot be matched safely.
fn refuse_if_dependents(ctx: &Ctx, taps: &[TapName]) -> Result<(), OpError> {
    let state = state::scan(&ctx.env)?;
    for formula in state.iter() {
        for keg in formula.kegs() {
            let Some(raw_tap) = &keg.tab().source.tap else {
                continue;
            };
            let Ok(receipt_tap) = TapName::parse(raw_tap) else {
                ctx.reporter.opoo(&format!(
                    "Skipping {}: receipt names an unparseable tap `{raw_tap}`",
                    formula.name().name()
                ));
                continue;
            };
            if taps.iter().any(|tap| tap == &receipt_tap) {
                return Err(OpError::Refusal {
                    message: format!(
                        "Refusing to untap {}: installed formula {} depends on it.",
                        receipt_tap.name(),
                        formula.name().name()
                    ),
                });
            }
        }
    }
    Ok(())
}
