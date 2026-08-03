use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use zapbrew_api::{Catalog, Dependency, DependencyTag, Formula, UsesFromMacos};
use zapbrew_types::{BottleTag, MacOsVersion};

use crate::OpError;

/// Which dependency classes the expansion is preparing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyMode {
    All,
    Pour,
}

/// Host and filtering inputs for dependency expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyOptions {
    pub target: BottleTag,
    pub mode: DependencyMode,
}

/// Runtime necessity after every occurrence of a dependency is merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Necessity {
    Optional,
    Recommended,
    Required,
}

/// One dependency in stable post-order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedDependency {
    pub name: String,
    pub necessity: Necessity,
    pub build: bool,
    pub test: bool,
}

/// Direct or transitive inverse dependency lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsesMode {
    Direct,
    Recursive,
}

/// Expand all dependencies of `roots` in stable post-order.
pub fn expand<I, S>(
    catalog: &Catalog,
    roots: I,
    options: &DependencyOptions,
) -> Result<Vec<ExpandedDependency>, OpError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if matches!(options.target, BottleTag::All) {
        return Err(OpError::InvalidState {
            reason: "the universal bottle tag is not a host platform".to_owned(),
        });
    }

    let mut root_names = Vec::new();
    let mut root_set = HashSet::new();
    for root in roots {
        let requested = root.as_ref();
        let formula = catalog
            .get(requested)
            .ok_or_else(|| OpError::MissingFormula {
                name: requested.to_owned(),
            })?;
        if root_set.insert(formula.name.clone()) {
            root_names.push(formula.name.clone());
        }
    }

    let mut traversal = Traversal {
        catalog,
        options,
        roots: root_set,
        active: Vec::new(),
        complete: HashSet::new(),
        order: Vec::new(),
        merged: HashMap::new(),
    };
    for root in root_names {
        traversal.visit(&root)?;
    }

    let mut expanded = Vec::with_capacity(traversal.order.len());
    for name in traversal.order {
        let Some(merged) = traversal.merged.remove(&name) else {
            continue;
        };
        let dependency = ExpandedDependency {
            name,
            necessity: merged.necessity,
            build: merged.build,
            test: merged.test,
        };
        if matches!(options.mode, DependencyMode::Pour) && (dependency.build || dependency.test) {
            continue;
        }
        expanded.push(dependency);
    }
    Ok(expanded)
}

/// Find formulae that use any target, sorted and deduplicated.
pub fn uses<I, S>(catalog: &Catalog, targets: I, mode: UsesMode) -> Result<Vec<String>, OpError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut sought = HashSet::new();
    for target in targets {
        let requested = target.as_ref();
        let formula = catalog
            .get(requested)
            .ok_or_else(|| OpError::MissingFormula {
                name: requested.to_owned(),
            })?;
        sought.insert(formula.name.clone());
    }

    let original = sought.clone();
    let mut matches = HashSet::new();
    loop {
        let mut changed = false;
        for formula in catalog.iter() {
            if original.contains(&formula.name) || matches.contains(&formula.name) {
                continue;
            }
            if formula_uses_any(catalog, formula, &sought) {
                changed |= matches.insert(formula.name.clone());
            }
        }
        if !matches!(mode, UsesMode::Recursive) || !changed {
            break;
        }
        sought.extend(matches.iter().cloned());
    }

    let mut result: Vec<String> = matches.into_iter().collect();
    result.sort();
    Ok(result)
}

#[derive(Debug, Clone, Copy)]
struct MergedTags {
    necessity: Necessity,
    build: bool,
    test: bool,
}

impl MergedTags {
    fn from_tags(tags: &[DependencyTag]) -> Self {
        Self {
            necessity: necessity(tags),
            build: tags.contains(&DependencyTag::Build),
            test: tags.contains(&DependencyTag::Test),
        }
    }

    fn merge(&mut self, other: Self) {
        self.necessity = self.necessity.max(other.necessity);
        self.build &= other.build;
        self.test |= other.test;
    }
}

struct Traversal<'a> {
    catalog: &'a Catalog,
    options: &'a DependencyOptions,
    roots: HashSet<String>,
    active: Vec<String>,
    complete: HashSet<String>,
    order: Vec<String>,
    merged: HashMap<String, MergedTags>,
}

impl Traversal<'_> {
    fn visit(&mut self, name: &str) -> Result<(), OpError> {
        if let Some(cycle_start) = self.active.iter().position(|active| active == name) {
            let mut cycle = self.active[cycle_start..].to_vec();
            cycle.push(name.to_owned());
            return Err(OpError::DependencyCycle { cycle });
        }
        if self.complete.contains(name) {
            return Ok(());
        }

        let formula = self
            .catalog
            .get(name)
            .ok_or_else(|| OpError::MissingFormula {
                name: name.to_owned(),
            })?;
        self.active.push(formula.name.clone());

        for dependency in &formula.dependencies {
            self.visit_dependency(&dependency.name, &dependency.tags)?;
        }
        for dependency in &formula.uses_from_macos {
            if include_uses_from_macos(dependency, self.options.target)? {
                self.visit_dependency(&dependency.name, &dependency.tags)?;
            }
        }

        self.active.pop();
        self.complete.insert(formula.name.clone());
        if !self.roots.contains(&formula.name) {
            self.order.push(formula.name.clone());
        }
        Ok(())
    }

    fn visit_dependency(&mut self, requested: &str, tags: &[DependencyTag]) -> Result<(), OpError> {
        let formula = self
            .catalog
            .get(requested)
            .ok_or_else(|| OpError::MissingFormula {
                name: requested.to_owned(),
            })?;
        let name = formula.name.clone();
        let occurrence = MergedTags::from_tags(tags);
        self.merged
            .entry(name.clone())
            .and_modify(|merged| merged.merge(occurrence))
            .or_insert(occurrence);
        self.visit(&name)
    }
}

fn necessity(tags: &[DependencyTag]) -> Necessity {
    if tags.contains(&DependencyTag::Recommended) {
        Necessity::Recommended
    } else if tags.contains(&DependencyTag::Optional) {
        Necessity::Optional
    } else {
        Necessity::Required
    }
}

fn include_uses_from_macos(dependency: &UsesFromMacos, target: BottleTag) -> Result<bool, OpError> {
    match target {
        BottleTag::Linux { .. } => Ok(true),
        BottleTag::MacOs { version, .. } => {
            let Some(since) = dependency.since.as_deref() else {
                return Ok(false);
            };
            let bound = MacOsVersion::from_str(since).map_err(|_| OpError::InvalidState {
                reason: format!(
                    "uses_from_macos dependency {} has unknown since bound {since}",
                    dependency.name
                ),
            })?;
            Ok(version < bound)
        }
        BottleTag::All => Err(OpError::InvalidState {
            reason: "the universal bottle tag is not a host platform".to_owned(),
        }),
    }
}

fn formula_uses_any(catalog: &Catalog, formula: &Formula, sought: &HashSet<String>) -> bool {
    formula
        .dependencies
        .iter()
        .any(|dependency| dependency_is_sought(catalog, dependency, sought))
        || formula
            .uses_from_macos
            .iter()
            .any(|dependency| name_is_sought(catalog, &dependency.name, sought))
}

fn dependency_is_sought(
    catalog: &Catalog,
    dependency: &Dependency,
    sought: &HashSet<String>,
) -> bool {
    name_is_sought(catalog, &dependency.name, sought)
}

fn name_is_sought(catalog: &Catalog, name: &str, sought: &HashSet<String>) -> bool {
    catalog
        .get(name)
        .is_some_and(|formula| sought.contains(&formula.name))
}
