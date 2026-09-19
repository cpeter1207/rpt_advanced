.DEFAULT_GOAL := all
CC ?= cc
CARGO ?= cargo
CARGO_TARGET_DIR ?= target
export CARGO_TARGET_DIR
prefix ?= /usr
multiarch := $(shell $(CC) -print-multiarch)
asteriskmoddir ?= $(prefix)/lib/$(multiarch)/asterisk/modules
libdir := $(prefix)/lib/$(multiarch)/rpt_advanced
LOADER_RUNPATH = $$ORIGIN/$(shell realpath -m --relative-to="$(asteriskmoddir)" "$(libdir)")
docdir := $(prefix)/share/doc/rpt-advanced
DESTDIR ?=
VERSION ?= 0.1.0-alpha7
DIST_NAME := rpt_advanced-$(VERSION)
CFLAGS ?= -O2 -g
MODULE_FLAGS := -std=gnu11 -D_GNU_SOURCE -DAST_MODULE=\"app_rpt_advanced\" -DAST_MODULE_SELF_SYM=__internal_app_rpt_advanced_self -Wall -Wextra -Werror
HEADERS := rust/asterisk/include/rptadv_asterisk_adapter.h rust/product/include/rptadv_product.h rust/control-asterisk-adapter/include/rptadv_control_asterisk_adapter.h rust/file-adapter/include/rptadv_file_adapter.h rust/speech-adapter/include/rptadv_speech_adapter.h rust/media-support/include/rptadv_media_types.h
CPPFLAGS += $(addprefix -I,$(dir $(HEADERS)))
LOADER := module/app_rpt_advanced_loader.c
ADAPTERS := asterisk control_asterisk file speech
LIBRARIES := $(addprefix build/librptadv_,$(addsuffix _adapter.so.1,$(ADAPTERS)))
LIBRARIES += build/librptadv_product.so.1
RUST_OUTPUT := $(abspath $(CARGO_TARGET_DIR))/release
TEST_ENV = LIBRARY_PATH="$(RUST_OUTPUT):$$LIBRARY_PATH" LD_LIBRARY_PATH="$(CURDIR)/build:$$LD_LIBRARY_PATH"
MANUALS := README.md QUALITY.md AGENTS.md WISHLIST.md COPYING $(wildcard doc/*.md doc/architecture/*.md doc/architecture/decisions/*.md)
DIST_FILES := Makefile COPYING AGENTS.md Doxyfile .clang-format .gitignore Cargo.toml Cargo.lock rust-toolchain.toml rust $(LOADER) $(wildcard tests/*.py) tests/radio_fixture.c tests/test_loader.c examples doc debian README.md QUALITY.md WISHLIST.md

.PHONY: all rust-build artifacts quality lint static-analysis docs dependency-boundary product-surface rust-quality rust-check rust-coverage loader-check loader-coverage check coverage install install-check integration dist distcheck platform-verify ci clean
# The small loader must reflect directory overrides even when Rust DSOs are unchanged.
.PHONY: build/app_rpt_advanced.so
all: build/app_rpt_advanced.so

build:
	mkdir -p $@

# Cargo owns incremental dependency tracking; Make never enumerates Rust source files.
rust-build: | build
	$(CARGO) build --locked --release --workspace
	@set -e; for name in $(ADAPTERS); do \
		install -p -m 0755 "$(RUST_OUTPUT)/librptadv_$${name}_adapter.so" "build/librptadv_$${name}_adapter.so.1"; \
	done
	install -p -m 0755 "$(RUST_OUTPUT)/librptadv_product.so" "build/librptadv_product.so.1"

$(LIBRARIES): rust-build
	@test -f $@

build/app_rpt_advanced.so: $(LOADER) $(HEADERS) $(LIBRARIES)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(MODULE_FLAGS) -fPIC -shared $< $(LDFLAGS) \
		-Lbuild -Wl,--enable-new-dtags,-rpath,'$(LOADER_RUNPATH)' \
		$(addprefix -l:,$(notdir $(LIBRARIES))) -o $@

artifacts: all
	python3 tests/test_rust_product_surface.py --artifacts build --asteriskmoddir "$(asteriskmoddir)" --libdir "$(libdir)"
	python3 tests/test_rust_product_surface.py --package debian/control

product-surface:
	python3 tests/test_rust_product_surface.py

dependency-boundary:
	python3 tests/test_asterisk_independence.py

lint:
	$(CARGO) fmt --all -- --check
	clang-format --dry-run --Werror $(LOADER) $(HEADERS) tests/radio_fixture.c tests/test_loader.c
	ruff check tests/*.py
	ruff format --check tests/*.py

static-analysis: dependency-boundary product-surface
	# AST_MODULE_INFO is external declaration machinery, not a callable API.
	cppcheck --check-level=exhaustive --enable=warning,style,performance,portability --error-exitcode=1 --std=c11 $(CPPFLAGS) '-DAST_MODULE_INFO(key,flags,description,...)=;' $(LOADER)
	clang-tidy $(LOADER) --warnings-as-errors='*' -- $(CPPFLAGS) $(MODULE_FLAGS) -fblocks
	$(TEST_ENV) $(CARGO) clippy --locked --workspace --all-targets -- -D warnings

rust-quality:
	$(CARGO) fmt --all -- --check
	$(TEST_ENV) $(CARGO) clippy --locked --workspace --all-targets -- -D warnings
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --locked --workspace --no-deps

docs: | build
	doxygen Doxyfile
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --locked --workspace --no-deps
	python3 tests/test_rust_product_surface.py --rustdoc $(CARGO_TARGET_DIR)/doc

quality: lint static-analysis docs

check: rust-build loader-check
	$(TEST_ENV) $(CARGO) test --locked --workspace

rust-check: rust-quality check

# Missing branch instrumentation is a failure, never an implicit coverage pass.
COVERAGE_TOOLCHAIN ?= nightly-2025-02-20
rust-coverage: | build
	mkdir -p build/coverage
	+@set -e; export RUSTUP_TOOLCHAIN=$(COVERAGE_TOOLCHAIN); \
		export CARGO_TARGET_DIR="$(abspath $(CARGO_TARGET_DIR))/llvm-cov-target"; \
		eval "$$($(CARGO) llvm-cov show-env --branch --export-prefix)"; \
		$(CARGO) llvm-cov clean --workspace; \
		$(MAKE) CARGO_TARGET_DIR="$$CARGO_TARGET_DIR" all; \
		export LIBRARY_PATH="$$CARGO_TARGET_DIR/release:$$LIBRARY_PATH"; \
		export LD_LIBRARY_PATH="$(CURDIR)/build:$$LD_LIBRARY_PATH"; \
		$(CARGO) test --locked --release --workspace --all-targets; \
		$(MAKE) -o rust-build CARGO_TARGET_DIR="$$CARGO_TARGET_DIR" integration; \
		$(CARGO) llvm-cov report --release \
			--ignore-filename-regex '(/tests[/.]|_tests\.rs$$|/fixture\.rs$$|/build\.rs$$)' \
			--json --output-path build/coverage/rust.json
	python3 tests/test_rust_product_surface.py --coverage build/coverage/rust.json

build/loader-coverage.o: $(LOADER) $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(MODULE_FLAGS) -O0 -g --coverage -c $< -o $@

build/test_loader.o: tests/test_loader.c $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(MODULE_FLAGS) -O0 -g -c $< -o $@

build/test_loader: build/test_loader.o build/loader-coverage.o
	$(CC) $^ --coverage -o $@

loader-check: build/test_loader
	./build/test_loader

loader-coverage: loader-check
	mkdir -p build/coverage
	gcovr --root . build --filter 'module/app_rpt_advanced_loader\.c$$' \
		--fail-under-line 100 --fail-under-branch 100 --xml-pretty \
		-o build/coverage/loader.xml --print-summary

coverage: rust-coverage loader-coverage

install: all
	install -d $(DESTDIR)$(asteriskmoddir) $(DESTDIR)$(libdir) $(DESTDIR)$(prefix)/include
	install -m 0755 build/app_rpt_advanced.so $(DESTDIR)$(asteriskmoddir)/
	install -m 0755 $(LIBRARIES) $(DESTDIR)$(libdir)/
	ln -sfn librptadv_product.so.1 $(DESTDIR)$(libdir)/librptadv_product.so
	ln -sfn librptadv_file_adapter.so.1 $(DESTDIR)$(libdir)/librptadv_file_adapter.so
	ln -sfn librptadv_speech_adapter.so.1 $(DESTDIR)$(libdir)/librptadv_speech_adapter.so
	ln -sfn librptadv_control_asterisk_adapter.so.1 $(DESTDIR)$(libdir)/librptadv_control_asterisk_adapter.so
	install -m 0644 rust/product/include/rptadv_product.h rust/control-asterisk-adapter/include/rptadv_control_asterisk_adapter.h $(DESTDIR)$(prefix)/include/
	install -D -m 0644 rust/file-adapter/include/rptadv_file_adapter.h $(DESTDIR)$(prefix)/include/rpt_advanced/file/rptadv_file_adapter.h
	install -D -m 0644 rust/media-support/include/rptadv_media_types.h $(DESTDIR)$(prefix)/include/rpt_advanced/file/rptadv_media_types.h
	install -D -m 0644 rust/speech-adapter/include/rptadv_speech_adapter.h $(DESTDIR)$(prefix)/include/rpt_advanced/speech/rptadv_speech_adapter.h
	install -D -m 0644 rust/media-support/include/rptadv_media_types.h $(DESTDIR)$(prefix)/include/rpt_advanced/speech/rptadv_media_types.h
	@set -e; for file in $(MANUALS); do \
		install -D -m 0644 "$$file" "$(DESTDIR)$(docdir)/$$file"; \
	done
	install -D -m 0644 COPYING $(DESTDIR)$(docdir)/copyright
	install -D -m 0644 examples/rpt_advanced.conf $(DESTDIR)$(docdir)/examples/rpt_advanced.conf

install-check: artifacts
	rm -rf -- $(CURDIR)/build/stage
	$(MAKE) -o rust-build DESTDIR=$(CURDIR)/build/stage prefix=/usr install
	python3 tests/test_rust_product_surface.py --stage build/stage --multiarch $(multiarch) --asteriskmoddir "$(asteriskmoddir)"

build/chan_rpt_fixture.so: tests/radio_fixture.c | build
	$(CC) $(CFLAGS) $(MODULE_FLAGS) -fPIC -shared $< $(LDFLAGS) -pthread -lm -o $@

integration: install-check build/chan_rpt_fixture.so
	install -m 0755 build/chan_rpt_fixture.so build/stage$(asteriskmoddir)/
	RPT_TEST_MODULE_DIR="$(CURDIR)/build/stage$(asteriskmoddir)" python3 tests/test_rust_asterisk_lifecycle.py
	RPT_TEST_MODULE_DIR="$(CURDIR)/build/stage$(asteriskmoddir)" python3 tests/test_asterisk_integration.py
	RPT_TEST_MODULE_DIR="$(CURDIR)/build/stage$(asteriskmoddir)" python3 tests/test_link_integration.py

dist: | build
	tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
		--exclude='__pycache__' --exclude='*.pyc' \
		--exclude='*.profraw' --exclude='*.profdata' \
		--transform='s,^,$(DIST_NAME)/,' -czf build/$(DIST_NAME).tar.gz $(DIST_FILES)

distcheck: dist
	+@set -e; stage=$$(mktemp -d "$(CURDIR)/build/dist-check.XXXXXX"); \
		trap 'rm -rf -- "$$stage"' EXIT HUP INT TERM; \
		tar -xzf build/$(DIST_NAME).tar.gz -C "$$stage"; \
		$(MAKE) -C "$$stage/$(DIST_NAME)" CARGO_TARGET_DIR="$(abspath $(CARGO_TARGET_DIR))" all install-check product-surface

platform-verify: check install-check integration distcheck

ci: quality platform-verify
	@if [ "$(multiarch)" = x86_64-linux-gnu ]; then $(MAKE) coverage; fi

clean:
	rm -rf -- $(CURDIR)/build
	$(CARGO) clean
