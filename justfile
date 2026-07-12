set dotenv-load := false

export PYTHONPATH := "src"

# Show active bd tasks.
bd:
    bd list --status open,in_progress --limit 20

# Run rule/config tests.
test-rules:
    python3 -m unittest tests.test_rules_config -v

# Run output integration tests.
test-outputs:
    python3 -m unittest tests.test_outputs -v

# Run focused local verification.
test:
    python3 -m unittest tests.test_rules_config tests.test_outputs tests.test_real_draft_smoke -v

# Smoke the plan audit CLI. Usage: just smoke-plan path/to/plan.md /tmp/plan.md
smoke-plan input output:
    python3 -m audit.plan --input {{input}} --output {{output}}

# Smoke the draft audit CLI. Usage: just smoke-draft path/to/ch01.md /tmp/draft.md
smoke-draft input output:
    python3 -m audit.draft --input {{input}} --format markdown --output {{output}}

# Real-draft stats smoke with metric summary. Writes under /tmp only.
# Override: just smoke-real-stats /other/story/dir
smoke-real-stats story_dir="$HOME/projects/novel/novel1/drafts/story-3-foreign-whispers":
    python3 scripts/smoke_real_stats.py {{story_dir}}

# Real single-chapter audit smoke. Writes /tmp/sentinel-real-smoke-audit.md only.
smoke-real-audit chapter="$HOME/projects/novel/novel1/drafts/story-3-foreign-whispers/ch01.md":
    python3 scripts/smoke_real_audit.py {{chapter}}
