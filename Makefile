.DEFAULT_GOAL := all
CC ?= cc
AR ?= ar
CPPFLAGS += -Isrc
CFLAGS ?= -O2 -g
WARNINGS := -std=c11 -Wall -Wextra -Wpedantic -Werror
SOURCES := $(wildcard src/*.c)
MODULE_SOURCE := module/app_rpt_advanced.c
MEDIA_SOURCE := module/media.c
MODULE_FLAGS := -std=gnu11 -D_GNU_SOURCE -DAST_MODULE_SELF_SYM=ra_module_self -Wall -Wextra -Werror
HEADERS := $(wildcard src/*.h)
TESTS := $(wildcard tests/test_*.c)
OBJECTS := $(patsubst src/%.c,build/%.o,$(SOURCES))
COVERAGE_OBJECTS := $(patsubst src/%.c,build/coverage-objects/%.o,$(SOURCES))
.SECONDARY: $(COVERAGE_OBJECTS)
TEST_PROGRAMS := $(patsubst tests/%.c,build/%,$(TESTS))
prefix ?= /usr/local
asteriskmoddir ?= $(prefix)/lib/asterisk/modules
DESTDIR ?=

.PHONY: all quality lint static-analysis docs check coverage install install-check integration platform-verify ci clean
all: build/librpt_advanced.a build/app_rpt_advanced.so

build:
	mkdir -p $@

build/%.o: src/%.c $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) -fPIC -c $< -o $@

build/app_rpt_advanced.o: $(MODULE_SOURCE) $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(CFLAGS) $(MODULE_FLAGS) -fPIC -c $< -o $@

build/app_rpt_advanced.so: build/app_rpt_advanced.o $(OBJECTS)
	$(CC) -shared $^ -lm -o $@

build/librpt_advanced.a: $(OBJECTS)
	$(AR) rcs $@ $^

quality: lint static-analysis docs

lint:
	clang-format --dry-run --Werror $(SOURCES) $(MODULE_SOURCE) $(MEDIA_SOURCE) module/media.h $(HEADERS) $(TESTS)
	ruff check tests/*.py
	ruff format --check tests/*.py

static-analysis:
	cppcheck --check-level=exhaustive --enable=warning,style,performance,portability --error-exitcode=1 --std=c11 -Isrc $(SOURCES) $(MODULE_SOURCE) $(MEDIA_SOURCE)
	clang-tidy $(SOURCES) --warnings-as-errors='*' -- -Isrc -std=c11
	clang-tidy $(MODULE_SOURCE) $(MEDIA_SOURCE) --warnings-as-errors='*' -- -Isrc $(MODULE_FLAGS) -fblocks

docs: | build
	doxygen Doxyfile

build/coverage-objects:
	mkdir -p $@

build/coverage-objects/%.o: src/%.c $(HEADERS) | build/coverage-objects
	$(CC) $(CPPFLAGS) $(WARNINGS) -O0 -g --coverage -fPIC -c $< -o $@

build/module-coverage:
	mkdir -p $@

build/module-coverage/app_rpt_advanced.o: $(MODULE_SOURCE) $(HEADERS) | build/module-coverage
	$(CC) $(CPPFLAGS) $(MODULE_FLAGS) -O0 -g --coverage -fPIC -c $< -o $@

build/module-coverage/app_rpt_advanced.so: build/module-coverage/app_rpt_advanced.o $(COVERAGE_OBJECTS)
	$(CC) --coverage -shared $^ -lm -o $@

build/test_asterisk_module: tests/test_asterisk_module.c build/module-coverage/app_rpt_advanced.so | build
	$(CC) $(MODULE_FLAGS) -DASTMM_LIBC=ASTMM_IGNORE $< -Wl,--export-dynamic -ldl -o $@

build/module-coverage/media.o: $(MEDIA_SOURCE) module/media.h | build/module-coverage
	$(CC) $(MODULE_FLAGS) -O0 -g --coverage -fPIC -c $< -o $@

build/test_asterisk_media: tests/test_asterisk_media.c build/module-coverage/media.o module/media.h | build
	$(CC) $(MODULE_FLAGS) -Imodule $< build/module-coverage/media.o --coverage -o $@

build/test_%: tests/test_%.c $(COVERAGE_OBJECTS) $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(WARNINGS) -O0 -g --coverage $< $(COVERAGE_OBJECTS) -lm -o $@

build/test_document: tests/test_document.c $(COVERAGE_OBJECTS) $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(WARNINGS) -O0 -g --coverage $< $(COVERAGE_OBJECTS) -Wl,--wrap=strdup,--wrap=reallocarray -lm -o $@

check: $(TEST_PROGRAMS)
	find build -name '*.gcda' -delete
	@set -e; for test in $(TEST_PROGRAMS); do ./$$test; done

coverage: check
	mkdir -p build/coverage
	gcovr --root . --filter 'src/|module/' --fail-under-line 100 --fail-under-branch 100 --xml-pretty -o build/coverage/coverage.xml --print-summary

install: all
	install -d $(DESTDIR)$(asteriskmoddir)
	install -m 0755 build/app_rpt_advanced.so $(DESTDIR)$(asteriskmoddir)/
	install -d $(DESTDIR)$(prefix)/lib $(DESTDIR)$(prefix)/include/rpt_advanced
	install -d $(DESTDIR)$(prefix)/share/doc/rpt_advanced
	install -m 0644 COPYING $(DESTDIR)$(prefix)/share/doc/rpt_advanced/copyright
	install -d $(DESTDIR)$(prefix)/share/doc/rpt_advanced/examples
	install -m 0644 examples/rpt_advanced.conf $(DESTDIR)$(prefix)/share/doc/rpt_advanced/examples/
	install -m 0644 build/librpt_advanced.a $(DESTDIR)$(prefix)/lib/
	install -m 0644 $(HEADERS) $(DESTDIR)$(prefix)/include/rpt_advanced/

install-check: all
	$(MAKE) DESTDIR=$(CURDIR)/build/stage prefix=/usr install
	cmp build/librpt_advanced.a build/stage/usr/lib/librpt_advanced.a
	cmp src/identifier.h build/stage/usr/include/rpt_advanced/identifier.h
	cmp src/duplex.h build/stage/usr/include/rpt_advanced/duplex.h
	cmp src/morse.h build/stage/usr/include/rpt_advanced/morse.h
	cmp src/config.h build/stage/usr/include/rpt_advanced/config.h
	cmp src/settings.h build/stage/usr/include/rpt_advanced/settings.h
	cmp src/config_reader.h build/stage/usr/include/rpt_advanced/config_reader.h
	cmp src/document.h build/stage/usr/include/rpt_advanced/document.h
	cmp src/schema.h build/stage/usr/include/rpt_advanced/schema.h
	cmp COPYING build/stage/usr/share/doc/rpt_advanced/copyright
	cmp examples/rpt_advanced.conf build/stage/usr/share/doc/rpt_advanced/examples/rpt_advanced.conf
	cmp build/app_rpt_advanced.so build/stage/usr/lib/asterisk/modules/app_rpt_advanced.so

integration: install-check
	python3 tests/test_asterisk_integration.py

platform-verify: all coverage install-check integration

ci: quality platform-verify

clean:
	rm -rf build
