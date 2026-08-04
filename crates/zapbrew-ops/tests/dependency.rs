use std::fmt::Debug;

use zapbrew_api::Catalog;
use zapbrew_ops::OpError;
use zapbrew_ops::dependency::{
    DependencyMode, DependencyOptions, EdgeFilter, Necessity, UsesOptions, expand, uses,
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

fn uses_options(host: BottleTag) -> UsesOptions {
    UsesOptions {
        host,
        recursive: false,
        filter: EdgeFilter::default(),
    }
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
            filter: EdgeFilter::ALL,
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
            filter: EdgeFilter::default(),
        },
    ));

    let names: Vec<&str> = expanded
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect();
    assert_eq!(names, ["middle", "shared"]);
}

#[test]
fn pour_filter_keeps_test_dependencies_when_include_test_is_set() {
    let target = tag("linux", None);
    let catalog = catalog(
        r#"[
          {"name":"root","full_name":"root","versions":{"stable":"1"},
           "test_dependencies":["testonly"]},
          {"name":"testonly","full_name":"testonly","versions":{"stable":"1"}}
        ]"#,
        target,
    );
    let expanded = ok(expand(
        &catalog,
        ["root"],
        &DependencyOptions {
            target,
            mode: DependencyMode::Pour,
            filter: EdgeFilter::query(false, true, false, false),
        },
    ));

    let names: Vec<&str> = expanded
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect();
    assert_eq!(names, ["testonly"]);
}

#[test]
fn pour_filter_drops_test_dependencies_when_include_test_is_false() {
    let target = tag("linux", None);
    let catalog = catalog(
        r#"[
          {"name":"root","full_name":"root","versions":{"stable":"1"},
           "test_dependencies":["testonly"]},
          {"name":"testonly","full_name":"testonly","versions":{"stable":"1"}}
        ]"#,
        target,
    );
    let expanded = ok(expand(
        &catalog,
        ["root"],
        &DependencyOptions {
            target,
            mode: DependencyMode::Pour,
            filter: EdgeFilter::default(),
        },
    ));

    let names: Vec<&str> = expanded
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect();
    assert!(names.is_empty());
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
            filter: EdgeFilter::ALL,
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
            filter: EdgeFilter::ALL,
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
            filter: EdgeFilter::ALL,
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
            filter: EdgeFilter::ALL,
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
        filter: EdgeFilter::ALL,
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
            filter: EdgeFilter::ALL,
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
    let direct = uses_options(target);
    let recursive = UsesOptions {
        recursive: true,
        ..direct
    };

    assert_eq!(
        ok(uses(&catalog, ["base-alias"], &direct)),
        ["alpha", "zeta"]
    );
    assert_eq!(
        ok(uses(&catalog, ["base"], &recursive)),
        ["alpha", "top", "zeta"]
    );
}

#[test]
fn inverse_uses_from_macos_obeys_linux_and_since_bound_hosts() {
    let payload = r#"[
      {"name":"root","full_name":"root","versions":{"stable":"1"},
       "uses_from_macos":["always-system","new-system"],
       "uses_from_macos_bounds":[{}, {"since":"sonoma"}]},
      {"name":"always-system","full_name":"always-system","versions":{"stable":"1"}},
      {"name":"new-system","full_name":"new-system","versions":{"stable":"1"}}
    ]"#;

    let linux = tag("linux", None);
    let linux_catalog = catalog(payload, linux);
    let linux_options = uses_options(linux);
    assert_eq!(
        ok(uses(&linux_catalog, ["always-system"], &linux_options)),
        ["root"]
    );
    assert_eq!(
        ok(uses(&linux_catalog, ["new-system"], &linux_options)),
        ["root"]
    );

    let ventura = tag("macos", Some("ventura"));
    let ventura_catalog = catalog(payload, ventura);
    let ventura_options = uses_options(ventura);
    assert!(ok(uses(&ventura_catalog, ["always-system"], &ventura_options)).is_empty());
    assert_eq!(
        ok(uses(&ventura_catalog, ["new-system"], &ventura_options)),
        ["root"]
    );

    let sonoma = tag("macos", Some("sonoma"));
    let sonoma_catalog = catalog(payload, sonoma);
    let sonoma_options = uses_options(sonoma);
    assert!(ok(uses(&sonoma_catalog, ["new-system"], &sonoma_options)).is_empty());
}

#[test]
fn inverse_uses_filters_edge_classes_until_selected() {
    let host = tag("linux", None);
    let catalog = catalog(
        r#"[
          {"name":"required","full_name":"required","versions":{"stable":"1"}},
          {"name":"recommended","full_name":"recommended","versions":{"stable":"1"}},
          {"name":"recommended-optional","full_name":"recommended-optional","versions":{"stable":"1"}},
          {"name":"optional","full_name":"optional","versions":{"stable":"1"}},
          {"name":"build","full_name":"build","versions":{"stable":"1"}},
          {"name":"test","full_name":"test","versions":{"stable":"1"}},
          {"name":"consumer","full_name":"consumer","versions":{"stable":"1"},
           "dependencies":["required"],"recommended_dependencies":["recommended"],
           "optional_dependencies":["optional"],"build_dependencies":["build"],
           "test_dependencies":["test"],
           "uses_from_macos":[{"recommended-optional":["optional","recommended"]}]}
        ]"#,
        host,
    );
    let default = uses_options(host);

    for target in ["required", "recommended", "recommended-optional"] {
        assert_eq!(ok(uses(&catalog, [target], &default)), ["consumer"]);
    }
    for target in ["optional", "build", "test"] {
        assert!(ok(uses(&catalog, [target], &default)).is_empty());
    }

    assert_eq!(
        ok(uses(
            &catalog,
            ["optional"],
            &UsesOptions {
                filter: EdgeFilter::query(false, false, true, false),
                ..default
            },
        )),
        ["consumer"]
    );
    assert_eq!(
        ok(uses(
            &catalog,
            ["build"],
            &UsesOptions {
                filter: EdgeFilter::query(true, false, false, false),
                ..default
            },
        )),
        ["consumer"]
    );
    assert_eq!(
        ok(uses(
            &catalog,
            ["test"],
            &UsesOptions {
                filter: EdgeFilter::query(false, true, false, false),
                ..default
            },
        )),
        ["consumer"]
    );
}

#[test]
fn inverse_uses_requires_every_target() {
    let host = tag("linux", None);
    let catalog = catalog(
        r#"[
          {"name":"left","full_name":"left","versions":{"stable":"1"}},
          {"name":"right","full_name":"right","versions":{"stable":"1"}},
          {"name":"left-only","full_name":"left-only","versions":{"stable":"1"},"dependencies":["left"]},
          {"name":"right-only","full_name":"right-only","versions":{"stable":"1"},"dependencies":["right"]},
          {"name":"both","full_name":"both","versions":{"stable":"1"},"dependencies":["left","right"]}
        ]"#,
        host,
    );

    assert_eq!(
        ok(uses(&catalog, ["left", "right"], &uses_options(host))),
        ["both"]
    );
}

#[test]
fn recursive_inverse_uses_is_cycle_safe() {
    let host = tag("linux", None);
    let catalog = catalog(
        r#"[
          {"name":"base","full_name":"base","versions":{"stable":"1"}},
          {"name":"a","full_name":"a","versions":{"stable":"1"},"dependencies":["b"]},
          {"name":"b","full_name":"b","versions":{"stable":"1"},"dependencies":["a","base"]}
        ]"#,
        host,
    );
    let recursive = UsesOptions {
        recursive: true,
        ..uses_options(host)
    };

    assert_eq!(ok(uses(&catalog, ["base"], &recursive)), ["a", "b"]);
}

#[test]
fn inverse_uses_reports_a_missing_target() {
    let host = tag("linux", None);
    let catalog = catalog(
        r#"[{"name":"present","full_name":"present","versions":{"stable":"1"}}]"#,
        host,
    );

    let error = match uses(&catalog, ["missing"], &uses_options(host)) {
        Ok(_) => panic!("expected a missing target"),
        Err(error) => error,
    };
    match error {
        OpError::MissingFormula { name } => assert_eq!(name, "missing"),
        other => panic!("expected missing formula, got {other:?}"),
    }
}

#[test]
fn inverse_uses_rejects_the_universal_host_tag() {
    let catalog = catalog(
        r#"[
          {"name":"target","full_name":"target","versions":{"stable":"1"}},
          {"name":"consumer","full_name":"consumer","versions":{"stable":"1"},"dependencies":["target"]}
        ]"#,
        tag("linux", None),
    );

    let error = match uses(&catalog, ["target"], &uses_options(BottleTag::All)) {
        Ok(_) => panic!("expected a concrete host requirement"),
        Err(error) => error,
    };
    match error {
        OpError::InvalidState { reason } => {
            assert_eq!(reason, "the universal bottle tag is not a host platform");
        }
        other => panic!("expected invalid state, got {other:?}"),
    }
}

fn query_names(
    catalog: &Catalog,
    host: BottleTag,
    filter: EdgeFilter,
) -> std::collections::BTreeSet<String> {
    ok(expand(
        catalog,
        ["root"],
        &DependencyOptions {
            target: host,
            mode: DependencyMode::All,
            filter,
        },
    ))
    .into_iter()
    .map(|dependency| dependency.name)
    .collect()
}

#[test]
fn query_filter_truth_table_is_disjunctive_after_recommended_ignore() {
    let host = tag("linux", None);
    let catalog = catalog(
        r#"[
          {"name":"root","full_name":"root","versions":{"stable":"1"},
           "dependencies":["required"],"recommended_dependencies":["recommended"],
           "optional_dependencies":["optional"],"build_dependencies":["build"],
           "test_dependencies":["test"],
           "uses_from_macos":[
             {"recommended-build":["recommended","build"]},
             {"recommended-test":["recommended","test"]},
             {"recommended-optional":["recommended","optional"]},
             {"build-optional":["build","optional"]}
           ]},
          {"name":"required","full_name":"required","versions":{"stable":"1"}},
          {"name":"recommended","full_name":"recommended","versions":{"stable":"1"}},
          {"name":"optional","full_name":"optional","versions":{"stable":"1"}},
          {"name":"build","full_name":"build","versions":{"stable":"1"}},
          {"name":"test","full_name":"test","versions":{"stable":"1"}},
          {"name":"recommended-build","full_name":"recommended-build","versions":{"stable":"1"}},
          {"name":"recommended-test","full_name":"recommended-test","versions":{"stable":"1"}},
          {"name":"recommended-optional","full_name":"recommended-optional","versions":{"stable":"1"}},
          {"name":"build-optional","full_name":"build-optional","versions":{"stable":"1"}}
        ]"#,
        host,
    );
    let cases = [
        (
            EdgeFilter::default(),
            vec![
                "recommended",
                "recommended-build",
                "recommended-optional",
                "recommended-test",
                "required",
            ],
        ),
        (
            EdgeFilter::query(true, false, false, false),
            vec![
                "build",
                "build-optional",
                "recommended",
                "recommended-build",
                "recommended-optional",
                "recommended-test",
                "required",
            ],
        ),
        (
            EdgeFilter::query(false, true, false, false),
            vec![
                "recommended",
                "recommended-build",
                "recommended-optional",
                "recommended-test",
                "required",
                "test",
            ],
        ),
        (
            EdgeFilter::query(false, false, true, false),
            vec![
                "build-optional",
                "optional",
                "recommended",
                "recommended-build",
                "recommended-optional",
                "recommended-test",
                "required",
            ],
        ),
        (
            EdgeFilter::query(false, false, false, true),
            vec!["required"],
        ),
        (
            EdgeFilter::query(true, true, true, true),
            vec!["build", "build-optional", "optional", "required", "test"],
        ),
    ];

    for (filter, expected) in cases {
        assert_eq!(
            query_names(&catalog, host, filter),
            expected.into_iter().map(str::to_owned).collect()
        );
    }
}

#[test]
fn query_test_edges_are_root_only_and_excluded_edges_are_pruned() {
    let host = tag("linux", None);
    let catalog = catalog(
        r#"[
          {"name":"root","full_name":"root","versions":{"stable":"1"},
           "dependencies":["child"],"test_dependencies":["root-test"],
           "optional_dependencies":["missing-optional"]},
          {"name":"child","full_name":"child","versions":{"stable":"1"},
           "test_dependencies":["indirect-test"]},
          {"name":"root-test","full_name":"root-test","versions":{"stable":"1"}},
          {"name":"indirect-test","full_name":"indirect-test","versions":{"stable":"1"}}
        ]"#,
        host,
    );

    assert_eq!(
        query_names(&catalog, host, EdgeFilter::query(false, true, false, false)),
        ["child", "root-test"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
}
