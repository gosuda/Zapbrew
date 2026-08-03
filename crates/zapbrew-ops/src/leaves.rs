use crate::state::{InstalledFormula, InstalledKeg, scan};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    OnRequest,
    AsDependency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Args {
    pub filter: Filter,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let state = scan(&ctx.env)?;
    for installed in state.iter() {
        let name = installed.name().name();
        if !state.dependents_of(name).is_empty() {
            continue;
        }
        let installed_on_request = intent_keg(installed)
            .map(|keg| keg.tab().installed_on_request)
            .unwrap_or_default();
        let selected = match args.filter {
            Filter::All => true,
            Filter::OnRequest => installed_on_request,
            Filter::AsDependency => !installed_on_request,
        };
        if selected {
            ctx.reporter.print(name);
        }
    }
    Ok(())
}

fn intent_keg(installed: &InstalledFormula) -> Option<&InstalledKeg> {
    installed
        .optlinked()
        .or_else(|| installed.linked())
        .or_else(|| match installed.kegs() {
            [keg] => Some(keg),
            _ => None,
        })
        .or_else(|| installed.latest())
}
