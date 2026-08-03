use std::collections::BTreeSet;

use crate::dependency::{DependencyMode, DependencyOptions, EdgeFilter, expand, tree};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub tree: bool,
    pub union: bool,
    pub include_build: bool,
    pub include_test: bool,
    pub include_optional: bool,
    pub skip_recommended: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    if args.names.is_empty() {
        return Ok(());
    }

    let options = DependencyOptions {
        target: ctx.env.bottle_tag,
        mode: DependencyMode::All,
        filter: EdgeFilter::query(
            args.include_build,
            args.include_test,
            args.include_optional,
            args.skip_recommended,
        ),
    };

    if args.tree {
        if args.names.len() != 1 {
            return Err(OpError::InvalidState {
                reason: "dependency tree requires exactly one formula".to_owned(),
            });
        }
        let rendered = tree(ctx.catalog.as_ref(), &args.names[0], &options)?;
        ctx.reporter.print(&rendered.lines.join("\n"));
        if let Some(cycle) = rendered.cycle {
            return Err(OpError::DependencyCycle { cycle });
        }
        return Ok(());
    }

    let mut closures = args
        .names
        .iter()
        .map(|name| {
            expand(ctx.catalog.as_ref(), [name], &options).map(|dependencies| {
                dependencies
                    .into_iter()
                    .map(|dependency| dependency.name)
                    .collect::<BTreeSet<_>>()
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut selected = closures.pop().unwrap_or_default();
    if args.union {
        for closure in closures {
            selected.extend(closure);
        }
    } else {
        for closure in closures {
            selected = selected.intersection(&closure).cloned().collect();
        }
    }

    if !selected.is_empty() {
        ctx.reporter
            .print(&selected.into_iter().collect::<Vec<_>>().join("\n"));
    }
    Ok(())
}
