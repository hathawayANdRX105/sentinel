from __future__ import annotations
import tempfile
import unittest
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]



from stats import draft as build_draft_stats
from reports import kit as build_review_kit
from reports import learning as build_review_learning_logs
from reports import scorecard as build_review_scorecards
import consistency as consistency_index
from audit import draft as draft_audit
from lib import io as review_io
from lib import paths as review_paths


class ReviewOutputTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.novel_story = ROOT.parent / "novel-novel2" / "novel1" / "drafts" / "arc1" / "story3"
        if not cls.novel_story.exists():
            raise unittest.SkipTest(f"novel data not available at {cls.novel_story}")
        cls.story_dir = cls.novel_story
        cls.files = build_review_scorecards.collect_chapter_files([str(cls.story_dir)])[:3]
        cls.corpus_profile = draft_audit.build_corpus_profile(draft_audit.corpus_paths_for_targets(cls.files))
        cls.analyses = [
            (
                path,
                draft_audit.analyze_path(path, sample_limit=4, corpus_profile=cls.corpus_profile),
            )
            for path in cls.files
        ]
        cls.snapshots = {
            path: consistency_index.build_story_conflict_snapshot_from_path(path)
            for path in cls.files
        }
        analysis = {
            "ending": {
                "tail_excerpt": "冷光在墙面上停了一下，像一道不肯散的影子。",
                "flow_terms": [],
                "image_terms": [{"term": "冷光", "count": 1}, {"term": "影子", "count": 1}],
            }
        }
        if build_draft_stats.infer_ending_label(analysis) != "imagery_coda":
            raise AssertionError("expected imagery ending label fixture")

    def test_reviewlib_paths_and_io_helpers(self) -> None:
        draft_path = ROOT / "novel1" / "drafts" / "arc1" / "story3" / "ch01.md"
        stats_path = review_paths.stats_path_for(draft_path)
        self.assertTrue(str(stats_path).endswith("novel1/draft-stats/arc1/story3/ch01.md"))

        with tempfile.TemporaryDirectory() as tmpdir:
            target = Path(tmpdir) / "nested" / "sample.txt"
            review_io.write_text(target, "ok")
            self.assertEqual(target.read_text(encoding="utf-8"), "ok")

    def test_scorecard_and_learning_summaries_include_ending_trends(self) -> None:
        scorecard_summary = build_review_scorecards.build_story_summary(
            self.story_dir,
            self.analyses,
            self.snapshots,
        )
        learning_summary = build_review_learning_logs.build_story_summary(
            self.story_dir,
            self.analyses,
            self.snapshots,
        )

        self.assertIn("## Ending Trend Signals", scorecard_summary)
        self.assertIn("flow=`", scorecard_summary)
        self.assertIn("## Ending Trend Signals", learning_summary)
        self.assertIn("repeated=`", learning_summary)

    def test_story_trend_convergence_can_raise_scorecard_risk(self) -> None:
        trend_snapshots = build_review_scorecards.build_story_trend_snapshots(self.analyses)
        converging_path = self.files[1]
        self.assertIn(converging_path, trend_snapshots)
        self.assertIn("ending_tone", trend_snapshots[converging_path]["convergence_kinds"])

        alignment = {"available": False}
        base_axes = build_review_scorecards.build_axes(
            self.analyses[1][1],
            self.snapshots[converging_path],
            alignment,
            None,
        )
        base_gate, _base_priority, _base_recommendation = build_review_scorecards.decide_gate(
            self.analyses[1][1],
            base_axes,
        )

        converged_axes = build_review_scorecards.build_axes(
            self.analyses[1][1],
            self.snapshots[converging_path],
            alignment,
            trend_snapshots[converging_path],
        )
        converged_gate, _priority, _recommendation = build_review_scorecards.decide_gate(
            self.analyses[1][1],
            converged_axes,
        )

        gate_rank = {"PASS": 0, "WATCH": 1, "FAIL": 2}
        self.assertGreaterEqual(gate_rank[converged_gate], gate_rank[base_gate])

    def test_review_kit_assigns_repeated_ending_trend_review(self) -> None:
        assignments = build_review_kit.collect_review_assignments(
            self.story_dir,
            self.analyses,
            self.snapshots,
        )
        self.assertTrue(
            any(item.get("source") == "ending_trends" for item in assignments),
            "expected review kit to emit an ending_trends assignment",
        )


if __name__ == "__main__":
    unittest.main()
