use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use zapbrew_api::{Cask, CaskCatalog, Catalog, DependencyTag, Formula, UsesFromMacos};
use zapbrew_types::{BottleTag, MacOsVersion};

use crate::OpError;

/// Which dependency classes the expansion is preparing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyMode {
    All,
    Pour,
}

/// Dependency edge classes selected for a graph query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeFilter {
    pub include_build: bool,
    pub include_test: bool,
    pub include_optional: bool,
    pub skip_recommended: bool,
    root_only_test: bool,
}

impl EdgeFilter {
    /// Preserve the complete graph for install/fetch expansion before the pour filter is applied.
    pub const ALL: Self = Self {
        include_build: true,
        include_test: true,
        include_optional: true,
        skip_recommended: false,
        root_only_test: false,
    };

    /// Homebrew query defaults plus the caller-selected additions and ignore.
    #[must_use]
    pub const fn query(
        include_build: bool,
        include_test: bool,
        include_optional: bool,
        skip_recommended: bool,
    ) -> Self {
        Self {
            include_build,
            include_test,
            include_optional,
            skip_recommended,
            root_only_test: true,
        }
    }

    fn includes(self, tags: &[DependencyTag], root_edge: bool) -> bool {
        let recommended = tags.contains(&DependencyTag::Recommended);
        if self.skip_recommended && recommended {
            return false;
        }

        let build = tags.contains(&DependencyTag::Build);
        let test = tags.contains(&DependencyTag::Test);
        let optional = tags.contains(&DependencyTag::Optional);
        let required = !recommended && !build && !test && !optional;

        required
            || recommended
            || (build && self.include_build)
            || (test && self.include_test && (!self.root_only_test || root_edge))
            || (optional && self.include_optional)
    }
}

impl Default for EdgeFilter {
    fn default() -> Self {
        Self::query(false, false, false, false)
    }
}

/// Host and filtering inputs for dependency expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyOptions {
    pub target: BottleTag,
    pub mode: DependencyMode,
    pub filter: EdgeFilter,
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

/// Host and edge filters for inverse dependency lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsesOptions {
    pub host: BottleTag,
    pub recursive: bool,
    pub filter: EdgeFilter,
}

/// A rendered dependency tree and the first cycle encountered while rendering it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DependencyTree {
    pub(crate) lines: Vec<String>,
    pub(crate) cycle: Option<Vec<String>>,
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
    validate_host(options.target)?;

    let mut root_names = Vec::new();
    let mut root_set = HashSet::new();
    for root in roots {
        let formula = formula(catalog, root.as_ref())?;
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
        if matches!(options.mode, DependencyMode::Pour)
            && ((dependency.build && !options.filter.include_build)
                || (dependency.test && !options.filter.include_test))
        {
            continue;
        }
        expanded.push(dependency);
    }
    Ok(expanded)
}

/// Render one dependency tree without collapsing shared subtrees.
pub(crate) fn tree(
    catalog: &Catalog,
    root: &str,
    options: &DependencyOptions,
) -> Result<DependencyTree, OpError> {
    validate_host(options.target)?;
    let root = formula(catalog, root)?;
    let mut renderer = TreeRenderer {
        catalog,
        options,
        active: Vec::new(),
        lines: vec![root.name.clone()],
        cycle: None,
    };
    renderer.visit(root, "", true)?;
    Ok(DependencyTree {
        lines: renderer.lines,
        cycle: renderer.cycle,
    })
}

/// Find formulae that reach every target through the selected dependency edges.
pub fn uses<I, S>(
    catalog: &Catalog,
    targets: I,
    options: &UsesOptions,
) -> Result<Vec<String>, OpError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    validate_host(options.host)?;

    let mut sought = HashSet::new();
    for target in targets {
        sought.insert(formula(catalog, target.as_ref())?.name.clone());
    }
    if sought.is_empty() {
        return Ok(Vec::new());
    }

    let mut matches = Vec::new();
    for candidate in catalog.iter() {
        if sought.contains(&candidate.name) {
            continue;
        }
        if formula_reaches_all(catalog, candidate, &sought, options)? {
            matches.push(candidate.name.clone());
        }
    }
    matches.sort();
    Ok(matches)
}

/// Find formulae and casks that reach every formula-or-cask target through
/// dependency edges expressible by the signed catalogs.
pub fn uses_with_casks<I, S>(
    catalog: &Catalog,
    casks: &CaskCatalog,
    targets: I,
    options: &UsesOptions,
) -> Result<Vec<String>, OpError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    validate_host(options.host)?;

    let mut sought = HashSet::new();
    for target in targets {
        let requested = target.as_ref();
        if let Some(formula) = catalog.get(requested) {
            sought.insert(Package::Formula(formula.name.clone()));
        } else if let Some(cask) = casks.get(requested) {
            sought.insert(Package::Cask(cask.token.clone()));
        } else {
            return Err(OpError::MissingFormula {
                name: requested.to_owned(),
            });
        }
    }
    if sought.is_empty() {
        return Ok(Vec::new());
    }

    let formula_sought: HashSet<String> = sought
        .iter()
        .filter_map(|target| match target {
            Package::Formula(name) => Some(name.clone()),
            Package::Cask(_) => None,
        })
        .collect();
    let cask_target_present = formula_sought.len() < sought.len();
    let mut matches = HashSet::new();

    // A formula can only reach other formulae.
    if !cask_target_present {
        for candidate in catalog.iter() {
            if formula_sought.contains(&candidate.name) {
                continue;
            }
            if formula_reaches_all(catalog, candidate, &formula_sought, options)? {
                matches.insert(candidate.name.clone());
            }
        }
    }

    // A cask can reach formulae and other casks through its depends_on block.
    for candidate in casks.iter() {
        if sought.contains(&Package::Cask(candidate.token.clone())) {
            continue;
        }
        if cask_reaches_all(catalog, casks, candidate, &sought, options)? {
            matches.insert(candidate.token.clone());
        }
    }

    let mut matches: Vec<_> = matches.into_iter().collect();
    matches.sort();
    Ok(matches)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Package {
    Formula(String),
    Cask(String),
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

        let current = formula(self.catalog, name)?;
        self.active.push(current.name.clone());
        let root_edge = self.roots.contains(&current.name);

        for edge in edges(current, self.options.target)? {
            if !self.options.filter.includes(edge.tags, root_edge) {
                continue;
            }
            let dependency = formula(self.catalog, edge.name)?;
            let name = dependency.name.clone();
            let occurrence = MergedTags::from_tags(edge.tags);
            self.merged
                .entry(name.clone())
                .and_modify(|merged| merged.merge(occurrence))
                .or_insert(occurrence);
            self.visit(&name)?;
        }

        self.active.pop();
        self.complete.insert(current.name.clone());
        if !root_edge {
            self.order.push(current.name.clone());
        }
        Ok(())
    }
}

struct TreeRenderer<'a> {
    catalog: &'a Catalog,
    options: &'a DependencyOptions,
    active: Vec<String>,
    lines: Vec<String>,
    cycle: Option<Vec<String>>,
}

impl TreeRenderer<'_> {
    fn visit(&mut self, current: &Formula, prefix: &str, root_edge: bool) -> Result<(), OpError> {
        self.active.push(current.name.clone());
        let children = edges(current, self.options.target)?
            .into_iter()
            .filter(|edge| self.options.filter.includes(edge.tags, root_edge))
            .map(|edge| formula(self.catalog, edge.name).map(|child| (child, edge.tags)))
            .collect::<Result<Vec<_>, _>>()?;
        let last = children.len().saturating_sub(1);

        for (index, (child, _tags)) in children.into_iter().enumerate() {
            let final_child = index == last;
            let branch = if final_child {
                "└── "
            } else {
                "├── "
            };
            let mut line = format!("{prefix}{branch}{}", child.name);
            if let Some(cycle_start) = self.active.iter().position(|active| active == &child.name) {
                line.push_str(" (CIRCULAR DEPENDENCY)");
                if self.cycle.is_none() {
                    let mut cycle = self.active[cycle_start..].to_vec();
                    cycle.push(child.name.clone());
                    self.cycle = Some(cycle);
                }
                self.lines.push(line);
                continue;
            }

            self.lines.push(line);
            let continuation = if final_child { "    " } else { "│   " };
            self.visit(child, &format!("{prefix}{continuation}"), false)?;
        }
        self.active.pop();
        Ok(())
    }
}

struct Edge<'a> {
    name: &'a str,
    tags: &'a [DependencyTag],
}

fn edges(formula: &Formula, target: BottleTag) -> Result<Vec<Edge<'_>>, OpError> {
    let mut result = Vec::with_capacity(formula.dependencies.len() + formula.uses_from_macos.len());
    for dependency in &formula.dependencies {
        result.push(Edge {
            name: &dependency.name,
            tags: &dependency.tags,
        });
    }
    for dependency in &formula.uses_from_macos {
        if include_uses_from_macos(dependency, target)? {
            result.push(Edge {
                name: &dependency.name,
                tags: &dependency.tags,
            });
        }
    }
    Ok(result)
}

fn formula<'a>(catalog: &'a Catalog, requested: &str) -> Result<&'a Formula, OpError> {
    catalog
        .get(requested)
        .ok_or_else(|| OpError::MissingFormula {
            name: requested.to_owned(),
        })
}

fn validate_host(target: BottleTag) -> Result<(), OpError> {
    if matches!(target, BottleTag::All) {
        Err(OpError::InvalidState {
            reason: "the universal bottle tag is not a host platform".to_owned(),
        })
    } else {
        Ok(())
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

fn formula_reaches_all(
    catalog: &Catalog,
    current: &Formula,
    sought: &HashSet<String>,
    options: &UsesOptions,
) -> Result<bool, OpError> {
    let mut reached = HashSet::new();
    if options.recursive {
        let mut visited = HashSet::from([current.name.clone()]);
        collect_reachable(catalog, current, options, true, &mut visited, &mut reached)?;
    } else {
        for edge in edges(current, options.host)? {
            if options.filter.includes(edge.tags, true) {
                reached.insert(formula(catalog, edge.name)?.name.clone());
            }
        }
    }
    Ok(sought.is_subset(&reached))
}

fn collect_reachable(
    catalog: &Catalog,
    current: &Formula,
    options: &UsesOptions,
    root_edge: bool,
    visited: &mut HashSet<String>,
    reached: &mut HashSet<String>,
) -> Result<(), OpError> {
    for edge in edges(current, options.host)? {
        if !options.filter.includes(edge.tags, root_edge) {
            continue;
        }
        let dependency = formula(catalog, edge.name)?;
        reached.insert(dependency.name.clone());
        if visited.insert(dependency.name.clone()) {
            collect_reachable(catalog, dependency, options, false, visited, reached)?;
        }
    }
    Ok(())
}

fn cask_reaches_all(
    catalog: &Catalog,
    casks: &CaskCatalog,
    current: &Cask,
    sought: &HashSet<Package>,
    options: &UsesOptions,
) -> Result<bool, OpError> {
    let mut reached = HashSet::new();
    if options.recursive {
        let mut visited = HashSet::from([Package::Cask(current.token.clone())]);
        collect_cask_reachable(
            catalog,
            casks,
            current,
            options,
            true,
            &mut visited,
            &mut reached,
        )?;
    } else {
        for name in &current.depends_on.formula {
            if let Some(dependency) = catalog.get(name) {
                reached.insert(Package::Formula(dependency.name.clone()));
            }
        }
        for token in &current.depends_on.cask {
            if let Some(dependency) = casks.get(token) {
                reached.insert(Package::Cask(dependency.token.clone()));
            }
        }
    }
    Ok(sought.is_subset(&reached))
}

fn collect_cask_reachable(
    catalog: &Catalog,
    casks: &CaskCatalog,
    current: &Cask,
    options: &UsesOptions,
    root_edge: bool,
    visited: &mut HashSet<Package>,
    reached: &mut HashSet<Package>,
) -> Result<(), OpError> {
    for name in &current.depends_on.formula {
        let Some(dependency) = catalog.get(name) else {
            continue;
        };
        let package = Package::Formula(dependency.name.clone());
        reached.insert(package.clone());
        if visited.insert(package) {
            let mut formula_visited = HashSet::from([dependency.name.clone()]);
            let mut formula_reached = HashSet::new();
            collect_reachable(
                catalog,
                dependency,
                options,
                false,
                &mut formula_visited,
                &mut formula_reached,
            )?;
            for name in formula_reached {
                reached.insert(Package::Formula(name));
            }
        }
    }
    for token in &current.depends_on.cask {
        let Some(dependency) = casks.get(token) else {
            continue;
        };
        let package = Package::Cask(dependency.token.clone());
        reached.insert(package.clone());
        if visited.insert(package) {
            collect_cask_reachable(catalog, casks, dependency, options, false, visited, reached)?;
        }
    }
    let _ = root_edge;
    Ok(())
}
