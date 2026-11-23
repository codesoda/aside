# #32 engine acceptance map

The production adapter/pipeline/UI gates belong to their respective owners. Engine assertions:

| Scope | Engine tests |
| --- | --- |
| Pinned Git objects, production diff, cross-file imports, sites, stable IDs | `acceptance_tests::committed_cross_file_graph_has_shortest_paths_sites_and_stable_ids` |
| Changed HEAD/base rows, deleted LEFT anchors, unavailable base graph | `acceptance_tests::deletion_uses_base_anchor_and_never_claims_base_relationships`, `tests::changed_callee_has_cross_file_dependent_and_stable_id` |
| Rename path lineage, ambiguous identities | `acceptance_tests::renamed_module_file_preserves_identity_but_not_base_relationships`, `acceptance_tests::collision_is_marked_ambiguous_and_cannot_resolve_a_caller`, `contract_tests::ambiguous_rename_predecessor_has_no_lineage` |
| Zero-edge and callee-only results | `acceptance_tests::zero_edge_and_callee_only_changed_entities_still_emit_results` |
| Deterministic shortest paths, cycles, depth and visit caps | `acceptance_tests::depth_visit_and_shuffled_blob_order_are_deterministic`, `extra_tests::cycles_do_not_repeat_and_depth_is_bounded`, `extra_tests::visit_cap_and_zero_edge` |
| Shadowing, cfg, namespace, unsupported dispatch and layouts | `acceptance_tests::shadowed_and_unsupported_sites_never_become_false_caller_edges`, `contract_tests::conditional_import_and_namespace_collision_fail_closed`, `contract_tests::closure_and_generic_shadowing_remain_unresolved`,  `contract_tests::explicit_targets_shared_files_and_unlinked_sources_fail_closed` |
| Conventional manifest roots, inline/bin modules, imports, type references and inventory categories | `contract_tests::inline_and_bin_target_module_paths_are_distinct`, `contract_tests::grouped_alias_import_and_shadowing`, `contract_tests::imported_module_alias_resolves_only_unambiguous_calls`, `contract_tests::named_type_reference_has_site_and_dependency`, `contract_tests::inventory_functions_types_constants_modules_and_macros` |
| Full pinned tree path attestation, parser budget/blob and enumeration caps, cancellation, source mismatch | `contract_tests::full_tree_path_attestation_controls_completeness`, `contract_tests::parse_budget_and_blob_size_are_explicit`,  `tests::caps_and_unsupported_layout_are_explicit`, `tests::source_mismatch_falls_back_and_cancellation_skips` |

Limitations intentionally reported as unresolved or partial: arbitrary Cargo target paths/workspaces/build scripts, macro expansion, globs/re-exports, associated and dynamic dispatch, unlinked files, non-Rust languages, and base graph. The engine only sees objects supplied by the adapter. The adapter must call `analyze_with_corpus` with the full pinned HEAD tree paths; the compatibility `analyze` wrapper fails completeness closed when path coverage is not attested. Unreadable objects remain an adapter coverage concern. No universal Rust completeness is claimed.
