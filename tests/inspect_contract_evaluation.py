"""Offline, hand-authored evaluation of a possible Inspect static JSON mapping.

Run: python3 -m unittest -v tests/inspect_contract_evaluation.py
No Inspect binary, network, model, or Aside runtime is imported or invoked.
"""

import unittest


class IncompatibleInput(ValueError):
    pass


def map_static_review(document):
    """Evaluate a deliberately narrow subset, not a production adapter."""
    if not isinstance(document, dict) or set(document) != {
        "entity_reviews", "groups", "stats", "timing"
    }:
        raise IncompatibleInput("missing or incompatible ReviewResult envelope")
    reviews, stats = document["entity_reviews"], document["stats"]
    if not isinstance(reviews, list) or not isinstance(document["groups"], list) or not isinstance(document["timing"], dict):
        raise IncompatibleInput("invalid envelope types")
    if not isinstance(stats, dict) or not isinstance(stats.get("by_risk"), dict):
        raise IncompatibleInput("missing statistics")
    counts = stats["by_risk"]
    if any(type(counts.get(level)) is not int or counts[level] < 0 for level in ("low", "medium", "high", "critical")):
        raise IncompatibleInput("invalid risk breakdown")
    if type(stats.get("total_entities")) is not int or stats["total_entities"] != len(reviews) or sum(counts.values()) != len(reviews):
        raise IncompatibleInput("incomplete or inconsistent entity list")
    levels = {"Low": "low", "Medium": "medium", "High": "high", "Critical": "critical"}
    observed = dict.fromkeys(counts, 0)
    output = []
    for review in reviews:
        required = ("entity_id", "entity_name", "entity_type", "file_path", "change_type",
                    "classification", "risk_score", "risk_level", "blast_radius",
                    "dependent_count", "dependency_count", "is_public_api", "structural_change",
                    "group_id", "start_line", "end_line", "dependent_names", "dependency_names")
        if not isinstance(review, dict) or any(key not in review for key in required):
            raise IncompatibleInput("incomplete entity")
        level, score = review["risk_level"], review["risk_score"]
        if (not isinstance(level, str) or level not in levels or
                type(score) not in (float, int) or not 0 <= score <= 1):
            raise IncompatibleInput("invalid heuristic")
        if not isinstance(review["file_path"], str) or not review["file_path"] or not isinstance(review["entity_name"], str) or not review["entity_name"]:
            raise IncompatibleInput("missing identity")
        if any(type(review[key]) is not int or review[key] < 0 for key in ("start_line", "end_line", "blast_radius", "dependent_count", "dependency_count", "group_id")):
            raise IncompatibleInput("invalid entity numbers")
        structural = review["structural_change"]
        if (review["start_line"] > review["end_line"] or
                (structural is not None and type(structural) is not bool)):
            raise IncompatibleInput("invalid entity range or change")
        observed[levels[level]] += 1
        output.append({
            "path": review["file_path"], "entity": review["entity_name"],
            "attention_hint": {"level": levels[level], "score": review["risk_score"],
                               "kind": "upstream_heuristic_unverified"},
            "anchor": None,  # entity source spans are NOT diff-side anchors
            "assertion": "attention_hint", "validation": "unverified",
        })
    if observed != counts:
        raise IncompatibleInput("risk counts disagree with entities")
    return output


def example():
    # Hand-authored shape from tagged source, not a captured upstream response.
    return {
        "entity_reviews": [{
            "entity_id": "sample-id", "entity_name": "parse", "entity_type": "function",
            "file_path": "src/parser.rs", "change_type": "Modified",
            "classification": "Functional", "risk_score": 0.57, "risk_level": "High",
            "blast_radius": 3, "dependent_count": 2, "dependency_count": 1,
            "is_public_api": False, "structural_change": True, "group_id": 0,
            "start_line": 12, "end_line": 24, "dependent_names": [], "dependency_names": [],
        }], "groups": [], "stats": {"total_entities": 1,
            "by_risk": {"low": 0, "medium": 0, "high": 1, "critical": 0}},
        "timing": {},
    }


class ContractEvaluation(unittest.TestCase):
    def test_preserves_heuristic_without_verified_anchor(self):
        mapped = map_static_review(example())
        self.assertEqual(mapped[0]["attention_hint"], {"level": "high", "score": 0.57, "kind": "upstream_heuristic_unverified"})
        self.assertIsNone(mapped[0]["anchor"])
        self.assertEqual(mapped[0]["validation"], "unverified")

    def test_rejects_truncated_list_even_if_json_parses(self):
        data = example()
        data["entity_reviews"] = []
        with self.assertRaises(IncompatibleInput):
            map_static_review(data)

    def test_rejects_incompatible_or_missing_fields(self):
        for mutation in (lambda d: d.pop("stats"),
                         lambda d: d["entity_reviews"][0].pop("risk_score"),
                         lambda d: d["entity_reviews"][0].update(risk_level="Unknown"),
                         lambda d: d["entity_reviews"][0].update(risk_level=[]),
                         lambda d: d["entity_reviews"][0].update(risk_score=float("nan")),
                         lambda d: d["entity_reviews"][0].update(risk_score=10**400),
                         lambda d: d["entity_reviews"][0].update(structural_change=1),
                         lambda d: d["entity_reviews"][0].update(risk_level="Low")):
            with self.subTest(mutation=mutation):
                data = example()
                mutation(data)
                with self.assertRaises(IncompatibleInput):
                    map_static_review(data)


if __name__ == "__main__":
    unittest.main()
