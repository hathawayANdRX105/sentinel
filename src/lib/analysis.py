from __future__ import annotations

from pathlib import Path

from audit import draft as draft_audit


def build_corpus_profile_for_files(files: list[Path]) -> draft_audit.CorpusProfile | None:
    return draft_audit.build_corpus_profile(draft_audit.corpus_paths_for_targets(files))


def analyze_files(
    files: list[Path],
    *,
    sample_limit: int,
    corpus_profile: draft_audit.CorpusProfile | None = None,
) -> list[tuple[Path, dict[str, object]]]:
    template_bank = draft_audit.load_template_bank(draft_audit.DEFAULT_REVIEW_RULES_PATH)
    term_bank = draft_audit.load_term_bank(draft_audit.DEFAULT_REVIEW_RULES_PATH)
    return [
        (
            draft_path,
            draft_audit.analyze_path(
                draft_path,
                sample_limit=sample_limit,
                corpus_profile=corpus_profile,
                template_bank=template_bank,
                term_bank=term_bank,
            ),
        )
        for draft_path in files
    ]
