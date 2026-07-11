from __future__ import annotations

import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from sentinel.audit import draft as draft_audit
from sentinel.lib import rules


class ReviewRulesConfigTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.rules = rules.load_rules()
        cls.draft = rules.mapping_at(cls.rules, "draft")
        cls.plan = rules.mapping_at(cls.rules, "plan")

    def test_draft_regex_tokens_include_bushi(self) -> None:
        tokens = rules.list_at(self.draft, "regex_rules", "tokens")
        names = {str(item.get("name")) for item in tokens if isinstance(item, dict)}
        self.assertIn("不是", names)

    def test_draft_template_rules_include_bushi_ershi(self) -> None:
        templates = rules.list_at(self.draft, "template_rules")
        names = {str(item.get("name")) for item in templates if isinstance(item, dict)}
        self.assertIn("不是A而是B", names)

    def test_draft_tracked_terms_include_leide(self) -> None:
        terms = rules.list_at(self.draft, "tracked_terms")
        names = {str(item.get("term")) for item in terms if isinstance(item, dict)}
        self.assertIn("雷德", names)

    def test_draft_ending_label_display_imagery(self) -> None:
        display = rules.mapping_at(self.draft, "ending_labels", "display")
        self.assertEqual(display.get("imagery_coda"), "意象压轴")

    def test_plan_required_headings_chapter_function(self) -> None:
        headings = rules.mapping_at(self.plan, "required_headings")
        chapter_groups = headings.get("chapter-plan")
        self.assertIsInstance(chapter_groups, list)
        flat = {str(name) for group in chapter_groups for name in group}
        self.assertIn("本章功能", flat)

    def test_plan_function_rules_chapter_conflict(self) -> None:
        chapter_rules = rules.mapping_at(self.plan, "function_rules", "chapter")
        conflict = chapter_rules.get("conflict")
        self.assertIsInstance(conflict, list)
        self.assertIn("冲突", conflict)

    def test_inactive_candidates_are_not_active_templates(self) -> None:
        inactive = rules.list_at(self.draft, "inactive_template_candidates")
        active = rules.list_at(self.draft, "template_rules")
        inactive_names = {str(item.get("name")) for item in inactive if isinstance(item, dict)}
        active_names = {str(item.get("name")) for item in active if isinstance(item, dict)}
        self.assertTrue(inactive_names)
        self.assertTrue(inactive_names.isdisjoint(active_names))

    def test_analyze_text_hits_yaml_template_rule(self) -> None:
        text = (
            "这不是冲动，而是判断。\n"
            "那不是巧合，而是设计。\n"
            "他不是退让，而是换位。\n"
        )
        analysis = draft_audit.analyze_text(
            text,
            sample_limit=3,
            source="fixture",
            template_bank=draft_audit.load_template_bank(draft_audit.DEFAULT_REVIEW_RULES_PATH),
            term_bank=draft_audit.load_term_bank(draft_audit.DEFAULT_REVIEW_RULES_PATH),
            corpus_profile=None,
        )
        patterns = analysis.get("patterns") or []
        hits = [item for item in patterns if str(item.get("name")) == "不是A而是B"]
        self.assertTrue(hits, "expected YAML pattern 不是A而是B to hit")
        self.assertGreaterEqual(int(hits[0]["count"]), 1)


if __name__ == "__main__":
    unittest.main()
