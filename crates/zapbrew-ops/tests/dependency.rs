use std::fmt::Debug;

use zapbrew_api::Catalog;
use zapbrew_ops::OpError;
use zapbrew_ops::dependency::{
    DependencyMode, DependencyOptions, Necessity, UsesMode, expand, uses,
};
use zapbrew_types::BottleTag;

fn ok<T, E: Debug>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("expected success, got {error:?}"),
    }
}

fn tag(os: &str, version: Option<&str>) -> BottleTag {
    match BottleTag::from_host(os, "x86_64", version) {
        Some(tag) => tag,
        None => panic!("expected a supported test host"),
    }
}

fn catalog(payload: &str, target: BottleTag) -> Catalog {
    ok(Catalog::from_payload(payload.as_bytes(), &target))
}

const MERGE_GRAPH: &str = r#"[
  {"name":"root","full_name":"root","versions":{"stable":"1"},
   "dependencies":["middle","shared"],"build_dependencies":["shared","buildonly"]},
  {"name":"middle","full_name":"middle","versions":{"stable":"1"},
   "optional_dependencies":["shared"],"test_dependencies":["shared"],
   "build_dependencies":["buildonly"]},
  {"name":"shared","full_name":"shared","versions":{"stable":"1"}},
  {"name":"buildonly","full_name":"buildonly","versions":{"stable":"1"}}
]"#;

#[test]
fn expansion_is_post_order_and_merges_every_occurrence() {
    let target = tag("linux", None);
    let catalog = catalog(MERGE_GRAPH, target);
    let expanded = ok(expand(
        &catalog,
        ["root"],
        &DependencyOptions {
            target,
            mode: DependencyMode::All,
        },
    ));

    let names: Vec<&str> = expanded
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect();
    assert_eq!(names, ["buildonly", "shared", "middle"]);

    let shared = &expanded[1];
    assert_eq!(shared.necessity, Necessity::Required);
    assert!(
        !shared.build,
        "one runtime occurrence clears the build-only tag"
    );
    assert!(shared.test, "any test occurrence preserves the test tag");

    let buildonly = &expanded[0];
    assert!(buildonly.build, "all occurrences are build dependencies");
    assert!(!buildonly.test);
}

#[test]
fn pour_filter_runs_after_duplicate_tag_merge() {
    let target = tag("linux", None);
    let catalog = catalog(MERGE_GRAPH, target);
    let expanded = ok(expand(
        &catalog,
        ["root"],
        &DependencyOptions {
            target,
            mode: DependencyMode::Pour,
        },
    ));

    let names: Vec<&str> = expanded
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect();
    assert_eq!(names, ["middle"]);
}

#[test]
fn active_stack_reports_the_closed_cycle() {
    let target = tag("linux", None);
    let catalog = catalog(
        r#"[
          {"name":"a","full_name":"a","versions":{"stable":"1"},"dependencies":["b"]},
          {"name":"b","full_name":"b","versions":{"stable":"1"},"dependencies":["c"]},
          {"name":"c","full_name":"c","versions":{"stable":"1"},"dependencies":["b"]}
        ]"#,
        target,
    );

    let error = match expand(
        &catalog,
        ["a"],
        &DependencyOptions {
            target,
            mode: DependencyMode::All,
        },
    ) {
        Ok(_) => panic!("expected a dependency cycle"),
        Err(error) => error,
    };
    match error {
        OpError::DependencyCycle { cycle } => assert_eq!(cycle, ["b", "c", "b"]),
        other => panic!("expected cycle error, got {other:?}"),
    }
}

#[test]
fn uses_from_macos_obeys_linux_and_since_bound_hosts() {
    let payload = r#"[
      {"name":"root","full_name":"root","versions":{"stable":"1"},
       "uses_from_macos":["always-system",{"new-system":"build"}],
       "uses_from_macos_bounds":[{}, {"since":"sonoma"}]},
      {"name":"always-system","full_name":"always-system","versions":{"stable":"1"}},
      {"name":"new-system","full_name":"new-system","versions":{"stable":"1"}}
    ]"#;

    let linux = tag("linux", None);
    let linux_catalog = catalog(payload, linux);
    let on_linux = ok(expand(
        &linux_catalog,
        ["root"],
        &DependencyOptions {
            target: linux,
            mode: DependencyMode::All,
        },
    ));
    assert_eq!(
        on_linux
            .iter()
            .map(|dependency| dependency.name.as_str())
            .collect::<Vec<_>>(),
        ["always-system", "new-system"]
    );

    let ventura = tag("macos", Some("ventura"));
    let ventura_catalog = catalog(payload, ventura);
    let before_bound = ok(expand(
        &ventura_catalog,
        ["root"],
        &DependencyOptions {
            target: ventura,
            mode: DependencyMode::All,
        },
    ));
    assert_eq!(before_bound.len(), 1);
    assert_eq!(before_bound[0].name, "new-system");

    let sonoma = tag("macos", Some("sonoma"));
    let sonoma_catalog = catalog(payload, sonoma);
    let at_bound = ok(expand(
        &sonoma_catalog,
        ["root"],
        &DependencyOptions {
            target: sonoma,
            mode: DependencyMode::All,
        },
    ));
    assert!(at_bound.is_empty());
}

#[test]
fn missing_formula_is_typed_for_roots_and_edges() {
    let target = tag("linux", None);
    let catalog = catalog(
        r#"[{"name":"root","full_name":"root","versions":{"stable":"1"},"dependencies":["gone"]}]"#,
        target,
    );
    let options = DependencyOptions {
        target,
        mode: DependencyMode::All,
    };

    for requested in ["missing-root", "root"] {
        let error = match expand(&catalog, [requested], &options) {
            Ok(_) => panic!("expected missing formula for {requested}"),
            Err(error) => error,
        };
        match error {
            OpError::MissingFormula { name } => {
                let expected = if requested == "root" {
                    "gone"
                } else {
                    requested
                };
                assert_eq!(name, expected);
            }
            other => panic!("expected missing formula, got {other:?}"),
        }
    }
}

#[test]
fn recommended_outranks_optional_within_one_tagged_occurrence() {
    let target = tag("linux", None);
    let catalog = catalog(
        r#"[
          {"name":"root","full_name":"root","versions":{"stable":"1"},
           "uses_from_macos":[{"dep":["optional","recommended"]}]},
          {"name":"dep","full_name":"dep","versions":{"stable":"1"}}
        ]"#,
        target,
    );
    let expanded = ok(expand(
        &catalog,
        ["root"],
        &DependencyOptions {
            target,
            mode: DependencyMode::All,
        },
    ));

    assert_eq!(expanded.len(), 1);
    assert_eq!(expanded[0].necessity, Necessity::Recommended);
}

#[test]
fn inverse_uses_is_sorted_unique_and_optionally_recursive() {
    let target = tag("linux", None);
    let catalog = catalog(
        r#"[
          {"name":"base","full_name":"base","aliases":["base-alias"],"versions":{"stable":"1"}},
          {"name":"zeta","full_name":"zeta","versions":{"stable":"1"},"dependencies":["base"]},
          {"name":"alpha","full_name":"alpha","versions":{"stable":"1"},"recommended_dependencies":["base-alias"]},
          {"name":"top","full_name":"top","versions":{"stable":"1"},"dependencies":["zeta","alpha"]}
        ]"#,
        target,
    );

    assert_eq!(
        ok(uses(&catalog, ["base-alias"], UsesMode::Direct)),
        ["alpha", "zeta"]
    );
    assert_eq!(
        ok(uses(&catalog, ["base"], UsesMode::Recursive)),
        ["alpha", "top", "zeta"]
    );
}
