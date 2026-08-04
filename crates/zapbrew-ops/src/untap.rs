use std::fs;

use crate::tap::{TapName, is_empty_real_directory, is_installed, measure, remove_tree};
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
    let _ = args.force;

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
