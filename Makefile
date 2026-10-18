# Tailwind v4 standalone CLI (no Node.js needed):
# https://github.com/tailwindlabs/tailwindcss/releases/tag/v4.3.3
TAILWIND ?= tailwindcss
HELM ?= helm
# The visual job's image: screenshots are only comparable when rendered by
# the same OS, fonts and Chromium build. Its Playwright version must be the
# one playwright-rs drives (see its PLAYWRIGHT_VERSION) for the browsers in
# the image to be found.
VISUAL_IMAGE ?= mcr.microsoft.com/playwright:v1.63.0-noble
DOCKER ?= docker
# Extra `docker run` flags, e.g. a proxy: DOCKER_FLAGS="--network host -e HTTPS_PROXY".
DOCKER_FLAGS ?=

.PHONY: css check demo helm-lint visual visual-update visual-docker visual-docker-update

css: ## Rebuild assets/dist/app.css from assets/app.css and the templates
	$(TAILWIND) -i assets/app.css -o assets/dist/app.css --minify

check: ## What CI runs for the Rust code
	cargo fmt --check
	cargo clippy --all-targets --locked -- -D warnings
	cargo test --locked

demo: ## Run the UI with sample packages and no auth
	cargo run -- --demo

visual: ## Browser tests: screenshots against tests/screenshots/ (inside $(VISUAL_IMAGE))
	cargo test --locked --test visual -- --ignored

visual-update: ## Rewrite tests/screenshots/ (inside $(VISUAL_IMAGE))
	UPDATE_SNAPSHOTS=1 cargo test --locked --test visual -- --ignored

# The same as CI's visual job, from any machine with Docker. The image has
# the browsers; a C compiler and Rust are added (and Rust kept in a volume).
VISUAL_SETUP = (command -v cc >/dev/null || (apt-get update -qq && apt-get install -y -qq --no-install-recommends build-essential >/dev/null)) \
	&& (test -x /root/.cargo/bin/rustup || (curl -sSf -o /tmp/rustup-init https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init \
	&& chmod +x /tmp/rustup-init && /tmp/rustup-init -y -q --profile minimal --default-toolchain none)) \
	&& . /root/.cargo/env
VISUAL_RUN = $(DOCKER) run --rm $(DOCKER_FLAGS) -v "$(CURDIR)":/src -w /src \
	-v lastmile-visual-rustup:/root/.rustup -v lastmile-visual-cargo:/root/.cargo \
	-v lastmile-visual-target:/target -e CARGO_TARGET_DIR=/target \
	$(VISUAL_IMAGE) sh -c

visual-docker: ## `make visual` in the CI image
	$(VISUAL_RUN) '$(VISUAL_SETUP) && make visual'

visual-docker-update: ## `make visual-update` in the CI image: commit the new PNGs
	$(VISUAL_RUN) '$(VISUAL_SETUP) && make visual-update'

helm-lint: ## Lint and render the chart
	$(HELM) lint deploy/helm/lastmile --set oidc.issuer=https://idp.example.com
	$(HELM) template lastmile deploy/helm/lastmile --set oidc.issuer=https://idp.example.com \
	  --set serviceMonitor.enabled=true >/dev/null
	HELM=$(HELM) deploy/helm/tests/render.sh
