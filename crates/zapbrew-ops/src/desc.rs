use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    for requested in args.names {
        let formula = ctx
            .catalog
            .get(&requested)
            .ok_or_else(|| OpError::MissingFormula {
                name: requested.clone(),
            })?;
        if let Some(description) = &formula.desc {
            ctx.reporter
                .print(&format!("{}: {description}", formula.full_name));
        }
    }
    Ok(())
}
