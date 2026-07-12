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
    python3 -m unittest tests.test_rules_config tests.test_outputs -v

# Smoke the plan audit CLI. Usage: just smoke-plan path/to/plan.md /tmp/plan.md
smoke-plan input output:
    python3 -m audit.plan --input {{input}} --output {{output}}

# Smoke the draft audit CLI. Usage: just smoke-draft path/to/ch01.md /tmp/draft.md
smoke-draft input output:
    python3 -m audit.draft --input {{input}} --format markdown --output {{output}}
