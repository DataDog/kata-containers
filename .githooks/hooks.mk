install-hooks:
	@hook="$$(git rev-parse --git-common-dir)/hooks/pre-push"; \
	if [ -e "$$hook" ] && [ ! -L "$$hook" ]; then echo "$$hook already exists, not overwriting" >&2; exit 1; fi; \
	ln -sf ../../.githooks/pre-push "$$hook" && echo "installed $$hook"

.PHONY: install-hooks

release-tag:
	@.githooks/release-tag

.PHONY: release-tag
