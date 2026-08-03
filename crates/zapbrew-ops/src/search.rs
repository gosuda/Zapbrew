use regex::Regex;

use crate::render::columns;
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub query: String,
    pub desc: bool,
    pub formula_only: bool,
    pub cask_only: bool,
    /// Output width supplied by the caller. Zero forces one item per line.
    pub width: usize,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let query = Query::parse(&args.query)?;
    let mut formulae = Vec::new();
    let mut casks = Vec::new();

    if !args.cask_only {
        for formula in ctx.catalog.iter() {
            let name_match = query.matches(&formula.name)
                || formula.aliases.iter().any(|alias| query.matches(alias));
            let desc_match = args.desc
                && formula
                    .desc
                    .as_deref()
                    .is_some_and(|description| query.matches(description));
            if name_match || desc_match {
                formulae.push(formula.name.clone());
            }
        }
        formulae.sort_unstable();
    }

    if !args.formula_only {
        for cask in ctx.casks.iter() {
            let name_match = query.matches(&cask.token);
            let desc_match = args.desc
                && cask
                    .desc
                    .as_deref()
                    .is_some_and(|description| query.matches(description));
            if name_match || desc_match {
                casks.push(cask.token.clone());
            }
        }
        casks.sort_unstable();
    }

    if formulae.is_empty() && casks.is_empty() {
        return Err(OpError::Refusal {
            message: format!("No formulae or casks found for {:?}.", args.query),
        });
    }

    if !formulae.is_empty() {
        ctx.reporter.ohai("Formulae");
        ctx.reporter.print(&columns(&formulae, args.width));
    }
    if !formulae.is_empty() && !casks.is_empty() {
        ctx.reporter.print("");
    }
    if !casks.is_empty() {
        ctx.reporter.ohai("Casks");
        ctx.reporter.print(&columns(&casks, args.width));
    }
    Ok(())
}

enum Query {
    Regex(Regex),
    Simplified(String),
}

impl Query {
    fn parse(query: &str) -> Result<Self, OpError> {
        let Some(pattern) = query
            .strip_prefix('/')
            .and_then(|body| body.strip_suffix('/'))
        else {
            return Ok(Self::Simplified(simplify(query)));
        };
        Regex::new(pattern)
            .map(Self::Regex)
            .map_err(|_| OpError::Refusal {
                message: format!("{query} is not a valid regex."),
            })
    }

    fn matches(&self, candidate: &str) -> bool {
        match self {
            Self::Regex(regex) => regex.is_match(candidate),
            Self::Simplified(query) => simplify(candidate).contains(query),
        }
    }
}

fn simplify(value: &str) -> String {
    value
        .chars()
        .filter_map(|character| {
            let character = character.to_ascii_lowercase();
            (character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '@'
                || character == '+')
                .then_some(character)
        })
        .collect()
}
